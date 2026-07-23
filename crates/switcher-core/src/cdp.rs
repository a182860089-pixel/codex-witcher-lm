use serde::Deserialize;
use serde::Serialize;
use url::Url;

use crate::error::Result;
use crate::error::SwitcherError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdpEndpointKind<'a> {
    Browser(&'a str),
    Page(&'a str),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CdpTarget {
    pub id: String,
    #[serde(rename = "type")]
    pub target_type: String,
    pub url: String,
    pub web_socket_debugger_url: String,
}

pub fn validate_browser_websocket_url(raw: &str, port: u16) -> Result<String> {
    let id = endpoint_id(raw, port, "browser")?;
    validate_identifier(&id, "browser id")?;
    Ok(id)
}

pub fn validate_page_websocket_url(raw: &str, port: u16, page_id: &str) -> Result<()> {
    validate_identifier(page_id, "page id")?;
    let id = endpoint_id(raw, port, "page")?;
    if id != page_id {
        return Err(SwitcherError::Validation(
            "CDP target id does not match its WebSocket endpoint".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_page_target(target: &CdpTarget, port: u16) -> Result<()> {
    validate_page_target_for_cleanup(target, port)?;
    let page_url = Url::parse(&target.url)?;
    if page_url.query().is_some() || page_url.fragment().is_some() {
        return Err(SwitcherError::Validation(
            "CDP install target must not contain a query or fragment".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_page_target_for_cleanup(target: &CdpTarget, port: u16) -> Result<()> {
    if target.target_type != "page" {
        return Err(SwitcherError::Validation(
            "CDP target is not a page".to_string(),
        ));
    }
    let page_url = Url::parse(&target.url)?;
    if page_url.scheme() != "app"
        || page_url.host_str() != Some("codex")
        || page_url.username() != ""
        || page_url.password().is_some()
    {
        return Err(SwitcherError::Validation(
            "CDP page is not owned by the app://codex origin".to_string(),
        ));
    }
    validate_page_websocket_url(&target.web_socket_debugger_url, port, &target.id)
}

fn endpoint_id(raw: &str, port: u16, endpoint_type: &str) -> Result<String> {
    if raw.contains('%') {
        return Err(SwitcherError::Validation(
            "percent-encoded CDP endpoint paths are not accepted".to_string(),
        ));
    }
    let url = Url::parse(raw)?;
    if url.scheme() != "ws"
        || url.host_str() != Some("127.0.0.1")
        || url.port() != Some(port)
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SwitcherError::Validation(
            "CDP WebSocket must use the expected 127.0.0.1 listener".to_string(),
        ));
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() != 3 || segments[0] != "devtools" || segments[1] != endpoint_type {
        return Err(SwitcherError::Validation(format!(
            "unexpected CDP {endpoint_type} endpoint path"
        )));
    }
    Ok(segments[2].to_string())
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(SwitcherError::Validation(format!("invalid CDP {label}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_strict_loopback_target() {
        let target = CdpTarget {
            id: "page-123".into(),
            target_type: "page".into(),
            url: "app://codex/".into(),
            web_socket_debugger_url: "ws://127.0.0.1:9335/devtools/page/page-123".into(),
        };
        validate_page_target(&target, 9335).unwrap();
        assert_eq!(
            validate_browser_websocket_url("ws://127.0.0.1:9335/devtools/browser/browser-1", 9335)
                .unwrap(),
            "browser-1"
        );
    }

    #[test]
    fn rejects_remote_or_inconsistent_targets() {
        let unsafe_values = [
            "ws://example.com:9335/devtools/page/page-123",
            "ws://127.0.0.1:9336/devtools/page/page-123",
            "wss://127.0.0.1:9335/devtools/page/page-123",
            "ws://user@127.0.0.1:9335/devtools/page/page-123",
            "ws://127.0.0.1:9335/devtools/browser/page-123",
            "ws://127.0.0.1:9335/devtools/page/page-123?query=1",
            "ws://127.0.0.1:9335/devtools/page/page%2D123",
        ];
        for value in unsafe_values {
            assert!(validate_page_websocket_url(value, 9335, "page-123").is_err());
        }
    }

    #[test]
    fn rejects_non_codex_page_origin() {
        let target = CdpTarget {
            id: "page-123".into(),
            target_type: "page".into(),
            url: "https://example.test/".into(),
            web_socket_debugger_url: "ws://127.0.0.1:9335/devtools/page/page-123".into(),
        };
        assert!(validate_page_target(&target, 9335).is_err());
    }

    #[test]
    fn cleanup_validation_accepts_same_origin_navigation_only() {
        let target = CdpTarget {
            id: "page-123".into(),
            target_type: "page".into(),
            url: "app://codex/workspace?view=thread#latest".into(),
            web_socket_debugger_url: "ws://127.0.0.1:9335/devtools/page/page-123".into(),
        };
        assert!(validate_page_target(&target, 9335).is_err());
        validate_page_target_for_cleanup(&target, 9335).unwrap();
    }
}
