use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::process::Child;
use std::process::ChildStdin;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use std::time::Instant;

use codex_provider_switcher_core::CodexAccountStatus;
use codex_provider_switcher_core::CodexAuthMode;
use codex_provider_switcher_core::CodexJsonLineError;
use codex_provider_switcher_core::parse_codex_account_result;
use codex_provider_switcher_core::parse_codex_account_updated;
use codex_provider_switcher_core::read_codex_json_line;
use codex_provider_switcher_launcher::codex_cli_path;
use codex_provider_switcher_launcher::open_login_url;
use serde_json::Value;
use serde_json::json;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(12);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_MESSAGES: usize = 16;
const CLIENT_NAME: &str = "codex_provider_switcher";
const CLIENT_TITLE: &str = "Codex Provider Switcher";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

struct AppServer {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<Result<Value, String>>,
}

impl AppServer {
    fn spawn(codex_home: &Path) -> Result<Self, String> {
        let executable = codex_cli_path()?;
        let mut command = Command::new(executable);
        command
            .args(["app-server", "--stdio"])
            .env("CODEX_HOME", codex_home)
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("CODEX_APP_SERVER_LOGIN_CLIENT_ID")
            .env_remove("CODEX_APP_SERVER_LOGIN_ISSUER")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;

            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .map_err(|_| "could not start the official Codex App Server".to_string())?;
        let Some(stdin) = child.stdin.take() else {
            terminate_child(&mut child);
            return Err("Codex App Server input is unavailable".to_string());
        };
        let Some(stdout) = child.stdout.take() else {
            drop(stdin);
            terminate_child(&mut child);
            return Err("Codex App Server output is unavailable".to_string());
        };
        let (sender, messages) = mpsc::sync_channel(MAX_QUEUED_MESSAGES);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let message = match read_codex_json_line(&mut reader, MAX_MESSAGE_BYTES) {
                    Ok(None) => {
                        let _ = sender.try_send(Err(
                            "Codex App Server stopped before completing the request".to_string(),
                        ));
                        break;
                    }
                    Ok(Some(message)) => Ok(message),
                    Err(error) => {
                        let _ = sender.try_send(Err(reader_error_message(error)));
                        break;
                    }
                };
                if sender.try_send(message).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            stdin: Some(stdin),
            messages,
        };
        server.write(json!({
            "method": "initialize",
            "id": 0,
            "params": {
                "clientInfo": {
                    "name": CLIENT_NAME,
                    "title": CLIENT_TITLE,
                    "version": CLIENT_VERSION
                }
            }
        }))?;
        server.wait_for_response(0, RESPONSE_TIMEOUT)?;
        server.write(json!({"method": "initialized"}))?;
        Ok(server)
    }

    fn write(&mut self, message: Value) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "Codex App Server input is unavailable".to_string())?;
        serde_json::to_writer(&mut *stdin, &message)
            .map_err(|_| "could not write to Codex App Server".to_string())?;
        stdin
            .write_all(b"\n")
            .and_then(|_| stdin.flush())
            .map_err(|_| "could not write to Codex App Server".to_string())
    }

    fn wait_for_response(&self, id: i64, timeout: Duration) -> Result<Value, String> {
        self.wait_for_message(timeout, |message| {
            (message.get("id").and_then(Value::as_i64) == Some(id)).then_some(message)
        })
        .and_then(response_result)
    }

    fn wait_for_message<F>(&self, timeout: Duration, mut select: F) -> Result<Value, String>
    where
        F: FnMut(Value) -> Option<Value>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| "Codex App Server request timed out".to_string())?;
            let message =
                self.messages
                    .recv_timeout(remaining)
                    .map_err(|error| match error {
                        mpsc::RecvTimeoutError::Timeout => {
                            "Codex App Server request timed out".to_string()
                        }
                        mpsc::RecvTimeoutError::Disconnected => {
                            "Codex App Server stopped before completing the request".to_string()
                        }
                    })??;
            if let Some(selected) = select(message) {
                return Ok(selected);
            }
        }
    }

    fn read_account(&mut self, request_id: i64) -> Result<CodexAccountStatus, String> {
        self.write(json!({
            "method": "account/read",
            "id": request_id,
            "params": {"refreshToken": false}
        }))?;
        let result = self.wait_for_response(request_id, RESPONSE_TIMEOUT)?;
        parse_codex_account_result(result, codex_access_token_environment_present())
            .map_err(|_| "Codex returned an unsupported account response".to_string())
    }

    fn wait_for_account_updated(&self, timeout: Duration) -> Result<CodexAuthMode, String> {
        let params = self.wait_for_message(timeout, |message| {
            (message.get("method").and_then(Value::as_str) == Some("account/updated"))
                .then(|| message.get("params").cloned())
                .flatten()
        })?;
        parse_codex_account_updated(params)
            .map_err(|_| "Codex returned an unsupported account update".to_string())
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        self.stdin.take();
        terminate_child(&mut self.child);
    }
}

pub fn read_account(codex_home: &Path) -> Result<CodexAccountStatus, String> {
    AppServer::spawn(codex_home)?.read_account(1)
}

pub fn login_chatgpt(codex_home: &Path) -> Result<CodexAccountStatus, String> {
    let mut server = AppServer::spawn(codex_home)?;
    server.write(json!({
        "method": "account/login/start",
        "id": 1,
        "params": {"type": "chatgpt"}
    }))?;
    let login = server.wait_for_response(1, RESPONSE_TIMEOUT)?;
    let login_id = login
        .get("loginId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .ok_or_else(|| "Codex did not return a login identifier".to_string())?
        .to_string();
    let auth_url = login
        .get("authUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| "Codex did not return a login URL".to_string())?;
    open_login_url(auth_url)?;

    let completion = server.wait_for_message(LOGIN_TIMEOUT, |message| {
        if message.get("method").and_then(Value::as_str) != Some("account/login/completed") {
            return None;
        }
        let params = message.get("params")?;
        (params.get("loginId").and_then(Value::as_str) == Some(login_id.as_str()))
            .then_some(params.clone())
    })?;
    if completion.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(completion
            .get("error")
            .and_then(Value::as_str)
            .map(safe_error_message)
            .unwrap_or_else(|| "Codex official login was not completed".to_string()));
    }
    if server.wait_for_account_updated(RESPONSE_TIMEOUT)? != CodexAuthMode::Chatgpt {
        return Err("Codex login completed without activating ChatGPT authentication".to_string());
    }
    let account = server.read_account(2)?;
    if account.auth_mode != CodexAuthMode::Chatgpt {
        return Err("Codex completed login without activating a ChatGPT account".to_string());
    }
    Ok(account)
}

pub fn logout_chatgpt(codex_home: &Path) -> Result<CodexAccountStatus, String> {
    let mut server = AppServer::spawn(codex_home)?;
    let current = server.read_account(1)?;
    if current.auth_mode != CodexAuthMode::Chatgpt {
        return Err(match current.auth_mode {
            CodexAuthMode::ApiKey => {
                "refusing to log out because Codex is using API Key authentication".to_string()
            }
            CodexAuthMode::None => "Codex does not have an active ChatGPT login".to_string(),
            CodexAuthMode::Other => {
                "refusing to log out because Codex is using another authentication mode".to_string()
            }
            CodexAuthMode::Chatgpt => unreachable!(),
        });
    }

    server.write(json!({"method": "account/logout", "id": 2}))?;
    server.wait_for_response(2, RESPONSE_TIMEOUT)?;
    if server.wait_for_account_updated(RESPONSE_TIMEOUT)? != CodexAuthMode::None {
        return Err("Codex logout completed without clearing ChatGPT authentication".to_string());
    }
    let account = server.read_account(3)?;
    if account.auth_mode != CodexAuthMode::None {
        return Err("Codex logout could not be confirmed".to_string());
    }
    Ok(account)
}

fn reader_error_message(error: CodexJsonLineError) -> String {
    match error {
        CodexJsonLineError::TooLarge => {
            "Codex App Server returned an oversized message".to_string()
        }
        CodexJsonLineError::InvalidJson(_) => {
            "Codex App Server returned an invalid message".to_string()
        }
        CodexJsonLineError::Io(_) => "could not read from Codex App Server".to_string(),
    }
}

fn terminate_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    if child.kill().is_ok() {
        let _ = child.wait();
    }
}

fn response_result(response: Value) -> Result<Value, String> {
    if let Some(error) = response.get("error") {
        return Err(error
            .get("message")
            .and_then(Value::as_str)
            .map(safe_error_message)
            .unwrap_or_else(|| "Codex App Server rejected the request".to_string()));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "Codex App Server response did not contain a result".to_string())
}

fn safe_error_message(value: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|character| !character.is_control())
        .take(240)
        .collect::<String>();
    if sanitized.is_empty() {
        "Codex App Server request failed".to_string()
    } else {
        sanitized
    }
}

fn codex_access_token_environment_present() -> bool {
    std::env::var("CODEX_ACCESS_TOKEN").is_ok_and(|value| access_token_value_is_present(&value))
}

fn access_token_value_is_present(value: &str) -> bool {
    !value.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_response_envelopes() {
        assert!(response_result(json!({"id": 1})).is_err());
        assert_eq!(
            response_result(json!({"id": 1, "result": {"ok": true}})).unwrap(),
            json!({"ok": true})
        );
    }

    #[test]
    fn app_server_errors_are_bounded_and_single_line() {
        let message = format!("bad\n{}", "x".repeat(400));
        let safe = safe_error_message(&message);
        assert!(!safe.contains('\n'));
        assert!(safe.chars().count() <= 240);
    }

    #[test]
    fn access_token_presence_ignores_empty_and_whitespace_values() {
        assert!(!access_token_value_is_present(""));
        assert!(!access_token_value_is_present(" \r\n\t"));
        assert!(access_token_value_is_present("external-token"));
    }
}
