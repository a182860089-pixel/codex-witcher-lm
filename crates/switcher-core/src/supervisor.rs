use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::SinkExt;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use tokio::time::timeout;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::cdp::CdpTarget;
use crate::cdp::validate_browser_websocket_url;
use crate::cdp::validate_page_target;
use crate::cdp::validate_page_target_for_cleanup;
use crate::error::Result;
use crate::error::SwitcherError;

const MAX_CDP_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_INJECTION_BYTES: usize = 512 * 1024;
const CDP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_token: u64,
    pub executable: PathBuf,
    pub app_version: String,
    pub platform_identity: String,
}

pub trait ListenerIdentityVerifier: Send + Sync {
    fn verify_listener(&self, port: u16) -> Result<ProcessIdentity>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InjectedScript {
    pub target_id: String,
    pub identifier: String,
    pub cleanup_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserVersion {
    web_socket_debugger_url: String,
}

pub struct CdpSupervisor<V: ListenerIdentityVerifier> {
    port: u16,
    identity: ProcessIdentity,
    browser_id: String,
    verifier: V,
    client: Client,
    browser_anchor: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}

impl<V: ListenerIdentityVerifier> CdpSupervisor<V> {
    pub async fn attach(port: u16, verifier: V) -> Result<Self> {
        let identity = verifier.verify_listener(port)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(CDP_TIMEOUT)
            .timeout(CDP_TIMEOUT)
            .build()
            .map_err(|_| supervisor_error("could not construct loopback HTTP client"))?;
        let version: BrowserVersion = get_json(&client, port, "/json/version").await?;
        let browser_id = validate_browser_websocket_url(&version.web_socket_debugger_url, port)?;
        let (browser_anchor, _) =
            timeout(CDP_TIMEOUT, connect_async(&version.web_socket_debugger_url))
                .await
                .map_err(|_| supervisor_error("browser identity anchor timed out"))?
                .map_err(|_| supervisor_error("browser identity anchor failed"))?;

        let verified_again = verifier.verify_listener(port)?;
        if identity != verified_again {
            return Err(supervisor_error(
                "listener identity changed during CDP attachment",
            ));
        }

        Ok(Self {
            port,
            identity,
            browser_id,
            verifier,
            client,
            browser_anchor,
        })
    }

    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }

    pub fn browser_id(&self) -> &str {
        &self.browser_id
    }

    pub async fn verified_targets(&mut self) -> Result<Vec<CdpTarget>> {
        self.ensure_anchor_open()?;
        self.verify_platform_identity()?;
        let version: BrowserVersion = get_json(&self.client, self.port, "/json/version").await?;
        let browser_id =
            validate_browser_websocket_url(&version.web_socket_debugger_url, self.port)?;
        if browser_id != self.browser_id {
            return Err(supervisor_error("CDP Browser ID changed"));
        }

        let targets: Vec<CdpTarget> = get_json(&self.client, self.port, "/json/list").await?;
        let verified = targets
            .into_iter()
            .filter(|target| validate_page_target(target, self.port).is_ok())
            .collect::<Vec<_>>();
        self.verify_platform_identity()?;
        self.ensure_anchor_open()?;
        Ok(verified)
    }

    pub async fn install_reviewed_script(
        &mut self,
        target: &CdpTarget,
        script: &str,
    ) -> Result<InjectedScript> {
        if script.is_empty() || script.len() > MAX_INJECTION_BYTES {
            return Err(SwitcherError::Validation(
                "reviewed injection payload has an invalid size".to_string(),
            ));
        }
        self.verify_target_is_current(target).await?;
        let cleanup_key = format!(
            "__codex_provider_switcher_cleanup_{}",
            uuid::Uuid::new_v4().simple()
        );
        let guarded_script = guarded_adapter_script(script, &cleanup_key)?;
        let (mut socket, _) = timeout(CDP_TIMEOUT, connect_async(&target.web_socket_debugger_url))
            .await
            .map_err(|_| supervisor_error("page connection timed out"))?
            .map_err(|_| supervisor_error("page connection failed"))?;

        let add_result = send_command(
            &mut socket,
            1,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": guarded_script }),
        )
        .await?;
        let identifier = add_result
            .get("identifier")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or_else(|| supervisor_error("CDP did not return an early-script identifier"))?
            .to_string();
        let evaluate_result = send_command(
            &mut socket,
            2,
            "Runtime.evaluate",
            json!({
                "expression": guarded_script,
                "awaitPromise": true,
                "returnByValue": true
            }),
        )
        .await;
        if let Err(error) =
            evaluate_result.and_then(|result| ensure_runtime_status(&result, "installed"))
        {
            let rollback = rollback_script(&mut socket, 3, &identifier, &cleanup_key).await;
            return Err(error_after_rollback(error, rollback));
        }
        if let Err(error) = self.verify_target_is_current(target).await {
            let rollback = rollback_script(&mut socket, 3, &identifier, &cleanup_key).await;
            return Err(error_after_rollback(error, rollback));
        }
        if let Err(error) = self.verify_platform_identity() {
            let rollback = rollback_script(&mut socket, 3, &identifier, &cleanup_key).await;
            return Err(error_after_rollback(error, rollback));
        }
        if let Err(error) = self.ensure_anchor_open() {
            let rollback = rollback_script(&mut socket, 3, &identifier, &cleanup_key).await;
            return Err(error_after_rollback(error, rollback));
        }
        Ok(InjectedScript {
            target_id: target.id.clone(),
            identifier,
            cleanup_key,
        })
    }

    pub async fn remove_reviewed_script(
        &mut self,
        target: &CdpTarget,
        injected: &InjectedScript,
    ) -> Result<()> {
        if target.id != injected.target_id {
            return Err(SwitcherError::Validation(
                "injected script belongs to a different CDP target".to_string(),
            ));
        }
        if self.verify_cleanup_target_is_current(target).await.is_err() {
            return Err(supervisor_error(
                "adapter cleanup target could not be confirmed; restart Codex",
            ));
        }
        let (mut socket, _) = timeout(CDP_TIMEOUT, connect_async(&target.web_socket_debugger_url))
            .await
            .map_err(|_| supervisor_error("page connection timed out"))?
            .map_err(|_| supervisor_error("page connection failed"))?;
        rollback_script(&mut socket, 1, &injected.identifier, &injected.cleanup_key).await?;
        self.verify_platform_identity()?;
        self.ensure_anchor_open()
    }

    async fn verify_target_is_current(&mut self, target: &CdpTarget) -> Result<()> {
        validate_page_target(target, self.port)?;
        let targets = self.verified_targets().await?;
        if !targets.iter().any(|candidate| candidate == target) {
            return Err(supervisor_error("CDP target changed after it was selected"));
        }
        Ok(())
    }

    async fn verify_cleanup_target_is_current(&mut self, target: &CdpTarget) -> Result<()> {
        validate_page_target_for_cleanup(target, self.port)?;
        self.ensure_anchor_open()?;
        self.verify_platform_identity()?;
        let version: BrowserVersion = get_json(&self.client, self.port, "/json/version").await?;
        let browser_id =
            validate_browser_websocket_url(&version.web_socket_debugger_url, self.port)?;
        if browser_id != self.browser_id {
            return Err(supervisor_error("CDP Browser ID changed"));
        }
        let targets: Vec<CdpTarget> = get_json(&self.client, self.port, "/json/list").await?;
        let current = targets
            .iter()
            .find(|candidate| {
                candidate.id == target.id
                    && candidate.web_socket_debugger_url == target.web_socket_debugger_url
            })
            .ok_or_else(|| supervisor_error("CDP cleanup target changed"))?;
        validate_page_target_for_cleanup(current, self.port)?;
        self.verify_platform_identity()?;
        self.ensure_anchor_open()
    }

    fn verify_platform_identity(&self) -> Result<()> {
        let current = self.verifier.verify_listener(self.port)?;
        if current != self.identity {
            return Err(supervisor_error("CDP listener owner identity changed"));
        }
        Ok(())
    }

    fn ensure_anchor_open(&mut self) -> Result<()> {
        if self.browser_anchor.get_ref().get_ref().peer_addr().is_err() {
            return Err(supervisor_error("browser identity anchor is closed"));
        }
        match self.browser_anchor.next().now_or_never() {
            None | Some(Some(Ok(Message::Text(_) | Message::Binary(_) | Message::Ping(_)))) => {
                Ok(())
            }
            Some(Some(Ok(Message::Pong(_) | Message::Frame(_)))) => Ok(()),
            Some(Some(Ok(Message::Close(_)))) | Some(None) | Some(Some(Err(_))) => {
                Err(supervisor_error("browser identity anchor is closed"))
            }
        }
    }
}

fn guarded_adapter_script(adapter_source: &str, cleanup_key: &str) -> Result<String> {
    if !cleanup_key
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(SwitcherError::Validation(
            "invalid reviewed adapter cleanup key".to_string(),
        ));
    }
    let cleanup_key_json = serde_json::to_string(cleanup_key)?;
    let adapter_source_json = serde_json::to_string(adapter_source)?;
    Ok(format!(
        r#"(() => {{
  "use strict";
  if (
    globalThis.top !== globalThis ||
    globalThis.location.protocol !== "app:" ||
    globalThis.location.host !== "codex" ||
    globalThis.location.search !== "" ||
    globalThis.location.hash !== ""
  ) {{
    return {{ status: "skipped" }};
  }}
  const cleanupKey = {cleanup_key_json};
  const previousCleanup = globalThis[cleanupKey];
  if (typeof previousCleanup === "function") {{
    try {{ previousCleanup(); }} finally {{ delete globalThis[cleanupKey]; }}
  }}
  const adapterSource = {adapter_source_json};
  const adapter = (0, eval)(`(${{adapterSource}}\n)`);
  if (
    adapter === null ||
    typeof adapter !== "object" ||
    typeof adapter.apply !== "function" ||
    typeof adapter.cleanup !== "function"
  ) {{
    throw new TypeError("reviewed adapter must expose synchronous apply and cleanup functions");
  }}
  Object.defineProperty(globalThis, cleanupKey, {{
    value: () => {{
      const cleanupResult = adapter.cleanup();
      if (
        cleanupResult !== null &&
        (typeof cleanupResult === "object" || typeof cleanupResult === "function") &&
        typeof cleanupResult.then === "function"
      ) {{
        throw new TypeError("reviewed adapter cleanup must be synchronous");
      }}
      return cleanupResult;
    }},
    configurable: true,
    enumerable: false,
    writable: false
  }});
  try {{
    const applyResult = adapter.apply();
    if (
      applyResult !== null &&
      (typeof applyResult === "object" || typeof applyResult === "function") &&
      typeof applyResult.then === "function"
    ) {{
      throw new TypeError("reviewed adapter apply must be synchronous");
    }}
  }} catch (error) {{
    try {{
      const cleanupResult = adapter.cleanup();
      if (
        cleanupResult !== null &&
        (typeof cleanupResult === "object" || typeof cleanupResult === "function") &&
        typeof cleanupResult.then === "function"
      ) {{
        throw new TypeError("reviewed adapter cleanup must be synchronous");
      }}
    }} finally {{
      delete globalThis[cleanupKey];
    }}
    throw error;
  }}
  return {{ status: "installed" }};
}})()"#
    ))
}

async fn cleanup_current_document(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    id: u64,
    cleanup_key: &str,
) -> Result<()> {
    let cleanup_key_json = serde_json::to_string(cleanup_key)?;
    let expression = format!(
        r#"(() => {{
  if (
    globalThis.top !== globalThis ||
    globalThis.location.protocol !== "app:" ||
    globalThis.location.host !== "codex"
  ) {{
    return {{ status: "skipped" }};
  }}
  const cleanupKey = {cleanup_key_json};
  const cleanup = globalThis[cleanupKey];
  if (typeof cleanup !== "function") return {{ status: "absent" }};
  try {{
    const cleanupResult = cleanup();
    if (
      cleanupResult !== null &&
      (typeof cleanupResult === "object" || typeof cleanupResult === "function") &&
      typeof cleanupResult.then === "function"
    ) {{
      throw new TypeError("reviewed adapter cleanup must be synchronous");
    }}
    return {{ status: "removed" }};
  }} finally {{
    delete globalThis[cleanupKey];
  }}
}})()"#
    );
    let result = send_command(
        socket,
        id,
        "Runtime.evaluate",
        json!({
            "expression": expression,
            "awaitPromise": true,
            "returnByValue": true
        }),
    )
    .await?;
    ensure_runtime_status(&result, "removed")
}

async fn rollback_script(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    first_id: u64,
    identifier: &str,
    cleanup_key: &str,
) -> Result<()> {
    let remove_result = remove_early_script(socket, first_id, identifier).await;
    let cleanup_result = cleanup_current_document(socket, first_id + 1, cleanup_key).await;
    match (remove_result, cleanup_result) {
        (Ok(_), Ok(())) => Ok(()),
        _ => Err(supervisor_error(
            "adapter hook or current-document cleanup could not be confirmed; restart Codex",
        )),
    }
}

fn error_after_rollback(primary: SwitcherError, rollback: Result<()>) -> SwitcherError {
    match rollback {
        Ok(()) => primary,
        Err(error) => error,
    }
}

fn ensure_runtime_status(result: &Value, expected: &str) -> Result<()> {
    ensure_runtime_status_any(result, &[expected])
}

fn ensure_runtime_status_any(result: &Value, expected: &[&str]) -> Result<()> {
    if result.get("exceptionDetails").is_some() {
        return Err(supervisor_error("reviewed adapter raised an exception"));
    }
    let status = result
        .pointer("/result/value/status")
        .and_then(Value::as_str)
        .ok_or_else(|| supervisor_error("reviewed adapter returned an invalid result"))?;
    if !expected.contains(&status) {
        return Err(supervisor_error(
            "reviewed adapter did not reach the required state",
        ));
    }
    Ok(())
}

async fn remove_early_script(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    id: u64,
    identifier: &str,
) -> Result<Value> {
    send_command(
        socket,
        id,
        "Page.removeScriptToEvaluateOnNewDocument",
        json!({ "identifier": identifier }),
    )
    .await
}

pub fn allocate_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

async fn get_json<T: DeserializeOwned>(client: &Client, port: u16, path: &str) -> Result<T> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| supervisor_error("loopback CDP request failed"))?;
    if !response.status().is_success() {
        return Err(supervisor_error(
            "loopback CDP endpoint returned an error status",
        ));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| supervisor_error("could not read CDP response"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_CDP_RESPONSE_BYTES {
            return Err(supervisor_error("CDP response exceeded the size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| supervisor_error("CDP response was not valid JSON"))
}

async fn send_command(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value> {
    let payload = serde_json::to_string(&json!({
        "id": id,
        "method": method,
        "params": params
    }))?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|_| supervisor_error("could not send CDP command"))?;

    timeout(CDP_TIMEOUT, async {
        while let Some(message) = socket.next().await {
            let message = message.map_err(|_| supervisor_error("CDP page connection failed"))?;
            if let Message::Text(text) = message {
                let response: Value = serde_json::from_str(text.as_str())
                    .map_err(|_| supervisor_error("invalid CDP command response"))?;
                if response.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if response.get("error").is_some() {
                    return Err(supervisor_error("CDP command was rejected"));
                }
                return response
                    .get("result")
                    .cloned()
                    .ok_or_else(|| supervisor_error("CDP command result is missing"));
            }
        }
        Err(supervisor_error("CDP page connection closed"))
    })
    .await
    .map_err(|_| supervisor_error("CDP command timed out"))?
}

fn supervisor_error(message: &str) -> SwitcherError {
    SwitcherError::Supervisor(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_an_ephemeral_loopback_port() {
        let port = allocate_loopback_port().unwrap();
        assert_ne!(port, 0);
        TcpListener::bind(("127.0.0.1", port)).unwrap();
    }

    #[test]
    fn wraps_reviewed_adapter_with_origin_and_cleanup_guards() {
        let source = guarded_adapter_script(
            "({ apply() {}, cleanup() {} })",
            "__codex_provider_switcher_cleanup_test",
        )
        .unwrap();
        assert!(source.contains("globalThis.top !== globalThis"));
        assert!(source.contains("globalThis.location.protocol !== \"app:\""));
        assert!(source.contains("adapter.cleanup()"));
        assert!(source.contains("status: \"installed\""));
    }

    #[test]
    fn rejects_runtime_exception_details() {
        let result = json!({
            "result": { "type": "object", "value": { "status": "installed" } },
            "exceptionDetails": { "text": "boom" }
        });
        assert!(ensure_runtime_status(&result, "installed").is_err());
    }
}
