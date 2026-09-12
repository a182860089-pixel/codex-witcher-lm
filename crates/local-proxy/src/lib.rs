//! Loopback-only Responses API proxy for the focused Codex provider switcher.
//!
//! The proxy keeps entry and upstream bearer credentials in memory, atomically
//! swaps immutable routes, and pins a route for a conversation `thread_id` so
//! later model switches do not rewrite in-flight or already-started threads.
//! It is intentionally a Rust API rather than a command-line program so Tauri
//! can obtain secrets from the platform credential store before constructing a
//! route.

#![cfg_attr(not(test), forbid(unsafe_code))]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::header::{
    ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HOST, IF_NONE_MATCH,
    RETRY_AFTER, USER_AGENT,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::{Host, Url};
use zeroize::Zeroizing;

const DEFAULT_MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_PINNED_TURNS: usize = 4_096;
const DEFAULT_MAX_PINNED_THREADS: usize = 4_096;
const BINDINGS_SCHEMA_VERSION: u32 = 1;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_UPSTREAM_MAX_RETRIES: usize = 8;
const DEFAULT_UPSTREAM_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_UPSTREAM_RETRY_MAX_DELAY: Duration = Duration::from_secs(8);
const DEFAULT_UPSTREAM_RETRY_MAX_ELAPSED: Duration = Duration::from_secs(90);
const UPSTREAM_TCP_KEEPALIVE: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_ERROR_MESSAGE_BYTES: usize = 300;
const UPSTREAM_USER_AGENT: HeaderValue = HeaderValue::from_static("codex-provider-switcher/0.3.4");
const MAX_ROUTE_ID_BYTES: usize = 256;
const MAX_MODEL_ID_BYTES: usize = 256;
const MAX_TURN_KEY_BYTES: usize = 512;
const X_MODELS_ETAG: HeaderName = HeaderName::from_static("x-models-etag");
const BASE_INSTRUCTIONS: &str = "You are a coding agent working with the user in the current repository. Follow developer and user instructions, inspect relevant context before editing, keep changes scoped, use the available tools carefully, and verify completed work.";

/// A bearer credential whose debug representation never contains its value.
///
/// The value is zeroized when its last owner is dropped. The type deliberately
/// has no getter; callers can only pass it into [`RouteConfig`] or
/// [`LocalProxy::start`].
#[derive(Clone)]
pub struct BearerToken(Zeroizing<String>);

impl BearerToken {
    pub fn new(value: impl Into<String>) -> Result<Self, ProxyError> {
        let value = value.into();
        if value.is_empty()
            || !value.is_ascii()
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b' ')
        {
            return Err(ProxyError::InvalidBearerToken);
        }
        Ok(Self(Zeroizing::new(value)))
    }

    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    fn authorization_header(&self) -> Result<HeaderValue, ProxyError> {
        let mut rendered = Zeroizing::new(String::with_capacity(self.0.len() + 7));
        rendered.push_str("Bearer ");
        rendered.push_str(&self.0);
        let mut header =
            HeaderValue::from_str(&rendered).map_err(|_| ProxyError::InvalidBearerToken)?;
        header.set_sensitive(true);
        Ok(header)
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken([REDACTED])")
    }
}

/// A reasoning option advertised through the Codex-compatible `/models` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReasoningLevelDescriptor {
    pub effort: String,
    pub description: String,
}

/// User-selected model metadata stored on an immutable proxy route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelDescriptor {
    pub slug: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub context_window: Option<i64>,
    #[serde(default)]
    pub max_context_window: Option<i64>,
    #[serde(default)]
    pub default_reasoning_level: Option<String>,
    #[serde(default)]
    pub supported_reasoning_levels: Vec<ReasoningLevelDescriptor>,
    #[serde(default = "default_true")]
    pub supports_parallel_tool_calls: bool,
    #[serde(default)]
    pub supports_images: bool,
}

impl ModelDescriptor {
    pub fn new(slug: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            display_name: display_name.into(),
            description: None,
            context_window: None,
            max_context_window: None,
            default_reasoning_level: None,
            supported_reasoning_levels: Vec::new(),
            supports_parallel_tool_calls: true,
            supports_images: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Public, credential-free description of an active route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteSummary {
    pub id: String,
    pub selected_model: String,
    pub model_count: usize,
}

/// Credential-free binding of a Codex conversation to a saved route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadBinding {
    pub thread_id: String,
    pub route_id: String,
    pub selected_model: String,
}

/// Credential-free record of a recently used provider/model route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RouteBinding {
    pub route_id: String,
    pub selected_model: String,
}

/// Credential-free binding of a Responses conversation/response id to a route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationBinding {
    pub conversation_id: String,
    pub route_id: String,
    pub selected_model: String,
}

/// On-disk conversation and recent-route bindings that survive app upgrades.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyBindings {
    pub schema_version: u32,
    #[serde(default)]
    pub threads: Vec<ThreadBinding>,
    #[serde(default)]
    pub recent: Vec<RouteBinding>,
    #[serde(default)]
    pub conversations: Vec<ConversationBinding>,
}

/// Read credential-free proxy bindings from disk. Invalid files are ignored.
pub fn read_proxy_bindings(path: &Path) -> Option<ProxyBindings> {
    let bytes = std::fs::read(path).ok()?;
    let parsed: ProxyBindings = serde_json::from_slice(&bytes).ok()?;
    (parsed.schema_version == BINDINGS_SCHEMA_VERSION).then_some(parsed)
}

/// One Codex-compatible reasoning preset in [`CodexModelInfo`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexReasoningLevel {
    pub effort: String,
    pub description: String,
}

/// Model shape accepted by Codex's remote model catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexModelInfo {
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_reasoning_level: Option<String>,
    pub supported_reasoning_levels: Vec<CodexReasoningLevel>,
    pub shell_type: String,
    pub visibility: String,
    pub supported_in_api: bool,
    pub priority: i32,
    pub availability_nux: Option<Value>,
    pub upgrade: Option<Value>,
    pub base_instructions: String,
    pub include_skills_usage_instructions: bool,
    pub supports_reasoning_summary_parameter: bool,
    pub default_reasoning_summary: String,
    pub support_verbosity: bool,
    pub default_verbosity: Option<String>,
    pub apply_patch_tool_type: Option<String>,
    pub web_search_tool_type: String,
    pub truncation_policy: CodexTruncationPolicy,
    pub supports_parallel_tool_calls: bool,
    pub supports_image_detail_original: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<i64>,
    pub auto_compact_token_limit: Option<i64>,
    pub effective_context_window_percent: i64,
    pub experimental_supported_tools: Vec<String>,
    pub input_modalities: Vec<String>,
    pub supports_search_tool: bool,
    pub use_responses_lite: bool,
    pub auto_review_model_override: Option<String>,
}

/// Truncation policy required by Codex's remote model catalog schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexTruncationPolicy {
    pub mode: String,
    pub limit: i64,
}

/// Response wrapper served from both `/models` and `/v1/models`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexModelsResponse {
    pub models: Vec<CodexModelInfo>,
}

/// Immutable provider/model route. Its bearer key is never serializable.
#[derive(Clone)]
pub struct RouteConfig {
    id: String,
    upstream_base_url: Url,
    selected_model: String,
    models: Vec<ModelDescriptor>,
    upstream_bearer: BearerToken,
}

impl RouteConfig {
    pub fn new(
        id: impl Into<String>,
        upstream_base_url: impl AsRef<str>,
        selected_model: impl Into<String>,
        models: Vec<ModelDescriptor>,
        upstream_bearer: BearerToken,
    ) -> Result<Self, ProxyError> {
        let id = id.into();
        let selected_model = selected_model.into();
        validate_identifier("route id", &id, MAX_ROUTE_ID_BYTES)?;
        validate_identifier("selected model", &selected_model, MAX_MODEL_ID_BYTES)?;

        if models.is_empty() {
            return Err(ProxyError::InvalidRoute(
                "a route must advertise at least one model".to_string(),
            ));
        }

        let mut slugs = HashSet::with_capacity(models.len());
        for model in &models {
            validate_identifier("model slug", &model.slug, MAX_MODEL_ID_BYTES)?;
            if model.display_name.trim().is_empty() {
                return Err(ProxyError::InvalidRoute(
                    "model display name cannot be empty".to_string(),
                ));
            }
            if !slugs.insert(model.slug.as_str()) {
                return Err(ProxyError::InvalidRoute(
                    "model slugs must be unique within a route".to_string(),
                ));
            }
            if model.context_window.is_some_and(|value| value <= 0)
                || model.max_context_window.is_some_and(|value| value <= 0)
            {
                return Err(ProxyError::InvalidRoute(
                    "model context windows must be positive".to_string(),
                ));
            }
        }
        if !slugs.contains(selected_model.as_str()) {
            return Err(ProxyError::InvalidRoute(
                "the selected model must be present in the advertised model list".to_string(),
            ));
        }

        Ok(Self {
            id,
            upstream_base_url: normalize_upstream_base_url(upstream_base_url.as_ref())?,
            selected_model,
            models,
            upstream_bearer,
        })
    }

    pub fn single_model(
        id: impl Into<String>,
        upstream_base_url: impl AsRef<str>,
        model: ModelDescriptor,
        upstream_bearer: BearerToken,
    ) -> Result<Self, ProxyError> {
        let selected_model = model.slug.clone();
        Self::new(
            id,
            upstream_base_url,
            selected_model,
            vec![model],
            upstream_bearer,
        )
    }

    pub fn summary(&self) -> RouteSummary {
        RouteSummary {
            id: self.id.clone(),
            selected_model: self.selected_model.clone(),
            model_count: self.models.len(),
        }
    }

    pub fn models_response(&self) -> CodexModelsResponse {
        let models = self
            .models
            .iter()
            .enumerate()
            .map(|(index, model)| CodexModelInfo {
                slug: model.slug.clone(),
                display_name: model.display_name.clone(),
                description: model.description.clone(),
                default_reasoning_level: model.default_reasoning_level.clone(),
                supported_reasoning_levels: model
                    .supported_reasoning_levels
                    .iter()
                    .map(|level| CodexReasoningLevel {
                        effort: level.effort.clone(),
                        description: level.description.clone(),
                    })
                    .collect(),
                shell_type: "shell_command".to_string(),
                visibility: "list".to_string(),
                supported_in_api: true,
                priority: index as i32 + 1,
                availability_nux: None,
                upgrade: None,
                base_instructions: BASE_INSTRUCTIONS.to_string(),
                include_skills_usage_instructions: true,
                supports_reasoning_summary_parameter: false,
                default_reasoning_summary: "auto".to_string(),
                support_verbosity: false,
                default_verbosity: None,
                apply_patch_tool_type: Some("freeform".to_string()),
                web_search_tool_type: "text".to_string(),
                truncation_policy: CodexTruncationPolicy {
                    mode: "tokens".to_string(),
                    limit: 10_000,
                },
                supports_parallel_tool_calls: model.supports_parallel_tool_calls,
                supports_image_detail_original: model.supports_images,
                context_window: model.context_window,
                max_context_window: model.max_context_window.or(model.context_window),
                auto_compact_token_limit: None,
                effective_context_window_percent: 95,
                experimental_supported_tools: Vec::new(),
                input_modalities: if model.supports_images {
                    vec!["text".to_string(), "image".to_string()]
                } else {
                    vec!["text".to_string()]
                },
                supports_search_tool: false,
                use_responses_lite: false,
                auto_review_model_override: None,
            })
            .collect();
        CodexModelsResponse { models }
    }

    fn endpoint_url(&self, endpoint: UpstreamEndpoint) -> Url {
        self.upstream_base_url
            .join(endpoint.relative_path())
            .expect("a validated hierarchical base URL always joins a static path")
    }

    fn contains_model(&self, model: &str) -> bool {
        self.models.iter().any(|item| item.slug == model)
    }

    fn with_selected_model(&self, model: &str) -> Option<Self> {
        if !self.contains_model(model) {
            return None;
        }
        let mut next = self.clone();
        next.selected_model = model.to_string();
        Some(next)
    }
}

fn catalog_etag_for(catalog: &CodexModelsResponse) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_vec(catalog)
            .expect("Codex models response only contains serializable values"),
    );
    format!("\"{}\"", encode_hex(&hasher.finalize()))
}

impl fmt::Debug for RouteConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouteConfig")
            .field("id", &self.id)
            .field("upstream_base_url", &self.upstream_base_url)
            .field("selected_model", &self.selected_model)
            .field("models", &self.models)
            .field("upstream_bearer", &"[REDACTED]")
            .finish()
    }
}

/// Loopback listener and memory-bound settings.
#[derive(Debug, Clone)]
pub struct ProxyStartOptions {
    /// TCP port on `127.0.0.1`; use zero for an operating-system assigned port.
    pub port: u16,
    pub max_request_bytes: usize,
    pub max_pinned_turns: usize,
    pub max_pinned_threads: usize,
    pub upstream_connect_timeout: Duration,
    /// Number of retries for failures that happen before any successful
    /// response body has been received. This is deliberately finite.
    pub upstream_max_retries: usize,
    pub upstream_retry_base_delay: Duration,
    pub upstream_retry_max_delay: Duration,
    pub upstream_retry_max_elapsed: Duration,
    /// When false (crate default), the upstream client ignores `HTTP_PROXY`.
    pub use_system_proxy: bool,
    /// Optional credential-free JSON file used to keep conversation routes
    /// across Switcher restarts and in-place upgrades.
    pub bindings_path: Option<PathBuf>,
}

impl Default for ProxyStartOptions {
    fn default() -> Self {
        Self {
            port: 0,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_pinned_turns: DEFAULT_MAX_PINNED_TURNS,
            max_pinned_threads: DEFAULT_MAX_PINNED_THREADS,
            upstream_connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            upstream_max_retries: DEFAULT_UPSTREAM_MAX_RETRIES,
            upstream_retry_base_delay: DEFAULT_UPSTREAM_RETRY_BASE_DELAY,
            upstream_retry_max_delay: DEFAULT_UPSTREAM_RETRY_MAX_DELAY,
            upstream_retry_max_elapsed: DEFAULT_UPSTREAM_RETRY_MAX_ELAPSED,
            use_system_proxy: false,
            bindings_path: None,
        }
    }
}

/// Credential-free runtime status suitable for a Tauri command response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyHealth {
    pub running: bool,
    pub listen_addr: SocketAddr,
    pub active_route: Option<RouteSummary>,
    pub pinned_turns: usize,
    pub pinned_threads: usize,
    pub forwarded_requests: u64,
    pub in_flight_requests: usize,
    pub last_upstream_status: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRequestLog {
    pub id: String,
    pub time: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub status: u16,
    pub duration_ms: u64,
    pub thread_id: Option<String>,
    pub error: Option<String>,
    pub details: Option<String>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub first_byte_ms: Option<u64>,
    #[serde(default)]
    pub response_bytes: u64,
    #[serde(default)]
    pub stream_duration_ms: Option<u64>,
    #[serde(default)]
    pub stream_completed: bool,
    #[serde(default)]
    pub stream_error: Option<String>,
}

/// Starts proxy instances for Tauri without exposing a command-line surface.
#[derive(Debug)]
pub struct LocalProxy;

impl LocalProxy {
    pub async fn start(
        options: ProxyStartOptions,
        entry_bearer: BearerToken,
    ) -> Result<ProxyHandle, ProxyError> {
        if options.max_request_bytes == 0 {
            return Err(ProxyError::InvalidStartOptions(
                "max_request_bytes must be greater than zero".to_string(),
            ));
        }
        if options.max_pinned_turns == 0 {
            return Err(ProxyError::InvalidStartOptions(
                "max_pinned_turns must be greater than zero".to_string(),
            ));
        }
        if options.max_pinned_threads == 0 {
            return Err(ProxyError::InvalidStartOptions(
                "max_pinned_threads must be greater than zero".to_string(),
            ));
        }
        if options.upstream_retry_base_delay.is_zero()
            || options.upstream_retry_max_delay.is_zero()
            || options.upstream_retry_max_elapsed.is_zero()
        {
            return Err(ProxyError::InvalidStartOptions(
                "upstream retry durations must be greater than zero".to_string(),
            ));
        }
        if options.upstream_retry_base_delay > options.upstream_retry_max_delay {
            return Err(ProxyError::InvalidStartOptions(
                "upstream_retry_base_delay must not exceed upstream_retry_max_delay".to_string(),
            ));
        }

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, options.port))
            .await
            .map_err(ProxyError::Bind)?;
        let listen_addr = listener.local_addr().map_err(ProxyError::Bind)?;
        if listen_addr.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err(ProxyError::NonLoopbackListener);
        }

        let mut client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(options.upstream_connect_timeout)
            .http1_only()
            .tcp_nodelay(true)
            .tcp_keepalive(Some(UPSTREAM_TCP_KEEPALIVE))
            .user_agent(UPSTREAM_USER_AGENT.clone());
        if options.use_system_proxy {
            if let Some(proxy) = system_proxy_skipping_loopback() {
                client = client.proxy(proxy);
            }
        } else {
            client = client.no_proxy();
        }
        let client = client.build().map_err(ProxyError::BuildClient)?;

        let state = Arc::new(ProxyState {
            auth: EntryTokenVerifier::new(entry_bearer),
            routes: RouteTable::from_options(&options),
            client,
            listen_addr,
            max_request_bytes: options.max_request_bytes,
            upstream_max_retries: options.upstream_max_retries,
            upstream_retry_base_delay: options.upstream_retry_base_delay,
            upstream_retry_max_delay: options.upstream_retry_max_delay,
            upstream_retry_max_elapsed: options.upstream_retry_max_elapsed,
            metrics: Arc::new(ProxyMetrics::default()),
            request_logs: Mutex::new(VecDeque::with_capacity(128)),
        });
        state.metrics.running.store(true, Ordering::Release);

        let app = Router::new()
            .route("/health", get(health_handler))
            .route("/models", get(models_handler))
            .route("/v1/models", get(models_handler))
            .route("/responses", post(responses_handler))
            .route("/v1/responses", post(responses_handler))
            .route("/responses/compact", post(compact_handler))
            .route("/v1/responses/compact", post(compact_handler))
            .with_state(Arc::clone(&state));

        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let running = Arc::clone(&state.metrics);
        let server_task = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_receiver.await;
                })
                .await;
            running.running.store(false, Ordering::Release);
            result
        });

        Ok(ProxyHandle {
            state,
            listen_addr,
            lifecycle: Arc::new(ProxyLifecycle {
                shutdown_sender: Mutex::new(Some(shutdown_sender)),
                server_task: Mutex::new(Some(server_task)),
            }),
        })
    }
}

/// Cloneable controller intended to be held in Tauri managed state.
#[derive(Clone)]
pub struct ProxyHandle {
    state: Arc<ProxyState>,
    listen_addr: SocketAddr,
    lifecycle: Arc<ProxyLifecycle>,
}

impl ProxyHandle {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.listen_addr)
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    pub fn set_active_route(&self, route: RouteConfig) -> Option<RouteSummary> {
        self.state
            .routes
            .set_active(route)
            .map(|route| route.summary())
    }

    pub fn remember_route(&self, route: RouteConfig) {
        self.state.routes.remember(Arc::new(route));
    }

    pub fn pin_thread(&self, thread_id: &str, route: RouteConfig) -> Result<(), ProxyError> {
        validate_turn_key_part("thread_id", thread_id)?;
        self.state.routes.pin_thread(thread_id, Arc::new(route));
        Ok(())
    }

    pub fn clear_active_route(&self) -> Option<RouteSummary> {
        self.state
            .routes
            .clear_active()
            .map(|route| route.summary())
    }

    pub fn active_route(&self) -> Option<RouteSummary> {
        self.state.routes.active().map(|route| route.summary())
    }

    pub fn models_response(&self) -> Option<CodexModelsResponse> {
        self.state.routes.catalog_response()
    }

    pub fn release_turn(&self, thread_id: &str, turn_id: &str) -> bool {
        self.state
            .routes
            .release(&TurnKey::new_unchecked(thread_id, turn_id))
    }

    pub fn clear_pinned_turns(&self) {
        self.state.routes.clear_pins();
    }

    pub fn health(&self) -> ProxyHealth {
        self.state.health(self.listen_addr)
    }

    pub fn request_logs(&self) -> Vec<ProxyRequestLog> {
        self.state.get_request_logs()
    }

    pub fn clear_request_logs(&self) {
        self.state.clear_request_logs();
    }

    pub async fn shutdown(&self) -> Result<(), ProxyError> {
        if let Some(sender) = lock_unpoisoned(&self.lifecycle.shutdown_sender).take() {
            let _ = sender.send(());
        }
        let task = lock_unpoisoned(&self.lifecycle.server_task).take();
        if let Some(task) = task {
            task.await
                .map_err(ProxyError::ServerTask)?
                .map_err(ProxyError::Server)?;
        }
        self.state.metrics.running.store(false, Ordering::Release);
        Ok(())
    }
}

impl fmt::Debug for ProxyHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxyHandle")
            .field("listen_addr", &self.listen_addr)
            .field("health", &self.health())
            .finish_non_exhaustive()
    }
}

struct ProxyLifecycle {
    shutdown_sender: Mutex<Option<oneshot::Sender<()>>>,
    server_task: Mutex<Option<JoinHandle<Result<(), io::Error>>>>,
}

struct ProxyState {
    auth: EntryTokenVerifier,
    routes: RouteTable,
    client: reqwest::Client,
    listen_addr: SocketAddr,
    max_request_bytes: usize,
    upstream_max_retries: usize,
    upstream_retry_base_delay: Duration,
    upstream_retry_max_delay: Duration,
    upstream_retry_max_elapsed: Duration,
    metrics: Arc<ProxyMetrics>,
    request_logs: Mutex<VecDeque<ProxyRequestLog>>,
}

impl ProxyState {
    fn record_request_log(&self, log: ProxyRequestLog) {
        if let Ok(mut logs) = self.request_logs.lock() {
            if logs.len() >= 100 {
                logs.pop_back();
            }
            logs.push_front(log);
        }
    }

    fn get_request_logs(&self) -> Vec<ProxyRequestLog> {
        if let Ok(logs) = self.request_logs.lock() {
            logs.iter().cloned().collect()
        } else {
            Vec::new()
        }
    }

    fn update_request_log<F>(&self, id: &str, update: F)
    where
        F: FnOnce(&mut ProxyRequestLog),
    {
        if let Ok(mut logs) = self.request_logs.lock()
            && let Some(log) = logs.iter_mut().find(|log| log.id == id)
        {
            update(log);
        }
    }

    fn clear_request_logs(&self) {
        if let Ok(mut logs) = self.request_logs.lock() {
            logs.clear();
        }
    }

    fn health(&self, listen_addr: SocketAddr) -> ProxyHealth {
        let last_status = self.metrics.last_upstream_status.load(Ordering::Acquire);
        ProxyHealth {
            running: self.metrics.running.load(Ordering::Acquire),
            listen_addr,
            active_route: self.routes.active().map(|route| route.summary()),
            pinned_turns: self.routes.pin_count(),
            pinned_threads: self.routes.thread_pin_count(),
            forwarded_requests: self.metrics.forwarded_requests.load(Ordering::Acquire),
            in_flight_requests: self.metrics.in_flight_requests.load(Ordering::Acquire),
            last_upstream_status: (last_status != 0).then_some(last_status),
        }
    }
}

#[derive(Default)]
struct ProxyMetrics {
    running: AtomicBool,
    forwarded_requests: AtomicU64,
    in_flight_requests: AtomicUsize,
    last_upstream_status: AtomicU16,
}

struct RequestLease {
    metrics: Arc<ProxyMetrics>,
}

impl RequestLease {
    fn begin(metrics: Arc<ProxyMetrics>) -> Self {
        metrics.forwarded_requests.fetch_add(1, Ordering::AcqRel);
        metrics.in_flight_requests.fetch_add(1, Ordering::AcqRel);
        Self { metrics }
    }
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        self.metrics
            .in_flight_requests
            .fetch_sub(1, Ordering::AcqRel);
    }
}

struct EntryTokenVerifier {
    expected_digest: [u8; 32],
}

impl EntryTokenVerifier {
    fn new(token: BearerToken) -> Self {
        let digest = Sha256::digest(token.as_bytes());
        let mut expected_digest = [0_u8; 32];
        expected_digest.copy_from_slice(&digest);
        Self { expected_digest }
    }

    fn authorize(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }

        let bytes = value.as_bytes();
        if bytes.len() <= 7 || !bytes[..7].eq_ignore_ascii_case(b"Bearer ") {
            return false;
        }
        let candidate_digest = Sha256::digest(&bytes[7..]);
        bool::from(
            self.expected_digest
                .as_slice()
                .ct_eq(candidate_digest.as_slice()),
        )
    }
}

struct RouteTable {
    active: ArcSwapOption<RouteConfig>,
    recent: Mutex<LruRoutes>,
    thread_pins: Mutex<LruThreads>,
    conversation_pins: Mutex<LruThreads>,
    pins: Mutex<PinnedRoutes>,
    pending_threads: Mutex<Vec<ThreadBinding>>,
    pending_conversations: Mutex<Vec<ConversationBinding>>,
    pending_recent: Mutex<Vec<RouteBinding>>,
    max_pinned_turns: usize,
    max_pinned_threads: usize,
    bindings_path: Option<PathBuf>,
}

impl RouteTable {
    fn new(max_pinned_turns: usize) -> Self {
        Self::with_options(max_pinned_turns, max_pinned_turns, None)
    }

    fn from_options(options: &ProxyStartOptions) -> Self {
        Self::with_options(
            options.max_pinned_turns,
            options.max_pinned_threads,
            options.bindings_path.clone(),
        )
    }

    fn with_options(
        max_pinned_turns: usize,
        max_pinned_threads: usize,
        bindings_path: Option<PathBuf>,
    ) -> Self {
        let bindings = bindings_path
            .as_ref()
            .and_then(|path| read_proxy_bindings(path));
        let (pending_threads, pending_recent, pending_conversations) = match bindings {
            Some(bindings) => (bindings.threads, bindings.recent, bindings.conversations),
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        Self {
            active: ArcSwapOption::empty(),
            recent: Mutex::new(LruRoutes::default()),
            thread_pins: Mutex::new(LruThreads::default()),
            conversation_pins: Mutex::new(LruThreads::default()),
            pins: Mutex::new(PinnedRoutes::default()),
            pending_threads: Mutex::new(pending_threads),
            pending_conversations: Mutex::new(pending_conversations),
            pending_recent: Mutex::new(pending_recent),
            max_pinned_turns,
            max_pinned_threads,
            bindings_path,
        }
    }

    fn active(&self) -> Option<Arc<RouteConfig>> {
        self.active.load_full()
    }

    fn set_active(&self, route: RouteConfig) -> Option<Arc<RouteConfig>> {
        let next = Arc::new(route);
        self.remember(Arc::clone(&next));
        let previous = self.active.swap(Some(Arc::clone(&next)));
        if let Some(previous) = &previous {
            self.remember(Arc::clone(previous));
        }
        self.persist();
        previous
    }

    fn remember(&self, route: Arc<RouteConfig>) {
        lock_unpoisoned(&self.recent).insert(Arc::clone(&route), self.max_pinned_threads);
        self.hydrate_pending(route);
        self.persist();
    }

    fn hydrate_pending(&self, route: Arc<RouteConfig>) {
        lock_unpoisoned(&self.pending_recent).retain(|binding| {
            binding.route_id != route.id || binding.selected_model != route.selected_model
        });
        let matched: Vec<String> = {
            let mut pending = lock_unpoisoned(&self.pending_threads);
            let mut matched = Vec::new();
            pending.retain(|binding| {
                if binding.route_id == route.id && binding.selected_model == route.selected_model {
                    matched.push(binding.thread_id.clone());
                    false
                } else {
                    true
                }
            });
            matched
        };
        if !matched.is_empty() {
            let mut pins = lock_unpoisoned(&self.thread_pins);
            for thread_id in matched {
                pins.insert(thread_id, Arc::clone(&route), self.max_pinned_threads);
            }
        }
        let matched_conversations: Vec<String> = {
            let mut pending = lock_unpoisoned(&self.pending_conversations);
            let mut matched = Vec::new();
            pending.retain(|binding| {
                if binding.route_id == route.id && binding.selected_model == route.selected_model {
                    matched.push(binding.conversation_id.clone());
                    false
                } else {
                    true
                }
            });
            matched
        };
        if matched_conversations.is_empty() {
            return;
        }
        let mut pins = lock_unpoisoned(&self.conversation_pins);
        for conversation_id in matched_conversations {
            pins.insert(conversation_id, Arc::clone(&route), self.max_pinned_threads);
        }
    }

    fn pin_thread(&self, thread_id: &str, route: Arc<RouteConfig>) {
        lock_unpoisoned(&self.pending_threads).retain(|binding| binding.thread_id != thread_id);
        self.remember(Arc::clone(&route));
        lock_unpoisoned(&self.thread_pins).insert(
            thread_id.to_string(),
            route,
            self.max_pinned_threads,
        );
        self.persist();
    }

    fn pin_conversation(&self, conversation_id: &str, route: Arc<RouteConfig>) {
        if conversation_id.trim().is_empty() || conversation_id.len() > MAX_TURN_KEY_BYTES {
            return;
        }
        {
            let pins = lock_unpoisoned(&self.conversation_pins);
            if pins.routes.get(conversation_id).is_some_and(|existing| {
                existing.id == route.id && existing.selected_model == route.selected_model
            }) {
                return;
            }
        }
        lock_unpoisoned(&self.pending_conversations)
            .retain(|binding| binding.conversation_id != conversation_id);
        self.remember(Arc::clone(&route));
        lock_unpoisoned(&self.conversation_pins).insert(
            conversation_id.to_string(),
            route,
            self.max_pinned_threads,
        );
        self.persist();
    }

    fn clear_active(&self) -> Option<Arc<RouteConfig>> {
        self.active.swap(None)
    }

    fn resolve(
        &self,
        thread_id: Option<&str>,
        turn_key: Option<TurnKey>,
        requested_model: Option<&str>,
    ) -> Option<Arc<RouteConfig>> {
        self.resolve_with_conversations(thread_id, turn_key, requested_model, &[])
    }

    fn resolve_with_conversations(
        &self,
        thread_id: Option<&str>,
        turn_key: Option<TurnKey>,
        requested_model: Option<&str>,
        conversation_ids: &[String],
    ) -> Option<Arc<RouteConfig>> {
        let existing = self.lookup_existing(thread_id, turn_key.as_ref(), conversation_ids);
        let route = match existing {
            Some(route) => route,
            None => match requested_model.and_then(|model| self.route_for_model(model)) {
                Some(route) => route,
                None => self.active()?,
            },
        };
        if let Some(key) = turn_key {
            self.pin_turn(key, Arc::clone(&route));
        }
        Some(route)
    }

    fn lookup_existing(
        &self,
        thread_id: Option<&str>,
        turn_key: Option<&TurnKey>,
        conversation_ids: &[String],
    ) -> Option<Arc<RouteConfig>> {
        if let Some(key) = turn_key {
            let pins = lock_unpoisoned(&self.pins);
            if let Some(route) = pins.routes.get(key) {
                return Some(Arc::clone(route));
            }
        }
        if let Some(thread_id) = thread_id {
            let pins = lock_unpoisoned(&self.thread_pins);
            if let Some(route) = pins.routes.get(thread_id) {
                return Some(Arc::clone(route));
            }
        }
        let pins = lock_unpoisoned(&self.conversation_pins);
        for conversation_id in conversation_ids {
            if let Some(route) = pins.routes.get(conversation_id) {
                return Some(Arc::clone(route));
            }
        }
        None
    }

    fn route_for_model(&self, model: &str) -> Option<Arc<RouteConfig>> {
        let matches = |route: &RouteConfig| route.selected_model == model;
        for route in lock_unpoisoned(&self.thread_pins).routes.values() {
            if matches(route) {
                return Some(Arc::clone(route));
            }
        }
        for route in lock_unpoisoned(&self.conversation_pins).routes.values() {
            if matches(route) {
                return Some(Arc::clone(route));
            }
        }
        for route in lock_unpoisoned(&self.recent).routes.values() {
            if matches(route) {
                return Some(Arc::clone(route));
            }
        }
        self.active().filter(|route| matches(route))
    }

    fn pin_turn(&self, turn_key: TurnKey, route: Arc<RouteConfig>) {
        let mut pins = lock_unpoisoned(&self.pins);
        if pins.routes.contains_key(&turn_key) {
            return;
        }
        while pins.routes.len() >= self.max_pinned_turns {
            let Some(stale_key) = pins.insertion_order.pop_front() else {
                break;
            };
            pins.routes.remove(&stale_key);
        }
        pins.insertion_order.push_back(turn_key.clone());
        pins.routes.insert(turn_key, route);
    }

    fn release(&self, turn_key: &TurnKey) -> bool {
        lock_unpoisoned(&self.pins)
            .routes
            .remove(turn_key)
            .is_some()
    }

    fn clear_pins(&self) {
        let mut pins = lock_unpoisoned(&self.pins);
        pins.routes.clear();
        pins.insertion_order.clear();
    }

    fn pin_count(&self) -> usize {
        lock_unpoisoned(&self.pins).routes.len()
    }

    fn thread_pin_count(&self) -> usize {
        lock_unpoisoned(&self.thread_pins).routes.len()
    }

    fn catalog_response(&self) -> Option<CodexModelsResponse> {
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        let mut ingest = |route: &RouteConfig| {
            for model in route.models_response().models {
                if seen.insert(model.slug.clone()) {
                    models.push(model);
                }
            }
        };
        for route in lock_unpoisoned(&self.thread_pins).routes.values() {
            ingest(route);
        }
        for route in lock_unpoisoned(&self.conversation_pins).routes.values() {
            ingest(route);
        }
        for route in lock_unpoisoned(&self.recent).routes.values() {
            ingest(route);
        }
        if let Some(active) = self.active() {
            ingest(&active);
        }
        if models.is_empty() {
            None
        } else {
            Some(CodexModelsResponse { models })
        }
    }

    fn persist(&self) {
        let Some(path) = &self.bindings_path else {
            return;
        };
        let mut threads = {
            let pins = lock_unpoisoned(&self.thread_pins);
            pins.insertion_order
                .iter()
                .filter_map(|thread_id| {
                    let route = pins.routes.get(thread_id)?;
                    Some(ThreadBinding {
                        thread_id: thread_id.clone(),
                        route_id: route.id.clone(),
                        selected_model: route.selected_model.clone(),
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut seen_threads: HashSet<String> = threads
            .iter()
            .map(|binding| binding.thread_id.clone())
            .collect();
        for binding in lock_unpoisoned(&self.pending_threads).iter() {
            if seen_threads.insert(binding.thread_id.clone()) {
                threads.push(binding.clone());
            }
        }
        let mut conversations = {
            let pins = lock_unpoisoned(&self.conversation_pins);
            pins.insertion_order
                .iter()
                .filter_map(|conversation_id| {
                    let route = pins.routes.get(conversation_id)?;
                    Some(ConversationBinding {
                        conversation_id: conversation_id.clone(),
                        route_id: route.id.clone(),
                        selected_model: route.selected_model.clone(),
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut seen_conversations: HashSet<String> = conversations
            .iter()
            .map(|binding| binding.conversation_id.clone())
            .collect();
        for binding in lock_unpoisoned(&self.pending_conversations).iter() {
            if seen_conversations.insert(binding.conversation_id.clone()) {
                conversations.push(binding.clone());
            }
        }
        let mut recent = {
            let recent = lock_unpoisoned(&self.recent);
            recent
                .insertion_order
                .iter()
                .filter_map(|key| {
                    let route = recent.routes.get(key)?;
                    Some(RouteBinding {
                        route_id: route.id.clone(),
                        selected_model: route.selected_model.clone(),
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut seen_recent: HashSet<(String, String)> = recent
            .iter()
            .map(|binding| (binding.route_id.clone(), binding.selected_model.clone()))
            .collect();
        for binding in lock_unpoisoned(&self.pending_recent).iter() {
            if seen_recent.insert((binding.route_id.clone(), binding.selected_model.clone())) {
                recent.push(binding.clone());
            }
        }
        let payload = ProxyBindings {
            schema_version: BINDINGS_SCHEMA_VERSION,
            threads,
            recent,
            conversations,
        };
        let Ok(bytes) = serde_json::to_vec_pretty(&payload) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

#[derive(Default)]
struct PinnedRoutes {
    routes: HashMap<TurnKey, Arc<RouteConfig>>,
    insertion_order: VecDeque<TurnKey>,
}

#[derive(Default)]
struct LruRoutes {
    routes: HashMap<(String, String), Arc<RouteConfig>>,
    insertion_order: VecDeque<(String, String)>,
}

impl LruRoutes {
    fn insert(&mut self, route: Arc<RouteConfig>, max: usize) {
        let key = (route.id.clone(), route.selected_model.clone());
        if self
            .routes
            .insert(key.clone(), Arc::clone(&route))
            .is_some()
        {
            self.insertion_order.retain(|item| item != &key);
        }
        self.insertion_order.push_back(key);
        while self.routes.len() > max {
            let Some(stale) = self.insertion_order.pop_front() else {
                break;
            };
            self.routes.remove(&stale);
        }
    }
}

#[derive(Default)]
struct LruThreads {
    routes: HashMap<String, Arc<RouteConfig>>,
    insertion_order: VecDeque<String>,
}

impl LruThreads {
    fn insert(&mut self, thread_id: String, route: Arc<RouteConfig>, max: usize) {
        if self.routes.insert(thread_id.clone(), route).is_some() {
            self.insertion_order.retain(|item| item != &thread_id);
        }
        self.insertion_order.push_back(thread_id);
        while self.routes.len() > max {
            let Some(stale) = self.insertion_order.pop_front() else {
                break;
            };
            self.routes.remove(&stale);
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TurnKey {
    thread_id: String,
    turn_id: String,
}

impl TurnKey {
    fn new(thread_id: &str, turn_id: &str) -> Result<Self, ProxyError> {
        validate_turn_key_part("thread_id", thread_id)?;
        validate_turn_key_part("turn_id", turn_id)?;
        Ok(Self::new_unchecked(thread_id, turn_id))
    }

    fn new_unchecked(thread_id: &str, turn_id: &str) -> Self {
        Self {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
        }
    }

    fn from_request(headers: &HeaderMap, body: &Value) -> Result<Option<Self>, ProxyError> {
        let thread_id = extract_thread_id(headers, body);
        let turn_id = first_header_value(headers, &["turn-id", "x-codex-turn-id"])
            .or_else(|| string_at(body, "/client_metadata/turn_id"))
            .or_else(|| string_at(body, "/turn_id"));

        match (thread_id, turn_id) {
            (Some(thread_id), Some(turn_id)) => Self::new(thread_id, turn_id).map(Some),
            _ => Ok(None),
        }
    }
}

fn extract_thread_id<'a>(headers: &'a HeaderMap, body: &'a Value) -> Option<&'a str> {
    first_header_value(
        headers,
        &[
            "thread-id",
            "x-codex-thread-id",
            "session-id",
            "x-session-id",
            "x-codex-session-id",
        ],
    )
    .or_else(|| string_at(body, "/client_metadata/thread_id"))
    .or_else(|| string_at(body, "/client_metadata/session_id"))
    .or_else(|| string_at(body, "/thread_id"))
    .or_else(|| string_at(body, "/session_id"))
    .or_else(|| string_at(body, "/metadata/thread_id"))
    .or_else(|| string_at(body, "/metadata/session_id"))
}

fn extract_conversation_ids(headers: &HeaderMap, body: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let mut push = |value: Option<&str>| {
        let Some(value) = value else {
            return;
        };
        if value.len() > MAX_TURN_KEY_BYTES || ids.iter().any(|existing| existing == value) {
            return;
        }
        ids.push(value.to_string());
    };
    push(string_at(body, "/previous_response_id"));
    push(string_at(body, "/conversation_id"));
    push(string_at(body, "/conversation/id"));
    push(string_at(body, "/conversation"));
    push(string_at(body, "/metadata/conversation_id"));
    push(string_at(body, "/prompt_cache_key"));
    push(string_at(body, "/response_id"));
    push(first_header_value(
        headers,
        &[
            "conversation-id",
            "x-conversation-id",
            "x-codex-conversation-id",
        ],
    ));
    ids
}

fn outbound_model(route: &RouteConfig, requested: Option<&str>) -> String {
    match requested {
        Some(model) if route.contains_model(model) => model.to_string(),
        _ => route.selected_model.clone(),
    }
}

fn bind_route_to_model(route: &Arc<RouteConfig>, model: &str) -> Arc<RouteConfig> {
    if route.selected_model == model {
        return Arc::clone(route);
    }
    route
        .with_selected_model(model)
        .map(Arc::new)
        .unwrap_or_else(|| Arc::clone(route))
}

fn strip_continuation_fields(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    for key in [
        "previous_response_id",
        "conversation_id",
        "conversation",
        "prompt_cache_key",
        "response_id",
    ] {
        object.remove(key);
    }
    if let Some(Value::Object(metadata)) = object.get_mut("metadata") {
        metadata.remove("conversation_id");
    }
}

#[derive(Default)]
struct ResponseIdScanner {
    tail: Vec<u8>,
}

impl ResponseIdScanner {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        if chunk.is_empty() {
            return Vec::new();
        }
        self.tail.extend_from_slice(chunk);
        let ids = extract_provider_ids(&self.tail);
        const KEEP: usize = 64;
        if self.tail.len() > KEEP {
            let drain_to = self.tail.len() - KEEP;
            self.tail.drain(..drain_to);
        }
        ids
    }
}

fn extract_provider_ids(bytes: &[u8]) -> Vec<String> {
    let mut ids = Vec::new();
    let mut index = 0;
    while index + 5 <= bytes.len() {
        let prefix_len = if bytes[index..].starts_with(b"resp_")
            || bytes[index..].starts_with(b"conv_")
            || bytes[index..].starts_with(b"sess_")
        {
            5
        } else {
            index += 1;
            continue;
        };
        let start = index;
        index += prefix_len;
        while index < bytes.len() && is_provider_id_char(bytes[index]) {
            index += 1;
        }
        let len = index - start;
        if len >= 8 && len <= MAX_TURN_KEY_BYTES {
            if let Ok(id) = std::str::from_utf8(&bytes[start..index])
                && !ids.iter().any(|existing| existing == id)
            {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

fn is_provider_id_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

#[derive(Clone, Copy)]
enum UpstreamEndpoint {
    Responses,
    Compact,
}

impl UpstreamEndpoint {
    fn relative_path(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::Compact => "responses/compact",
        }
    }
}

async fn health_handler(State(state): State<Arc<ProxyState>>, headers: HeaderMap) -> Response {
    if !state.auth.authorize(&headers) {
        return unauthorized_response();
    }
    Json(state.health(state.listen_addr)).into_response()
}

async fn models_handler(State(state): State<Arc<ProxyState>>, headers: HeaderMap) -> Response {
    if !state.auth.authorize(&headers) {
        return unauthorized_response();
    }
    let Some(catalog) = state.routes.catalog_response() else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "no active provider route");
    };

    let etag = catalog_etag_for(&catalog);
    if headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|candidate| candidate == etag)
    {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response.headers_mut().insert(
            ETAG,
            HeaderValue::from_str(&etag).expect("catalog ETag is always valid ASCII"),
        );
        return response;
    }

    let body = serde_json::to_vec(&catalog)
        .expect("Codex models response only contains serializable values");
    let mut response = Response::new(Body::from(body));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&etag).expect("catalog ETag is always valid ASCII"),
    );
    response
}

async fn responses_handler(
    State(state): State<Arc<ProxyState>>,
    request: Request<Body>,
) -> Response {
    proxy_request(state, request, UpstreamEndpoint::Responses).await
}

async fn compact_handler(State(state): State<Arc<ProxyState>>, request: Request<Body>) -> Response {
    proxy_request(state, request, UpstreamEndpoint::Compact).await
}

async fn proxy_request(
    state: Arc<ProxyState>,
    request: Request<Body>,
    endpoint: UpstreamEndpoint,
) -> Response {
    if !state.auth.authorize(request.headers()) {
        return unauthorized_response();
    }

    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, state.max_request_bytes).await {
        Ok(body) => body,
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds the local proxy limit",
            );
        }
    };
    let mut body: Value = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "request body must be a JSON object",
            );
        }
    };
    if !body.is_object() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "request body must be a JSON object",
        );
    }

    let turn_key = match TurnKey::from_request(&parts.headers, &body) {
        Ok(turn_key) => turn_key,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "thread_id and turn_id must be non-empty and bounded",
            );
        }
    };
    let thread_id = extract_thread_id(&parts.headers, &body).map(str::to_string);
    let conversation_ids = extract_conversation_ids(&parts.headers, &body);
    let requested_model = string_at(&body, "/model").map(str::to_string);
    let Some(route) = state.routes.resolve_with_conversations(
        thread_id.as_deref(),
        turn_key,
        requested_model.as_deref(),
        &conversation_ids,
    ) else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "no active provider route");
    };

    let outbound_model = outbound_model(route.as_ref(), requested_model.as_deref());
    if !conversation_ids.is_empty() && outbound_model != route.selected_model {
        strip_continuation_fields(&mut body);
    }
    body.as_object_mut()
        .expect("object shape checked above")
        .insert("model".to_string(), Value::String(outbound_model.clone()));
    let body = match serde_json::to_vec(&body) {
        Ok(body) => body,
        Err(_) => {
            return error_response(StatusCode::BAD_REQUEST, "request body could not be encoded");
        }
    };

    let mut headers = sanitize_request_headers(&parts.headers);
    let upstream_authorization = match route.upstream_bearer.authorization_header() {
        Ok(header) => header,
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "active provider credential is invalid",
            );
        }
    };
    headers.insert(AUTHORIZATION, upstream_authorization);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    if !headers.contains_key(USER_AGENT) {
        headers.insert(USER_AGENT, UPSTREAM_USER_AGENT);
    }

    let request_start_time = std::time::Instant::now();
    let local_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let log_time = {
        let secs = local_timestamp % 86400;
        let hours = (secs / 3600 + 8) % 24;
        let mins = (secs % 3600) / 60;
        let s = secs % 60;
        format!("{:02}:{:02}:{:02}", hours, mins, s)
    };
    let log_id = format!("req-{}", uuid::Uuid::new_v4().simple());
    let log_provider = route.id.clone();
    let log_model = outbound_model.clone();
    let log_endpoint = format!("/{}", endpoint.relative_path());
    let log_thread_id = thread_id.clone();

    let lease = RequestLease::begin(Arc::clone(&state.metrics));
    let mut retry_count = 0_u32;
    let upstream = loop {
        let attempt = state
            .client
            .post(route.endpoint_url(endpoint))
            .headers(headers.clone())
            .body(body.clone())
            .send()
            .await;
        match attempt {
            Ok(response) if response.status().is_success() => break response,
            Ok(response) if is_retryable_status(response.status()) => {
                let status = response.status();
                let retry_after = retry_after_duration(response.headers());
                let error_body = response.bytes().await.ok();
                if retry_count as usize >= state.upstream_max_retries
                    || !wait_before_retry(
                        request_start_time,
                        state.upstream_retry_max_elapsed,
                        state.upstream_retry_base_delay,
                        state.upstream_retry_max_delay,
                        retry_count as usize,
                        retry_after,
                    )
                    .await
                {
                    let duration_ms = request_start_time.elapsed().as_millis() as u64;
                    let message = error_body
                        .as_deref()
                        .and_then(json_error_message)
                        .or_else(|| {
                            error_body
                                .as_deref()
                                .and_then(|body| html_error_message(status, body))
                        })
                        .unwrap_or_else(|| format!("upstream provider returned {status}"));
                    drop(lease);
                    state.record_request_log(ProxyRequestLog {
                        id: log_id,
                        time: log_time,
                        provider: log_provider,
                        model: log_model,
                        endpoint: log_endpoint,
                        status: status.as_u16(),
                        duration_ms,
                        thread_id: log_thread_id,
                        error: Some(message.clone()),
                        details: Some(format!(
                            "Upstream returned retryable status {status} after {retry_count} retries"
                        )),
                        retry_count,
                        first_byte_ms: None,
                        response_bytes: 0,
                        stream_duration_ms: None,
                        stream_completed: false,
                        stream_error: None,
                    });
                    return error_response(status, message);
                }
                retry_count += 1;
            }
            Ok(response) => break response,
            Err(error) => {
                if retry_count as usize >= state.upstream_max_retries
                    || !wait_before_retry(
                        request_start_time,
                        state.upstream_retry_max_elapsed,
                        state.upstream_retry_base_delay,
                        state.upstream_retry_max_delay,
                        retry_count as usize,
                        None,
                    )
                    .await
                {
                    drop(lease);
                    let duration_ms = request_start_time.elapsed().as_millis() as u64;
                    let err_msg = classify_upstream_error(&error);
                    state.record_request_log(ProxyRequestLog {
                        id: log_id,
                        time: log_time,
                        provider: log_provider,
                        model: log_model,
                        endpoint: log_endpoint,
                        status: StatusCode::BAD_GATEWAY.as_u16(),
                        duration_ms,
                        thread_id: log_thread_id,
                        error: Some(err_msg.to_string()),
                        details: Some(format!(
                            "Gateway error: {err_msg} after {retry_count} retries"
                        )),
                        retry_count,
                        first_byte_ms: None,
                        response_bytes: 0,
                        stream_duration_ms: None,
                        stream_completed: false,
                        stream_error: None,
                    });
                    return error_response(StatusCode::BAD_GATEWAY, err_msg);
                }
                retry_count += 1;
            }
        }
    };

    let status = upstream.status();
    state
        .metrics
        .last_upstream_status
        .store(status.as_u16(), Ordering::Release);
    let duration_ms = request_start_time.elapsed().as_millis() as u64;
    state.record_request_log(ProxyRequestLog {
        id: log_id.clone(),
        time: log_time,
        provider: log_provider,
        model: log_model,
        endpoint: log_endpoint,
        status: status.as_u16(),
        duration_ms,
        thread_id: log_thread_id,
        error: if status.is_client_error() || status.is_server_error() {
            Some(format!("HTTP {}", status.as_u16()))
        } else {
            None
        },
        details: Some(format!(
            "Upstream returned status {} in {} ms after {} retries",
            status.as_u16(),
            duration_ms,
            retry_count
        )),
        retry_count,
        first_byte_ms: None,
        response_bytes: 0,
        stream_duration_ms: None,
        stream_completed: false,
        stream_error: None,
    });
    if status.is_client_error() || status.is_server_error() {
        drop(lease);
        return normalize_upstream_error(status, upstream).await;
    }
    let pin_route = bind_route_to_model(&route, &outbound_model);
    if let Some(thread_id) = thread_id.as_deref() {
        state.routes.pin_thread(thread_id, Arc::clone(&pin_route));
    }
    for conversation_id in &conversation_ids {
        state
            .routes
            .pin_conversation(conversation_id, Arc::clone(&pin_route));
    }
    let mut headers = sanitize_response_headers(upstream.headers());
    if let Some(catalog) = state.routes.catalog_response() {
        headers.insert(
            X_MODELS_ETAG,
            HeaderValue::from_str(&catalog_etag_for(&catalog))
                .expect("catalog ETag is always valid ASCII"),
        );
    }
    let scanner = RefCell::new(ResponseIdScanner::default());
    let pin_state = Arc::clone(&state);
    let pin_route = Arc::clone(&pin_route);
    let log_state = Arc::clone(&state);
    let stream_log_id = log_id.clone();
    let stream_start = request_start_time;
    let stream = stream::unfold(
        (
            upstream.bytes_stream(),
            scanner,
            pin_state,
            pin_route,
            log_state,
            stream_log_id,
            stream_start,
            lease,
            0_u64,
            false,
            false,
        ),
        |(
            mut upstream_stream,
            scanner,
            pin_state,
            pin_route,
            log_state,
            stream_log_id,
            stream_start,
            lease,
            mut response_bytes,
            mut first_byte_seen,
            finished,
        )| async move {
            if finished {
                return None;
            }
            match upstream_stream.next().await {
                Some(Ok(bytes)) => {
                    if !first_byte_seen {
                        first_byte_seen = true;
                        log_state.update_request_log(&stream_log_id, |log| {
                            log.first_byte_ms = Some(stream_start.elapsed().as_millis() as u64);
                        });
                    }
                    response_bytes = response_bytes.saturating_add(bytes.len() as u64);
                    for id in scanner.borrow_mut().push(&bytes) {
                        pin_state
                            .routes
                            .pin_conversation(&id, Arc::clone(&pin_route));
                    }
                    log_state.update_request_log(&stream_log_id, |log| {
                        log.response_bytes = response_bytes;
                    });
                    Some((
                        Ok(bytes),
                        (
                            upstream_stream,
                            scanner,
                            pin_state,
                            pin_route,
                            log_state,
                            stream_log_id,
                            stream_start,
                            lease,
                            response_bytes,
                            first_byte_seen,
                            false,
                        ),
                    ))
                }
                Some(Err(_)) => {
                    log_state.update_request_log(&stream_log_id, |log| {
                        log.stream_duration_ms = Some(stream_start.elapsed().as_millis() as u64);
                        log.stream_error = Some("upstream response stream failed".to_string());
                    });
                    Some((
                        Err(io::Error::other("upstream response stream failed")),
                        (
                            upstream_stream,
                            scanner,
                            pin_state,
                            pin_route,
                            log_state,
                            stream_log_id,
                            stream_start,
                            lease,
                            response_bytes,
                            first_byte_seen,
                            true,
                        ),
                    ))
                }
                None => {
                    log_state.update_request_log(&stream_log_id, |log| {
                        log.stream_duration_ms = Some(stream_start.elapsed().as_millis() as u64);
                        log.stream_completed = true;
                    });
                    None
                }
            }
        },
    );

    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn unauthorized_response() -> Response {
    error_response(StatusCode::UNAUTHORIZED, "invalid local proxy bearer token")
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn retry_after_duration(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

async fn wait_before_retry(
    request_start: std::time::Instant,
    max_elapsed: Duration,
    base_delay: Duration,
    max_delay: Duration,
    retry_index: usize,
    retry_after: Option<Duration>,
) -> bool {
    let elapsed = request_start.elapsed();
    let Some(remaining) = max_elapsed.checked_sub(elapsed) else {
        return false;
    };
    let exponential = base_delay
        .checked_mul(
            1_u32
                .checked_shl(retry_index.min(31) as u32)
                .unwrap_or(u32::MAX),
        )
        .unwrap_or(max_delay);
    let delay = retry_after.unwrap_or(exponential).min(max_delay);
    if delay > remaining {
        return false;
    }
    tokio::time::sleep(delay).await;
    true
}

fn error_response(status: StatusCode, message: impl AsRef<str>) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": sanitize_error_message(message.as_ref()),
                "type": "api_error"
            }
        })),
    )
        .into_response()
}

async fn normalize_upstream_error(status: StatusCode, upstream: reqwest::Response) -> Response {
    let bytes = match upstream.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(
                status,
                format!("upstream provider returned {status} and the error body could not be read"),
            );
        }
    };
    let limited = if bytes.len() > MAX_ERROR_BODY_BYTES {
        &bytes[..MAX_ERROR_BODY_BYTES]
    } else {
        bytes.as_ref()
    };
    if let Some(message) = json_error_message(limited) {
        return error_response(status, message);
    }
    if let Some(message) = html_error_message(status, limited) {
        return error_response(status, message);
    }
    error_response(status, format!("upstream provider returned {status}"))
}

fn classify_upstream_error(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "the active provider timed out"
    } else if error.is_connect() {
        "the active provider could not be reached"
    } else {
        "the active provider request failed"
    }
}

fn json_error_message(bytes: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    const POINTERS: &[&str] = &["/error/message", "/error/msg", "/message", "/msg", "/error"];
    for pointer in POINTERS {
        match value.pointer(pointer) {
            Some(Value::String(text)) => {
                let text = sanitize_error_message(text);
                if !text.is_empty() {
                    return Some(text);
                }
            }
            _ => {}
        }
    }
    None
}

fn html_error_message(status: StatusCode, bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let lowered = text.to_ascii_lowercase();
    if !lowered.contains("<html") && !lowered.contains("<!doctype") {
        return None;
    }
    if lowered.contains("sorry, you have been blocked")
        || lowered.contains("attention required")
        || lowered.contains("you are unable to access")
    {
        return Some(format!("Cloudflare blocked the active provider ({status})"));
    }
    if lowered.contains("error code 522")
        || lowered.contains("error code 524")
        || lowered.contains("connection timed out")
    {
        return Some("the active provider timed out".to_string());
    }
    Some(format!("upstream provider returned {status}"))
}

fn sanitize_error_message(input: &str) -> String {
    let mut output = String::new();
    for ch in input.chars() {
        if output.len() >= MAX_ERROR_MESSAGE_BYTES {
            break;
        }
        if matches!(ch, '\n' | '\r' | '\t') {
            if !output.ends_with(' ') {
                output.push(' ');
            }
        } else if !ch.is_control() {
            output.push(ch);
        }
    }
    output.trim().to_string()
}

fn sanitize_request_headers(source: &HeaderMap) -> HeaderMap {
    sanitize_headers(source, true)
}

fn sanitize_response_headers(source: &HeaderMap) -> HeaderMap {
    sanitize_headers(source, false)
}

fn sanitize_headers(source: &HeaderMap, request: bool) -> HeaderMap {
    let connection_headers = connection_declared_headers(source);
    let mut sanitized = HeaderMap::new();
    for (name, value) in source {
        if is_hop_by_hop(name)
            || connection_headers.contains(name.as_str())
            || name == HOST
            || name == CONTENT_LENGTH
            || (request && is_sensitive_client_header(name))
            || (!request && name.as_str() == "set-cookie")
        {
            continue;
        }
        sanitized.append(name.clone(), value.clone());
    }
    sanitized
}

fn connection_declared_headers(headers: &HeaderMap) -> HashSet<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn is_sensitive_client_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "authorization"
            | "cookie"
            | "api-key"
            | "apikey"
            | "x-api-key"
            | "x-auth-token"
            | "x-access-token"
            | "openai-api-key"
            | "x-openai-api-key"
            | "anthropic-api-key"
            | "x-goog-api-key"
            | "x-azure-api-key"
            | "cf-access-client-secret"
            | "forwarded"
            | "x-forwarded-for"
            | "x-forwarded-host"
            | "x-forwarded-proto"
            | "x-forwarded-port"
            | "via"
    )
}

fn first_header_value<'a>(headers: &'a HeaderMap, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn validate_turn_key_part(name: &'static str, value: &str) -> Result<(), ProxyError> {
    if value.trim().is_empty() || value.len() > MAX_TURN_KEY_BYTES {
        return Err(ProxyError::InvalidTurnKey(name));
    }
    Ok(())
}

fn validate_identifier(
    name: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ProxyError> {
    if value.trim().is_empty() || value.len() > max_bytes {
        return Err(ProxyError::InvalidRoute(format!(
            "{name} must be non-empty and no longer than {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn normalize_upstream_base_url(input: &str) -> Result<Url, ProxyError> {
    let mut url = Url::parse(input)
        .map_err(|_| ProxyError::InvalidRoute("upstream Base URL is invalid".to_string()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ProxyError::InvalidRoute(
            "upstream Base URL cannot contain credentials".to_string(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ProxyError::InvalidRoute(
            "upstream Base URL cannot contain a query or fragment".to_string(),
        ));
    }
    let secure = url.scheme() == "https";
    let loopback_http = url.scheme() == "http"
        && url.host().is_some_and(|host| match host {
            Host::Ipv4(address) => address.is_loopback(),
            Host::Ipv6(address) => address.is_loopback(),
            Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        });
    if !secure && !loopback_http {
        return Err(ProxyError::InvalidRoute(
            "upstream Base URL must use HTTPS, except for a loopback HTTP provider".to_string(),
        ));
    }
    if url.cannot_be_a_base() || url.host().is_none() {
        return Err(ProxyError::InvalidRoute(
            "upstream Base URL must be hierarchical and include a host".to_string(),
        ));
    }
    let normalized_path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&normalized_path);
    Ok(url)
}

fn system_proxy_skipping_loopback() -> Option<reqwest::Proxy> {
    let proxy_url = env_proxy_url().or_else(windows_ie_proxy_url)?;
    Some(
        reqwest::Proxy::all(proxy_url)
            .ok()?
            .no_proxy(reqwest::NoProxy::from_string(
                "127.0.0.1,localhost,::1,[::1]",
            )),
    )
}

fn env_proxy_url() -> Option<String> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(normalize_proxy_url(value));
            }
        }
    }
    None
}

#[cfg(windows)]
fn windows_ie_proxy_url() -> Option<String> {
    let settings = windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    if settings.get_u32("ProxyEnable").unwrap_or(0) == 0 {
        return None;
    }
    let raw = settings.get_string("ProxyServer").ok()?;
    parse_windows_proxy_server(&raw)
}

#[cfg(not(windows))]
fn windows_ie_proxy_url() -> Option<String> {
    None
}

fn parse_windows_proxy_server(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if !raw.contains('=') {
        return Some(normalize_proxy_url(raw));
    }
    for scheme in ["https=", "http="] {
        for part in raw.split(';') {
            if let Some(rest) = part.trim().strip_prefix(scheme) {
                return Some(normalize_proxy_url(rest));
            }
        }
    }
    None
}

fn normalize_proxy_url(value: &str) -> String {
    if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("bearer tokens must be non-empty printable ASCII without spaces")]
    InvalidBearerToken,
    #[error("invalid proxy start options: {0}")]
    InvalidStartOptions(String),
    #[error("invalid provider route: {0}")]
    InvalidRoute(String),
    #[error("invalid {0}")]
    InvalidTurnKey(&'static str),
    #[error("failed to bind the local proxy on 127.0.0.1")]
    Bind(#[source] io::Error),
    #[error("the local proxy listener was not bound to 127.0.0.1")]
    NonLoopbackListener,
    #[error("failed to construct the upstream HTTP client")]
    BuildClient(#[source] reqwest::Error),
    #[error("the local proxy server stopped unexpectedly")]
    Server(#[source] io::Error),
    #[error("the local proxy server task failed")]
    ServerTask(#[source] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: &str) -> BearerToken {
        BearerToken::new(value).expect("valid test token")
    }

    fn route(id: &str, model: &str) -> RouteConfig {
        RouteConfig::single_model(
            id,
            "https://provider.example/v1",
            ModelDescriptor::new(model, model),
            token("upstream-token-for-tests"),
        )
        .expect("valid test route")
    }

    #[test]
    fn bearer_debug_is_redacted_and_validation_rejects_header_injection() {
        let token = token("entry-token-for-tests");
        assert_eq!(format!("{token:?}"), "BearerToken([REDACTED])");
        assert!(BearerToken::new("unsafe\nvalue").is_err());
        assert!(BearerToken::new("contains space").is_err());
    }

    #[test]
    fn entry_bearer_comparison_accepts_only_one_matching_bearer_header() {
        let verifier = EntryTokenVerifier::new(token("entry-token-for-tests"));
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer entry-token-for-tests"),
        );
        assert!(verifier.authorize(&headers));

        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer incorrect-token"),
        );
        assert!(!verifier.authorize(&headers));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Basic entry-token-for-tests"),
        );
        assert!(!verifier.authorize(&headers));
    }

    #[test]
    fn active_route_swap_keeps_existing_threads_on_the_previous_route() {
        let table = RouteTable::new(8);
        table.set_active(route("route-a", "model-a"));
        let key = TurnKey::new("thread-1", "turn-1").unwrap();
        let first = table
            .resolve(Some("thread-1"), Some(key.clone()), Some("model-a"))
            .unwrap();
        assert_eq!(first.summary().id, "route-a");
        table.pin_thread("thread-1", Arc::clone(&first));

        table.set_active(route("route-b", "model-b"));
        let pinned = table
            .resolve(Some("thread-1"), Some(key.clone()), Some("model-b"))
            .unwrap();
        let next_turn = table
            .resolve(
                Some("thread-1"),
                Some(TurnKey::new("thread-1", "turn-2").unwrap()),
                Some("model-b"),
            )
            .unwrap();
        let new_thread = table
            .resolve(
                Some("thread-2"),
                Some(TurnKey::new("thread-2", "turn-1").unwrap()),
                None,
            )
            .unwrap();
        let requested_old = table
            .resolve(
                Some("thread-3"),
                Some(TurnKey::new("thread-3", "turn-1").unwrap()),
                Some("model-a"),
            )
            .unwrap();
        assert_eq!(pinned.summary().id, "route-a");
        assert_eq!(next_turn.summary().id, "route-a");
        assert_eq!(new_thread.summary().id, "route-b");
        assert_eq!(requested_old.summary().id, "route-a");

        let catalog = table.catalog_response().unwrap();
        let slugs: Vec<_> = catalog
            .models
            .iter()
            .map(|model| model.slug.as_str())
            .collect();
        assert!(slugs.contains(&"model-a"));
        assert!(slugs.contains(&"model-b"));

        assert!(table.release(&key));
        assert_eq!(
            table
                .resolve(Some("thread-1"), Some(key), Some("model-b"))
                .unwrap()
                .summary()
                .id,
            "route-a"
        );
    }

    #[test]
    fn previous_response_id_keeps_the_original_route_after_a_switch() {
        let table = RouteTable::new(8);
        table.set_active(route("route-a", "model-a"));
        let first = table.resolve(None, None, Some("model-a")).unwrap();
        table.pin_conversation("resp_oldchat1", Arc::clone(&first));

        table.set_active(route("route-b", "model-b"));
        let continued = table
            .resolve_with_conversations(None, None, Some("model-b"), &["resp_oldchat1".to_string()])
            .unwrap();
        let fresh = table.resolve(None, None, Some("model-b")).unwrap();
        assert_eq!(continued.summary().id, "route-a");
        assert_eq!(fresh.summary().id, "route-b");
    }

    #[test]
    fn provider_response_ids_are_extracted_from_sse_and_json() {
        let ids = extract_provider_ids(
            br#"data: {"type":"response.created","response":{"id":"resp_abc12345"}}"#,
        );
        assert!(ids.iter().any(|id| id == "resp_abc12345"));
        let ids = extract_provider_ids(br#"{"id":"conv_hello123","model":"x"}"#);
        assert!(ids.iter().any(|id| id == "conv_hello123"));
        let ids = extract_provider_ids(br#"{"id":"sess_hello123"}"#);
        assert!(ids.iter().any(|id| id == "sess_hello123"));
        assert!(extract_provider_ids(b"resp_ab").is_empty());
    }

    #[test]
    fn catalog_sibling_is_passed_through_instead_of_selecting_the_active_model() {
        let mixed = RouteConfig::new(
            "route-mixed",
            "https://provider.example/v1",
            "model-b",
            vec![
                ModelDescriptor::new("model-a", "A"),
                ModelDescriptor::new("model-b", "B"),
            ],
            token("upstream-token-for-tests"),
        )
        .unwrap();
        let table = RouteTable::new(8);
        table.set_active(mixed);
        let resolved = table.resolve(None, None, Some("model-a")).unwrap();
        assert_eq!(resolved.summary().selected_model, "model-b");
        assert_eq!(outbound_model(&resolved, Some("model-a")), "model-a");
        assert_eq!(outbound_model(&resolved, Some("unknown")), "model-b");
        assert_eq!(outbound_model(&resolved, None), "model-b");
    }

    #[test]
    fn failed_resolve_does_not_pin_a_thread() {
        let table = RouteTable::new(8);
        table.set_active(route("route-a", "model-a"));
        let _ = table
            .resolve(
                Some("thread-1"),
                Some(TurnKey::new("thread-1", "turn-1").unwrap()),
                Some("model-a"),
            )
            .unwrap();
        assert_eq!(table.thread_pin_count(), 0);

        table.set_active(route("route-b", "model-b"));
        let next = table
            .resolve(
                Some("thread-1"),
                Some(TurnKey::new("thread-1", "turn-2").unwrap()),
                Some("model-b"),
            )
            .unwrap();
        assert_eq!(next.summary().id, "route-b");
    }

    #[test]
    fn route_rejects_non_tls_remote_upstreams_and_normalizes_paths() {
        assert!(
            RouteConfig::single_model(
                "route",
                "http://provider.example/v1",
                ModelDescriptor::new("model", "Model"),
                token("upstream-token-for-tests"),
            )
            .is_err()
        );

        let route = RouteConfig::single_model(
            "route",
            "http://127.0.0.1:18080/v1",
            ModelDescriptor::new("model", "Model"),
            token("upstream-token-for-tests"),
        )
        .unwrap();
        assert_eq!(
            route.endpoint_url(UpstreamEndpoint::Responses).as_str(),
            "http://127.0.0.1:18080/v1/responses"
        );
        assert_eq!(
            route.endpoint_url(UpstreamEndpoint::Compact).as_str(),
            "http://127.0.0.1:18080/v1/responses/compact"
        );
    }

    #[test]
    fn models_response_is_codex_compatible_and_contains_no_credential() {
        let mut model = ModelDescriptor::new("model-a", "Model A");
        model.context_window = Some(64_000);
        model.supports_images = true;
        model.default_reasoning_level = Some("low".to_string());
        model.supported_reasoning_levels = vec![ReasoningLevelDescriptor {
            effort: "low".to_string(),
            description: "Fast".to_string(),
        }];
        let route = RouteConfig::single_model(
            "route",
            "https://provider.example/v1",
            model,
            token("upstream-token-for-tests"),
        )
        .unwrap();

        let value = serde_json::to_value(route.models_response()).unwrap();
        assert_eq!(value["models"][0]["slug"], "model-a");
        assert_eq!(
            value["models"][0]["input_modalities"],
            json!(["text", "image"])
        );
        assert_eq!(value["models"][0]["supports_image_detail_original"], true);
        assert_eq!(value["models"][0]["truncation_policy"]["mode"], "tokens");
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains("upstream-token-for-tests"));
    }

    #[test]
    fn sanitization_strips_sensitive_and_connection_declared_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer entry"));
        headers.insert("cookie", HeaderValue::from_static("session=secret"));
        headers.insert("x-api-key", HeaderValue::from_static("secret"));
        headers.insert(
            "connection",
            HeaderValue::from_static("x-remove, keep-alive"),
        );
        headers.insert("x-remove", HeaderValue::from_static("remove-me"));
        headers.insert("x-keep", HeaderValue::from_static("keep-me"));
        let sanitized = sanitize_request_headers(&headers);

        assert!(!sanitized.contains_key(AUTHORIZATION));
        assert!(!sanitized.contains_key("cookie"));
        assert!(!sanitized.contains_key("x-api-key"));
        assert!(!sanitized.contains_key("connection"));
        assert!(!sanitized.contains_key("x-remove"));
        assert_eq!(sanitized["x-keep"], "keep-me");
    }

    #[test]
    fn json_error_message_reads_openai_shaped_bodies_and_ignores_html() {
        assert_eq!(
            json_error_message(
                br#"{"error":{"message":"Service temporarily unavailable","type":"api_error"}}"#
            )
            .as_deref(),
            Some("Service temporarily unavailable")
        );
        assert_eq!(
            json_error_message(br#"{"message":"plain"}"#).as_deref(),
            Some("plain")
        );
        assert_eq!(
            json_error_message(b"<html><body>Bad Gateway</body></html>"),
            None
        );
        assert_eq!(
            html_error_message(
                StatusCode::BAD_GATEWAY,
                b"<html><body>Bad Gateway</body></html>"
            )
            .as_deref(),
            Some("upstream provider returned 502 Bad Gateway")
        );
        assert_eq!(
            html_error_message(
                StatusCode::FORBIDDEN,
                br#"<html><head><title>Attention Required! | Cloudflare</title></head><body>Sorry, you have been blocked</body></html>"#
            )
            .as_deref(),
            Some("Cloudflare blocked the active provider (403 Forbidden)")
        );
        assert_eq!(html_error_message(StatusCode::OK, b"not html"), None);
        assert_eq!(
            sanitize_error_message("line one\r\nline two\u{0007}"),
            "line one line two"
        );
        assert_eq!(
            parse_windows_proxy_server("127.0.0.1:7890").as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(
            parse_windows_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7890").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    async fn serve_http(
        status_line: &'static str,
        body: &'static str,
        record: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                record.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0_u8; 1024];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
                let response = format!(
                    "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn default_upstream_client_ignores_http_proxy_env() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let upstream_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let upstream_port =
            serve_http("HTTP/1.1 200 OK", "upstream-ok", upstream_hits.clone()).await;
        let proxy_port =
            serve_http("HTTP/1.1 502 Bad Gateway", "proxy-hit", proxy_hits.clone()).await;
        let _http = EnvGuard::set("HTTP_PROXY", &format!("http://127.0.0.1:{proxy_port}"));
        let _https = EnvGuard::set("HTTPS_PROXY", &format!("http://127.0.0.1:{proxy_port}"));
        let _all = EnvGuard::set("ALL_PROXY", &format!("http://127.0.0.1:{proxy_port}"));

        let handle =
            LocalProxy::start(ProxyStartOptions::default(), token("entry-token-for-tests"))
                .await
                .expect("start local proxy");
        handle.set_active_route(
            RouteConfig::single_model(
                "route",
                &format!("http://127.0.0.1:{upstream_port}/v1"),
                ModelDescriptor::new("model", "Model"),
                token("upstream-token-for-tests"),
            )
            .expect("loopback route"),
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("test client");
        let response = client
            .post(format!(
                "http://127.0.0.1:{}/v1/responses",
                handle.listen_addr().port()
            ))
            .header(AUTHORIZATION, "Bearer entry-token-for-tests")
            .json(&serde_json::json!({"input":"ping"}))
            .send()
            .await
            .expect("local responses");
        let body = response.text().await.expect("body");
        assert!(body.contains("upstream-ok"), "{body}");
        assert_eq!(upstream_hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(proxy_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn system_proxy_mode_uses_http_proxy_env() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let proxy_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy_port =
            serve_http("HTTP/1.1 502 Bad Gateway", "proxy-hit", proxy_hits.clone()).await;
        let _http = EnvGuard::set("HTTP_PROXY", &format!("http://127.0.0.1:{proxy_port}"));
        let _https = EnvGuard::set("HTTPS_PROXY", &format!("http://127.0.0.1:{proxy_port}"));
        let _all = EnvGuard::set("ALL_PROXY", &format!("http://127.0.0.1:{proxy_port}"));

        let handle = LocalProxy::start(
            ProxyStartOptions {
                use_system_proxy: true,
                ..ProxyStartOptions::default()
            },
            token("entry-token-for-tests"),
        )
        .await
        .expect("start local proxy");
        handle.set_active_route(
            RouteConfig::single_model(
                "route",
                "https://example.invalid/v1",
                ModelDescriptor::new("model", "Model"),
                token("upstream-token-for-tests"),
            )
            .expect("https route"),
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("test client");
        let _ = client
            .post(format!(
                "http://127.0.0.1:{}/v1/responses",
                handle.listen_addr().port()
            ))
            .header(AUTHORIZATION, "Bearer entry-token-for-tests")
            .json(&serde_json::json!({"input":"ping"}))
            .send()
            .await;
        assert!(
            proxy_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "system proxy mode should send the upstream request through HTTP_PROXY"
        );
        handle.shutdown().await.expect("shutdown");
    }
}
