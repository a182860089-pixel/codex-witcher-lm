//! Loopback-only Responses API proxy for the focused Codex provider switcher.
//!
//! The proxy keeps entry and upstream bearer credentials in memory, atomically
//! swaps immutable routes, and pins a route for a `(thread_id, turn_id)` pair.
//! It is intentionally a Rust API rather than a command-line program so Tauri
//! can obtain secrets from the platform credential store before constructing a
//! route.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::header::{
    ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HOST, IF_NONE_MATCH,
    USER_AGENT,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
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
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
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

    fn catalog_etag(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.id.as_bytes());
        hasher.update([0]);
        hasher.update(self.selected_model.as_bytes());
        hasher.update([0]);
        hasher.update(
            serde_json::to_vec(&self.models_response())
                .expect("Codex models response only contains serializable values"),
        );
        format!("\"{}\"", encode_hex(&hasher.finalize()))
    }
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
    pub upstream_connect_timeout: Duration,
    /// When false (crate default), the upstream client ignores `HTTP_PROXY`.
    pub use_system_proxy: bool,
}

impl Default for ProxyStartOptions {
    fn default() -> Self {
        Self {
            port: 0,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            max_pinned_turns: DEFAULT_MAX_PINNED_TURNS,
            upstream_connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            use_system_proxy: false,
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
    pub forwarded_requests: u64,
    pub in_flight_requests: usize,
    pub last_upstream_status: Option<u16>,
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
            routes: RouteTable::new(options.max_pinned_turns),
            client,
            listen_addr,
            max_request_bytes: options.max_request_bytes,
            metrics: Arc::new(ProxyMetrics::default()),
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
        self.state
            .routes
            .active()
            .map(|route| route.models_response())
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
    metrics: Arc<ProxyMetrics>,
}

impl ProxyState {
    fn health(&self, listen_addr: SocketAddr) -> ProxyHealth {
        let last_status = self.metrics.last_upstream_status.load(Ordering::Acquire);
        ProxyHealth {
            running: self.metrics.running.load(Ordering::Acquire),
            listen_addr,
            active_route: self.routes.active().map(|route| route.summary()),
            pinned_turns: self.routes.pin_count(),
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
    pins: Mutex<PinnedRoutes>,
    max_pinned_turns: usize,
}

impl RouteTable {
    fn new(max_pinned_turns: usize) -> Self {
        Self {
            active: ArcSwapOption::empty(),
            pins: Mutex::new(PinnedRoutes::default()),
            max_pinned_turns,
        }
    }

    fn active(&self) -> Option<Arc<RouteConfig>> {
        self.active.load_full()
    }

    fn set_active(&self, route: RouteConfig) -> Option<Arc<RouteConfig>> {
        self.active.swap(Some(Arc::new(route)))
    }

    fn clear_active(&self) -> Option<Arc<RouteConfig>> {
        self.active.swap(None)
    }

    fn resolve(&self, turn_key: Option<TurnKey>) -> Option<Arc<RouteConfig>> {
        let Some(turn_key) = turn_key else {
            return self.active();
        };

        let mut pins = lock_unpoisoned(&self.pins);
        if let Some(route) = pins.routes.get(&turn_key) {
            return Some(Arc::clone(route));
        }

        let active = self.active()?;
        while pins.routes.len() >= self.max_pinned_turns {
            let Some(stale_key) = pins.insertion_order.pop_front() else {
                break;
            };
            pins.routes.remove(&stale_key);
        }
        pins.insertion_order.push_back(turn_key.clone());
        pins.routes.insert(turn_key, Arc::clone(&active));
        Some(active)
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
}

#[derive(Default)]
struct PinnedRoutes {
    routes: HashMap<TurnKey, Arc<RouteConfig>>,
    insertion_order: VecDeque<TurnKey>,
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
        let thread_id = first_header_value(
            headers,
            &["thread-id", "x-codex-thread-id", "x-client-request-id"],
        )
        .or_else(|| string_at(body, "/client_metadata/thread_id"))
        .or_else(|| string_at(body, "/thread_id"));
        let turn_id = first_header_value(headers, &["turn-id", "x-codex-turn-id"])
            .or_else(|| string_at(body, "/client_metadata/turn_id"))
            .or_else(|| string_at(body, "/turn_id"));

        match (thread_id, turn_id) {
            (Some(thread_id), Some(turn_id)) => Self::new(thread_id, turn_id).map(Some),
            _ => Ok(None),
        }
    }
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
    let Some(route) = state.routes.active() else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "no active provider route");
    };

    let etag = route.catalog_etag();
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

    let body = serde_json::to_vec(&route.models_response())
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
    let Some(route) = state.routes.resolve(turn_key) else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "no active provider route");
    };

    body.as_object_mut()
        .expect("object shape checked above")
        .insert(
            "model".to_string(),
            Value::String(route.selected_model.clone()),
        );
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

    let lease = RequestLease::begin(Arc::clone(&state.metrics));
    let upstream = match state
        .client
        .post(route.endpoint_url(endpoint))
        .headers(headers)
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            drop(lease);
            return error_response(StatusCode::BAD_GATEWAY, classify_upstream_error(&error));
        }
    };

    let status = upstream.status();
    state
        .metrics
        .last_upstream_status
        .store(status.as_u16(), Ordering::Release);
    if status.is_client_error() || status.is_server_error() {
        drop(lease);
        return normalize_upstream_error(status, upstream).await;
    }
    let mut headers = sanitize_response_headers(upstream.headers());
    if let Some(active_route) = state.routes.active() {
        headers.insert(
            X_MODELS_ETAG,
            HeaderValue::from_str(&active_route.catalog_etag())
                .expect("catalog ETag is always valid ASCII"),
        );
    }
    let stream = upstream.bytes_stream().map(move |item| {
        let _keep_request_active_until_stream_drop = &lease;
        item.map_err(|_| io::Error::other("upstream response stream failed"))
    });

    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn unauthorized_response() -> Response {
    error_response(StatusCode::UNAUTHORIZED, "invalid local proxy bearer token")
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
    fn active_route_swap_preserves_a_turn_pin() {
        let table = RouteTable::new(8);
        table.set_active(route("route-a", "model-a"));
        let key = TurnKey::new("thread-1", "turn-1").unwrap();
        let first = table.resolve(Some(key.clone())).unwrap();
        assert_eq!(first.summary().id, "route-a");

        table.set_active(route("route-b", "model-b"));
        let pinned = table.resolve(Some(key.clone())).unwrap();
        let next_turn = table
            .resolve(Some(TurnKey::new("thread-1", "turn-2").unwrap()))
            .unwrap();
        assert_eq!(pinned.summary().id, "route-a");
        assert_eq!(next_turn.summary().id, "route-b");

        assert!(table.release(&key));
        assert_eq!(table.resolve(Some(key)).unwrap().summary().id, "route-b");
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
