use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, ETAG, LOCATION};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codex_provider_switcher_local_proxy::{
    BearerToken, CodexModelsResponse, LocalProxy, ModelDescriptor, ProxyHandle, ProxyHealth,
    ProxyStartOptions, RouteConfig,
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const ENTRY_TOKEN: &str = "entry-token-for-integration-tests";
const UPSTREAM_TOKEN: &str = "upstream-token-for-integration-tests";

struct TestServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl TestServer {
    async fn spawn(app: Router) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test upstream");
        let addr = listener.local_addr().expect("test upstream address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test upstream server");
        });
        Self { addr, task }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Clone)]
struct CapturedRequest {
    path: String,
    headers: HeaderMap,
    body: Value,
}

#[derive(Default)]
struct CaptureState {
    requests: Mutex<Vec<CapturedRequest>>,
}

impl CaptureState {
    fn requests(&self) -> Vec<CapturedRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

async fn capture_provider(
    State(state): State<Arc<CaptureState>>,
    request: Request<Body>,
) -> Response {
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .expect("read captured body");
    let body = serde_json::from_slice(&body).expect("captured JSON body");
    state
        .requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(CapturedRequest {
            path,
            headers,
            body,
        });

    (
        StatusCode::OK,
        [(CONTENT_TYPE, "application/json")],
        Json(json!({"ok": true})),
    )
        .into_response()
}

fn bearer(value: &str) -> BearerToken {
    BearerToken::new(value).expect("valid test bearer")
}

fn model(slug: &str) -> ModelDescriptor {
    ModelDescriptor::new(slug, format!("Display {slug}"))
}

fn route(
    server: &TestServer,
    route_id: &str,
    selected_model: &str,
    models: Vec<ModelDescriptor>,
) -> RouteConfig {
    RouteConfig::new(
        route_id,
        server.base_url(),
        selected_model,
        models,
        bearer(UPSTREAM_TOKEN),
    )
    .expect("valid local upstream route")
}

async fn proxy_with_route(route: RouteConfig) -> ProxyHandle {
    let proxy = LocalProxy::start(ProxyStartOptions::default(), bearer(ENTRY_TOKEN))
        .await
        .expect("start local proxy");
    proxy.set_active_route(route);
    proxy
}

async fn proxy_with_route_and_options(
    route: RouteConfig,
    options: ProxyStartOptions,
) -> ProxyHandle {
    let proxy = LocalProxy::start(options, bearer(ENTRY_TOKEN))
        .await
        .expect("start local proxy");
    proxy.set_active_route(route);
    proxy
}

fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .build()
        .expect("test client")
}

#[tokio::test]
async fn authenticates_sanitizes_overwrites_and_exposes_health_and_models() {
    let capture = Arc::new(CaptureState::default());
    let upstream = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(capture_provider))
            .route("/v1/responses/compact", post(capture_provider))
            .with_state(Arc::clone(&capture)),
    )
    .await;
    let proxy = proxy_with_route(route(
        &upstream,
        "route-a",
        "model-a",
        vec![model("model-a"), model("model-b")],
    ))
    .await;
    let client = no_redirect_client();

    let unauthorized = client
        .post(format!("{}/responses", proxy.base_url()))
        .json(&json!({"model": "client-model"}))
        .send()
        .await
        .expect("unauthorized response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert!(capture.requests().is_empty());

    let response = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("cookie", "client-cookie=private")
        .header("x-api-key", "client-sensitive-key")
        .header("connection", "x-remove")
        .header("x-remove", "hop-by-hop")
        .header("x-preserved", "preserved")
        .header("accept-encoding", "gzip, deflate, br")
        .header("thread-id", "thread-a")
        .json(&json!({
            "model": "client-model",
            "input": "hello",
            "client_metadata": {"turn_id": "turn-a"}
        }))
        .send()
        .await
        .expect("proxied response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-models-etag"));

    let compact = client
        .post(format!("{}/v1/responses/compact", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "thread-a")
        .json(&json!({
            "model": "another-client-model",
            "input": [],
            "client_metadata": {"turn_id": "turn-a"}
        }))
        .send()
        .await
        .expect("proxied compact response");
    assert_eq!(compact.status(), StatusCode::OK);

    let requests = capture.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/v1/responses");
    assert_eq!(requests[1].path, "/v1/responses/compact");
    for request in &requests {
        assert_eq!(
            request.headers[AUTHORIZATION],
            format!("Bearer {UPSTREAM_TOKEN}")
        );
        assert!(!request.headers.contains_key("cookie"));
        assert!(!request.headers.contains_key("x-api-key"));
        assert!(!request.headers.contains_key("connection"));
        assert!(!request.headers.contains_key("x-remove"));
        assert_eq!(request.headers["accept-encoding"], "identity");
        assert_eq!(request.body["model"], "model-a");
    }
    assert_eq!(requests[0].headers["x-preserved"], "preserved");

    let models = client
        .get(format!(
            "{}/v1/models?client_version=1.2.3",
            proxy.base_url()
        ))
        .bearer_auth(ENTRY_TOKEN)
        .send()
        .await
        .expect("models response");
    assert_eq!(models.status(), StatusCode::OK);
    let etag = models
        .headers()
        .get(ETAG)
        .expect("models ETag")
        .to_str()
        .unwrap()
        .to_string();
    let models: CodexModelsResponse = models.json().await.expect("models JSON");
    assert_eq!(
        models
            .models
            .iter()
            .map(|item| item.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["model-a", "model-b"]
    );

    let not_modified = client
        .get(format!("{}/models", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("if-none-match", etag)
        .send()
        .await
        .expect("conditional models response");
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);

    let health: ProxyHealth = client
        .get(format!("{}/health", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .send()
        .await
        .expect("health response")
        .json()
        .await
        .expect("health JSON");
    assert!(health.running);
    assert_eq!(health.listen_addr, proxy.listen_addr());
    assert_eq!(health.active_route.unwrap().selected_model, "model-a");
    assert_eq!(health.forwarded_requests, 2);
    assert_eq!(health.last_upstream_status, Some(200));

    proxy.shutdown().await.expect("stop proxy");
}

#[derive(Default)]
struct MarkerState {
    requests: AtomicUsize,
    seen_models: Mutex<Vec<String>>,
    marker: &'static str,
}

async fn marker_provider(
    State(state): State<Arc<MarkerState>>,
    request: Request<Body>,
) -> Response {
    state.requests.fetch_add(1, Ordering::AcqRel);
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .expect("read marker request");
    let body: Value = serde_json::from_slice(&body).expect("marker request JSON");
    state
        .seen_models
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(body["model"].as_str().unwrap().to_string());
    Json(json!({
        "provider": state.marker,
        "id": format!("resp_{}_conversation", state.marker)
    }))
    .into_response()
}

async fn marker_server(marker: &'static str) -> (TestServer, Arc<MarkerState>) {
    let state = Arc::new(MarkerState {
        marker,
        ..MarkerState::default()
    });
    let server = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(marker_provider))
            .route("/v1/responses/compact", post(marker_provider))
            .with_state(Arc::clone(&state)),
    )
    .await;
    (server, state)
}

#[tokio::test]
async fn hot_switches_new_threads_but_keeps_existing_conversations_on_the_old_route() {
    let (upstream_a, state_a) = marker_server("a").await;
    let (upstream_b, state_b) = marker_server("b").await;
    let proxy = proxy_with_route(route(
        &upstream_a,
        "route-a",
        "model-a",
        vec![model("model-a")],
    ))
    .await;
    let client = no_redirect_client();

    let request = |path: &str, thread_id: &str, turn_id: &str| {
        client
            .post(format!("{}{path}", proxy.base_url()))
            .bearer_auth(ENTRY_TOKEN)
            .header("thread-id", thread_id)
            .json(&json!({
                "model": "ignored-client-model",
                "client_metadata": {"turn_id": turn_id}
            }))
    };

    let first: Value = request("/responses", "thread-one", "turn-one")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["provider"], "a");

    proxy.set_active_route(route(
        &upstream_b,
        "route-b",
        "model-b",
        vec![model("model-b")],
    ));

    let same_turn: Value = request("/responses/compact", "thread-one", "turn-one")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let next_turn: Value = request("/responses", "thread-one", "turn-two")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_thread: Value = request("/responses", "thread-two", "turn-one")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(same_turn["provider"], "a");
    assert_eq!(next_turn["provider"], "a");
    assert_eq!(new_thread["provider"], "b");
    assert_eq!(state_a.requests.load(Ordering::Acquire), 3);
    assert_eq!(state_b.requests.load(Ordering::Acquire), 1);
    assert_eq!(
        *state_a
            .seen_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        vec!["model-a", "model-a", "model-a"]
    );
    assert_eq!(
        *state_b
            .seen_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        vec!["model-b"]
    );

    let catalog: CodexModelsResponse = client
        .get(format!("{}/v1/models", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let slugs: Vec<_> = catalog
        .models
        .iter()
        .map(|model| model.slug.as_str())
        .collect();
    assert!(slugs.contains(&"model-a"));
    assert!(slugs.contains(&"model-b"));

    proxy.shutdown().await.unwrap();
}

fn bindings_temp_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codex-provider-switcher-bindings-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp bindings directory");
    dir.join("bindings.json")
}

#[tokio::test]
async fn restored_bindings_keep_existing_threads_after_a_proxy_restart() {
    let (upstream_a, state_a) = marker_server("a").await;
    let (upstream_b, state_b) = marker_server("b").await;
    let bindings_path = bindings_temp_path();
    let client = no_redirect_client();
    let route_a = route(&upstream_a, "route-a", "model-a", vec![model("model-a")]);
    let route_b = route(&upstream_b, "route-b", "model-b", vec![model("model-b")]);

    let first = LocalProxy::start(
        ProxyStartOptions {
            bindings_path: Some(bindings_path.clone()),
            ..ProxyStartOptions::default()
        },
        bearer(ENTRY_TOKEN),
    )
    .await
    .expect("start first proxy");
    first.set_active_route(route_a.clone());
    let first_reply: Value = client
        .post(format!("{}/responses", first.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "thread-one")
        .json(&json!({
            "model": "ignored",
            "client_metadata": {"turn_id": "turn-one"}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first_reply["provider"], "a");
    first.shutdown().await.unwrap();

    let second = LocalProxy::start(
        ProxyStartOptions {
            bindings_path: Some(bindings_path.clone()),
            ..ProxyStartOptions::default()
        },
        bearer(ENTRY_TOKEN),
    )
    .await
    .expect("start second proxy");
    second.set_active_route(route_b);
    second.remember_route(route_a);
    let existing: Value = client
        .post(format!("{}/responses", second.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "thread-one")
        .json(&json!({
            "model": "model-b",
            "client_metadata": {"turn_id": "turn-two"}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fresh: Value = client
        .post(format!("{}/responses", second.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "thread-two")
        .json(&json!({
            "model": "model-b",
            "client_metadata": {"turn_id": "turn-one"}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(existing["provider"], "a");
    assert_eq!(fresh["provider"], "b");
    assert_eq!(state_a.requests.load(Ordering::Acquire), 2);
    assert_eq!(state_b.requests.load(Ordering::Acquire), 1);
    second.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(bindings_path.parent().expect("temp parent"));
}

#[tokio::test]
async fn previous_response_id_keeps_existing_chats_on_the_old_route() {
    let (upstream_a, state_a) = marker_server("a").await;
    let (upstream_b, state_b) = marker_server("b").await;
    let proxy = proxy_with_route(route(
        &upstream_a,
        "route-a",
        "model-a",
        vec![model("model-a")],
    ))
    .await;
    let client = no_redirect_client();
    let first: Value = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({ "model": "model-a" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["provider"], "a");
    let response_id = first["id"].as_str().expect("response id").to_string();

    proxy.set_active_route(route(
        &upstream_b,
        "route-b",
        "model-b",
        vec![model("model-b")],
    ));

    let continued: Value = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({
            "model": "model-b",
            "previous_response_id": response_id
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fresh: Value = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({ "model": "model-b" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(continued["provider"], "a");
    assert_eq!(fresh["provider"], "b");
    assert_eq!(state_a.requests.load(Ordering::Acquire), 2);
    assert_eq!(state_b.requests.load(Ordering::Acquire), 1);
    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn catalog_sibling_models_are_forwarded_instead_of_rewritten() {
    let capture = Arc::new(CaptureState::default());
    let upstream = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(capture_provider))
            .with_state(Arc::clone(&capture)),
    )
    .await;
    let proxy = proxy_with_route(route(
        &upstream,
        "luming",
        "gpt-5.6-sol",
        vec![model("grok-4.6"), model("gpt-5.6-sol")],
    ))
    .await;
    let client = no_redirect_client();

    let existing = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "hello-thread")
        .json(&json!({
            "model": "grok-4.6",
            "input": "hello",
            "client_metadata": {"turn_id": "turn-one"}
        }))
        .send()
        .await
        .expect("existing chat response");
    assert_eq!(existing.status(), StatusCode::OK);

    let switched = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "new-thread")
        .json(&json!({
            "model": "gpt-5.6-sol",
            "input": "new",
            "client_metadata": {"turn_id": "turn-one"}
        }))
        .send()
        .await
        .expect("switched chat response");
    assert_eq!(switched.status(), StatusCode::OK);

    let unknown = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "unknown-thread")
        .json(&json!({
            "model": "client-model",
            "input": "unknown",
            "client_metadata": {"turn_id": "turn-one"}
        }))
        .send()
        .await
        .expect("unknown model response");
    assert_eq!(unknown.status(), StatusCode::OK);

    let requests = capture.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].body["model"], "grok-4.6");
    assert_eq!(requests[1].body["model"], "gpt-5.6-sol");
    assert_eq!(requests[2].body["model"], "gpt-5.6-sol");

    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn switching_selected_model_does_not_rewrite_an_existing_catalog_thread() {
    let capture = Arc::new(CaptureState::default());
    let upstream = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(capture_provider))
            .with_state(Arc::clone(&capture)),
    )
    .await;
    let proxy = proxy_with_route(route(
        &upstream,
        "luming",
        "grok-4.6",
        vec![model("grok-4.6"), model("gpt-5.6-sol")],
    ))
    .await;
    let client = no_redirect_client();

    let first = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "hello-thread")
        .json(&json!({
            "model": "grok-4.6",
            "client_metadata": {"turn_id": "turn-one"}
        }))
        .send()
        .await
        .expect("first grok turn");
    assert_eq!(first.status(), StatusCode::OK);

    proxy.set_active_route(route(
        &upstream,
        "luming",
        "gpt-5.6-sol",
        vec![model("grok-4.6"), model("gpt-5.6-sol")],
    ));

    let continued = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "hello-thread")
        .json(&json!({
            "model": "grok-4.6",
            "client_metadata": {"turn_id": "turn-two"}
        }))
        .send()
        .await
        .expect("continued grok turn");
    assert_eq!(continued.status(), StatusCode::OK);

    let fresh = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "other-thread")
        .json(&json!({
            "model": "gpt-5.6-sol",
            "client_metadata": {"turn_id": "turn-one"}
        }))
        .send()
        .await
        .expect("fresh gpt turn");
    assert_eq!(fresh.status(), StatusCode::OK);

    let requests = capture.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].body["model"], "grok-4.6");
    assert_eq!(requests[1].body["model"], "grok-4.6");
    assert_eq!(requests[2].body["model"], "gpt-5.6-sol");

    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_turn_does_not_pin_the_thread_to_the_failing_route() {
    let failing =
        TestServer::spawn(Router::new().route("/v1/responses", post(json_unavailable))).await;
    let (ok, state_ok) = marker_server("ok").await;
    let proxy = proxy_with_route_and_options(
        route(&failing, "route-fail", "model-a", vec![model("model-a")]),
        ProxyStartOptions {
            upstream_max_retries: 0,
            ..ProxyStartOptions::default()
        },
    )
    .await;
    let client = no_redirect_client();

    let first = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "hello-thread")
        .json(&json!({
            "model": "model-a",
            "client_metadata": {"turn_id": "turn-one"}
        }))
        .send()
        .await
        .expect("failing turn");
    assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);

    proxy.set_active_route(route(&ok, "route-ok", "model-b", vec![model("model-b")]));

    let retry: Value = client
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "hello-thread")
        .json(&json!({
            "model": "model-b",
            "client_metadata": {"turn_id": "turn-two"}
        }))
        .send()
        .await
        .expect("retry after failure")
        .json()
        .await
        .expect("retry JSON");
    assert_eq!(retry["provider"], "ok");
    assert_eq!(state_ok.requests.load(Ordering::Acquire), 1);

    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn strips_continuation_ids_when_outbound_model_would_change() {
    let capture = Arc::new(CaptureState::default());
    let upstream = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(capture_provider))
            .with_state(Arc::clone(&capture)),
    )
    .await;
    let proxy = proxy_with_route(route(
        &upstream,
        "luming",
        "gpt-5.6-sol",
        vec![model("grok-4.6"), model("gpt-5.6-sol")],
    ))
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .header("thread-id", "hello-thread")
        .json(&json!({
            "model": "grok-4.6",
            "previous_response_id": "resp_oldchat1",
            "input": "hello"
        }))
        .send()
        .await
        .expect("stripped continuation response");
    assert_eq!(response.status(), StatusCode::OK);

    let requests = capture.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body["model"], "grok-4.6");
    assert!(requests[0].body.get("previous_response_id").is_none());

    proxy.shutdown().await.unwrap();
}

async fn sse_provider() -> Response {
    let chunks = stream::unfold(0_u8, |index| async move {
        match index {
            0 => Some((
                Ok::<Bytes, Infallible>(Bytes::from_static(b"data: first\n\n")),
                1,
            )),
            1 => {
                sleep(Duration::from_millis(250)).await;
                Some((
                    Ok::<Bytes, Infallible>(Bytes::from_static(b"data: second\n\n")),
                    2,
                ))
            }
            _ => None,
        }
    });
    let mut response = Response::new(Body::from_stream(chunks));
    response.headers_mut().insert(
        CONTENT_TYPE,
        "text/event-stream".parse().expect("SSE content type"),
    );
    response
}

#[tokio::test]
async fn forwards_sse_as_an_unbuffered_byte_stream() {
    let upstream =
        TestServer::spawn(Router::new().route("/v1/responses", post(sse_provider))).await;
    let proxy = proxy_with_route(route(
        &upstream,
        "route-sse",
        "model-sse",
        vec![model("model-sse")],
    ))
    .await;
    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored"}))
        .send()
        .await
        .expect("SSE response");
    assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");

    let mut stream = response.bytes_stream();
    let first = timeout(Duration::from_millis(150), stream.next())
        .await
        .expect("first SSE chunk arrived before the second was produced")
        .expect("first SSE item")
        .expect("first SSE bytes");
    assert_eq!(first.as_ref(), b"data: first\n\n");

    let mut all = first.to_vec();
    while let Some(chunk) = stream.next().await {
        all.extend_from_slice(&chunk.expect("remaining SSE bytes"));
    }
    assert_eq!(all, b"data: first\n\ndata: second\n\n");
    let log = &proxy.request_logs()[0];
    assert_eq!(log.retry_count, 0);
    assert_eq!(log.response_bytes, all.len() as u64);
    assert!(log.first_byte_ms.is_some());
    assert!(log.stream_duration_ms.is_some());
    assert!(log.stream_completed);
    assert!(log.stream_error.is_none());

    proxy.shutdown().await.unwrap();
}

#[derive(Default)]
struct MidStreamFailureState {
    hits: AtomicUsize,
}

async fn mid_stream_failure_provider(State(state): State<Arc<MidStreamFailureState>>) -> Response {
    state.hits.fetch_add(1, Ordering::AcqRel);
    let chunks = stream::unfold(0_u8, |index| async move {
        match index {
            0 => Some((
                Ok::<Bytes, io::Error>(Bytes::from_static(b"data: partial\n\n")),
                1,
            )),
            1 => {
                sleep(Duration::from_millis(50)).await;
                Some((Err(io::Error::other("simulated upstream reset")), 2))
            }
            _ => None,
        }
    });
    let mut response = Response::new(Body::from_stream(chunks));
    response.headers_mut().insert(
        CONTENT_TYPE,
        "text/event-stream".parse().expect("SSE content type"),
    );
    response
}

#[tokio::test]
async fn records_mid_stream_failure_without_replaying_request() {
    let state = Arc::new(MidStreamFailureState::default());
    let upstream = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(mid_stream_failure_provider))
            .with_state(Arc::clone(&state)),
    )
    .await;
    let proxy = proxy_with_route(route(
        &upstream,
        "route-stream-failure",
        "model-stream-failure",
        vec![model("model-stream-failure")],
    ))
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored", "stream": true}))
        .send()
        .await
        .expect("stream failure response");
    let mut body = response.bytes_stream();
    let first = body
        .next()
        .await
        .expect("partial stream item")
        .expect("partial stream bytes");
    assert_eq!(first.as_ref(), b"data: partial\n\n");
    assert!(body.next().await.expect("stream error item").is_err());
    assert_eq!(state.hits.load(Ordering::Acquire), 1);

    let log = &proxy.request_logs()[0];
    assert!(!log.stream_completed);
    assert!(log.stream_error.is_some());
    assert_eq!(log.response_bytes, first.len() as u64);
    assert!(log.stream_duration_ms.is_some());

    proxy.shutdown().await.unwrap();
}

#[derive(Default)]
struct RedirectState {
    source_hits: AtomicUsize,
    target_hits: AtomicUsize,
}

async fn redirect_source(State(state): State<Arc<RedirectState>>) -> Redirect {
    state.source_hits.fetch_add(1, Ordering::AcqRel);
    Redirect::temporary("/v1/redirect-target")
}

async fn redirect_target(State(state): State<Arc<RedirectState>>) -> StatusCode {
    state.target_hits.fetch_add(1, Ordering::AcqRel);
    StatusCode::OK
}

#[tokio::test]
async fn never_follows_upstream_redirects() {
    let state = Arc::new(RedirectState::default());
    let upstream = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(redirect_source))
            .route("/v1/redirect-target", get(redirect_target))
            .with_state(Arc::clone(&state)),
    )
    .await;
    let proxy = proxy_with_route(route(
        &upstream,
        "route-redirect",
        "model-redirect",
        vec![model("model-redirect")],
    ))
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored"}))
        .send()
        .await
        .expect("redirect response");
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(response.headers()[LOCATION], "/v1/redirect-target");
    assert_eq!(state.source_hits.load(Ordering::Acquire), 1);
    assert_eq!(state.target_hits.load(Ordering::Acquire), 0);

    proxy.shutdown().await.unwrap();
}

#[derive(Default)]
struct FailureState {
    hits: AtomicUsize,
}

async fn failing_provider(State(state): State<Arc<FailureState>>) -> StatusCode {
    state.hits.fetch_add(1, Ordering::AcqRel);
    StatusCode::INTERNAL_SERVER_ERROR
}

#[tokio::test]
async fn does_not_retry_non_transient_upstream_statuses() {
    let state = Arc::new(FailureState::default());
    let upstream = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(failing_provider))
            .with_state(Arc::clone(&state)),
    )
    .await;
    let proxy = proxy_with_route_and_options(
        route(
            &upstream,
            "route-failure",
            "model-failure",
            vec![model("model-failure")],
        ),
        ProxyStartOptions {
            upstream_max_retries: 2,
            upstream_retry_base_delay: Duration::from_millis(1),
            upstream_retry_max_delay: Duration::from_millis(1),
            upstream_retry_max_elapsed: Duration::from_secs(1),
            ..ProxyStartOptions::default()
        },
    )
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored"}))
        .send()
        .await
        .expect("failure response");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(state.hits.load(Ordering::Acquire), 1);

    proxy.shutdown().await.unwrap();
}

#[derive(Default)]
struct TransientState {
    hits: AtomicUsize,
}

async fn transient_then_ok(State(state): State<Arc<TransientState>>) -> Response {
    let hit = state.hits.fetch_add(1, Ordering::AcqRel);
    if hit < 2 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": {"message": "temporary outage"}})),
        )
            .into_response();
    }
    Json(json!({"ok": true})).into_response()
}

#[tokio::test]
async fn retries_transient_upstream_statuses_until_success() {
    let state = Arc::new(TransientState::default());
    let upstream = TestServer::spawn(
        Router::new()
            .route("/v1/responses", post(transient_then_ok))
            .with_state(Arc::clone(&state)),
    )
    .await;
    let proxy = proxy_with_route_and_options(
        route(
            &upstream,
            "route-transient",
            "model-transient",
            vec![model("model-transient")],
        ),
        ProxyStartOptions {
            upstream_max_retries: 3,
            upstream_retry_base_delay: Duration::from_millis(1),
            upstream_retry_max_delay: Duration::from_millis(1),
            upstream_retry_max_elapsed: Duration::from_secs(1),
            ..ProxyStartOptions::default()
        },
    )
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored"}))
        .send()
        .await
        .expect("eventual success response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(state.hits.load(Ordering::Acquire), 3);
    assert_eq!(proxy.request_logs()[0].retry_count, 2);

    proxy.shutdown().await.unwrap();
}

async fn html_bad_gateway() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        "<html><body>Bad Gateway</body></html>",
    )
        .into_response()
}

async fn cloudflare_forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        "<html><head><title>Attention Required! | Cloudflare</title></head><body>Sorry, you have been blocked</body></html>",
    )
        .into_response()
}

async fn json_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "message": "Service temporarily unavailable",
                "type": "api_error"
            }
        })),
    )
        .into_response()
}

fn error_message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn html_upstream_errors_are_normalized_to_json() {
    let upstream =
        TestServer::spawn(Router::new().route("/v1/responses", post(html_bad_gateway))).await;
    let proxy = proxy_with_route(route(
        &upstream,
        "route-html",
        "model-html",
        vec![model("model-html")],
    ))
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored"}))
        .send()
        .await
        .expect("html error response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body: Value = response.json().await.expect("json error body");
    assert_eq!(body["error"]["type"], "api_error");
    assert_eq!(
        error_message(&body),
        "upstream provider returned 502 Bad Gateway"
    );

    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn cloudflare_html_blocks_are_normalized_to_json() {
    let upstream =
        TestServer::spawn(Router::new().route("/v1/responses", post(cloudflare_forbidden))).await;
    let proxy = proxy_with_route(route(
        &upstream,
        "route-cf",
        "model-cf",
        vec![model("model-cf")],
    ))
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored"}))
        .send()
        .await
        .expect("cloudflare error response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = response.json().await.expect("json error body");
    assert_eq!(body["error"]["type"], "api_error");
    assert_eq!(
        error_message(&body),
        "Cloudflare blocked the active provider (403 Forbidden)"
    );

    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn json_upstream_error_messages_are_preserved() {
    let upstream =
        TestServer::spawn(Router::new().route("/v1/responses", post(json_unavailable))).await;
    let proxy = proxy_with_route(route(
        &upstream,
        "route-json-error",
        "model-json-error",
        vec![model("model-json-error")],
    ))
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored"}))
        .send()
        .await
        .expect("json error response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("json error body");
    assert_eq!(error_message(&body), "Service temporarily unavailable");
    assert_eq!(body["error"]["type"], "api_error");

    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn unreachable_upstream_returns_a_json_bad_gateway() {
    let proxy = proxy_with_route(
        RouteConfig::single_model(
            "route-dead",
            "http://127.0.0.1:1/v1",
            model("model-dead"),
            bearer(UPSTREAM_TOKEN),
        )
        .expect("loopback dead route"),
    )
    .await;

    let response = no_redirect_client()
        .post(format!("{}/responses", proxy.base_url()))
        .bearer_auth(ENTRY_TOKEN)
        .json(&json!({"model": "ignored"}))
        .send()
        .await
        .expect("dead upstream response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body: Value = response.json().await.expect("json error body");
    assert_eq!(body["error"]["type"], "api_error");
    assert!(
        error_message(&body).starts_with("the active provider"),
        "unexpected gateway message: {}",
        error_message(&body)
    );

    proxy.shutdown().await.unwrap();
}
