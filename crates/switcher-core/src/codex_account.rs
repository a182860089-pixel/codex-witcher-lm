use std::io::BufRead;
use std::io::Read;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::error::Result;
use crate::error::SwitcherError;
use crate::validation::validate_official_account_metadata;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CodexAuthMode {
    None,
    ApiKey,
    Chatgpt,
    Other,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountStatus {
    pub auth_mode: CodexAuthMode,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub requires_openai_auth: bool,
    pub codex_access_token_environment_present: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CodexJsonLineError {
    #[error("could not read a Codex App Server message")]
    Io(#[from] std::io::Error),
    #[error("Codex App Server message exceeds the configured limit")]
    TooLarge,
    #[error("Codex App Server message is not valid JSON")]
    InvalidJson(#[source] serde_json::Error),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountReadResult {
    account: Option<Account>,
    requires_openai_auth: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Account {
    ApiKey {},
    Chatgpt {
        email: Option<String>,
        #[serde(rename = "planType")]
        plan_type: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountUpdatedParams {
    auth_mode: Value,
}

pub fn read_codex_json_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> std::result::Result<Option<Value>, CodexJsonLineError> {
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or(CodexJsonLineError::TooLarge)?;
    let read_limit = u64::try_from(read_limit).map_err(|_| CodexJsonLineError::TooLarge)?;
    let mut line = Vec::with_capacity(max_bytes.saturating_add(1).min(8 * 1024));
    let bytes_read = reader
        .take(read_limit)
        .read_until(b'\n', &mut line)
        .map_err(CodexJsonLineError::Io)?;

    if bytes_read == 0 {
        return Ok(None);
    }
    if line.len() > max_bytes {
        return Err(CodexJsonLineError::TooLarge);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }

    serde_json::from_slice(&line)
        .map(Some)
        .map_err(CodexJsonLineError::InvalidJson)
}

pub fn parse_codex_account_result(
    result: Value,
    codex_access_token_environment_present: bool,
) -> Result<CodexAccountStatus> {
    let parsed = serde_json::from_value::<AccountReadResult>(result)
        .map_err(|_| unsupported_account_response())?;
    let (auth_mode, email, plan_type) = match parsed.account {
        None => (CodexAuthMode::None, None, None),
        Some(Account::ApiKey {}) => (CodexAuthMode::ApiKey, None, None),
        Some(Account::Chatgpt { email, plan_type }) => {
            validate_official_account_metadata(email.as_deref(), Some(&plan_type))?;
            (CodexAuthMode::Chatgpt, email, Some(plan_type))
        }
        Some(Account::Other) => (CodexAuthMode::Other, None, None),
    };
    Ok(CodexAccountStatus {
        auth_mode,
        email,
        plan_type,
        requires_openai_auth: parsed.requires_openai_auth,
        codex_access_token_environment_present,
    })
}

pub fn parse_codex_account_updated(params: Value) -> Result<CodexAuthMode> {
    let parsed = serde_json::from_value::<AccountUpdatedParams>(params)
        .map_err(|_| unsupported_account_response())?;
    match parsed.auth_mode {
        Value::Null => Ok(CodexAuthMode::None),
        Value::String(value) => Ok(match value.as_str() {
            "apikey" => CodexAuthMode::ApiKey,
            "chatgpt" => CodexAuthMode::Chatgpt,
            _ => CodexAuthMode::Other,
        }),
        _ => Err(unsupported_account_response()),
    }
}

fn unsupported_account_response() -> SwitcherError {
    SwitcherError::Validation("unsupported Codex account response".to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::json;

    use super::*;

    #[test]
    fn reads_lf_crlf_and_eof_terminated_json_lines() {
        let mut lf = Cursor::new(
            br#"{"kind":"lf"}
"#,
        );
        assert_eq!(
            read_codex_json_line(&mut lf, 64).unwrap(),
            Some(json!({"kind": "lf"}))
        );
        assert_eq!(read_codex_json_line(&mut lf, 64).unwrap(), None);

        let mut crlf = Cursor::new(b"{\"kind\":\"crlf\"}\r\n".as_slice());
        assert_eq!(
            read_codex_json_line(&mut crlf, 64).unwrap(),
            Some(json!({"kind": "crlf"}))
        );

        let mut eof = Cursor::new(br#"{"kind":"eof"}"#);
        assert_eq!(
            read_codex_json_line(&mut eof, 64).unwrap(),
            Some(json!({"kind": "eof"}))
        );
    }

    #[test]
    fn rejects_invalid_json_and_bounds_oversized_reads() {
        let mut invalid = Cursor::new(b"{nope}\n".as_slice());
        assert!(matches!(
            read_codex_json_line(&mut invalid, 64),
            Err(CodexJsonLineError::InvalidJson(_))
        ));

        let limit = 32;
        let mut oversized = Cursor::new(vec![b'x'; limit + 128]);
        assert!(matches!(
            read_codex_json_line(&mut oversized, limit),
            Err(CodexJsonLineError::TooLarge)
        ));
        assert_eq!(oversized.position(), (limit + 1) as u64);
    }

    #[test]
    fn classifies_supported_and_unknown_account_states() {
        let logged_out =
            parse_codex_account_result(json!({"account": null, "requiresOpenaiAuth": true}), false)
                .unwrap();
        assert_eq!(logged_out.auth_mode, CodexAuthMode::None);

        let api_key = parse_codex_account_result(
            json!({
                "account": {"type": "apiKey", "apiKey": "must-not-escape"},
                "requiresOpenaiAuth": true
            }),
            true,
        )
        .unwrap();
        assert_eq!(api_key.auth_mode, CodexAuthMode::ApiKey);
        assert!(api_key.codex_access_token_environment_present);
        assert_eq!(
            serde_json::to_value(&api_key).unwrap()["authMode"],
            json!("apiKey")
        );
        assert_eq!(
            serde_json::to_value(&api_key).unwrap()["codexAccessTokenEnvironmentPresent"],
            json!(true)
        );

        let chatgpt = parse_codex_account_result(
            json!({
                "account": {
                    "type": "chatgpt",
                    "email": "user@example.com",
                    "planType": "pro"
                },
                "requiresOpenaiAuth": true
            }),
            false,
        )
        .unwrap();
        assert_eq!(chatgpt.auth_mode, CodexAuthMode::Chatgpt);
        assert_eq!(chatgpt.email.as_deref(), Some("user@example.com"));
        assert_eq!(chatgpt.plan_type.as_deref(), Some("pro"));

        let unknown = parse_codex_account_result(
            json!({
                "account": {"type": "futureMode", "accessToken": "must-not-escape"},
                "requiresOpenaiAuth": false
            }),
            false,
        )
        .unwrap();
        assert_eq!(unknown.auth_mode, CodexAuthMode::Other);
    }

    #[test]
    fn accepts_missing_email_and_rejects_invalid_account_metadata() {
        let without_email = parse_codex_account_result(
            json!({
                "account": {"type": "chatgpt", "email": null, "planType": "plus"},
                "requiresOpenaiAuth": true
            }),
            false,
        )
        .unwrap();
        assert_eq!(without_email.email, None);

        for email in ["\nuser@example.com".to_string(), "x".repeat(321)] {
            assert!(
                parse_codex_account_result(
                    json!({
                        "account": {
                            "type": "chatgpt",
                            "email": email,
                            "planType": "plus"
                        },
                        "requiresOpenaiAuth": true
                    }),
                    false
                )
                .is_err()
            );
        }
        for plan_type in ["bad plan".to_string(), "x".repeat(33)] {
            assert!(
                parse_codex_account_result(
                    json!({
                        "account": {
                            "type": "chatgpt",
                            "email": "user@example.com",
                            "planType": plan_type
                        },
                        "requiresOpenaiAuth": true
                    }),
                    false
                )
                .is_err()
            );
        }
    }

    #[test]
    fn account_status_serialization_cannot_expose_extra_token_fields() {
        let status = parse_codex_account_result(
            json!({
                "account": {
                    "type": "chatgpt",
                    "email": "user@example.com",
                    "planType": "plus",
                    "accessToken": "secret-access-token",
                    "refreshToken": "secret-refresh-token"
                },
                "requiresOpenaiAuth": true
            }),
            false,
        )
        .unwrap();
        let serialized = serde_json::to_string(&status).unwrap();
        assert!(!serialized.contains("accessToken"));
        assert!(!serialized.contains("refreshToken"));
        assert!(!serialized.contains("secret"));
    }

    #[test]
    fn parses_account_updated_auth_modes_strictly() {
        assert_eq!(
            parse_codex_account_updated(json!({"authMode": "chatgpt", "planType": "plus"}))
                .unwrap(),
            CodexAuthMode::Chatgpt
        );
        assert_eq!(
            parse_codex_account_updated(json!({"authMode": null, "planType": null})).unwrap(),
            CodexAuthMode::None
        );
        assert_eq!(
            parse_codex_account_updated(json!({"authMode": "futureMode"})).unwrap(),
            CodexAuthMode::Other
        );
        assert!(parse_codex_account_updated(json!({})).is_err());
        assert!(parse_codex_account_updated(json!({"authMode": 1})).is_err());
    }
}
