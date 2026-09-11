use std::collections::HashSet;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde::Serialize;

use crate::validation::validate_base_url;
use crate::validation::validate_model_id;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_MODELS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FetchedModel {
    pub id: String,
    pub owned_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelDiscovery {
    pub base_url: String,
    pub models: Vec<FetchedModel>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
}

pub async fn fetch_models(
    base_url: &str,
    api_key: &str,
) -> std::result::Result<ModelDiscovery, String> {
    let api_key = api_key.trim();
    if api_key.is_empty()
        || api_key.len() > 8_192
        || api_key.contains('\0')
        || api_key.contains('\r')
        || api_key.contains('\n')
    {
        return Err("API Key must be 1-8192 characters without line breaks".to_string());
    }
    let candidates = model_endpoint_candidates(base_url)?;
    let mut client = reqwest::Client::builder()
        .redirect(Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .http1_only()
        .tcp_nodelay(true)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .user_agent("codex-provider-switcher/0.3.4");
    if destination_is_loopback(base_url) {
        client = client.no_proxy();
    }
    let client = client
        .build()
        .map_err(|_| "could not create the secure HTTP client".to_string())?;
    let mut last_not_found = None;

    for candidate in candidates {
        let response = client
            .get(&candidate)
            .bearer_auth(api_key)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    "model request timed out".to_string()
                } else {
                    "could not connect to the model endpoint".to_string()
                }
            })?;
        let status = response.status();

        if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            last_not_found = Some(status);
            continue;
        }
        if !status.is_success() {
            return Err(format!("model endpoint returned HTTP {}", status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err("model response is too large".to_string());
        }

        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "could not read the model response".to_string())?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err("model response is too large".to_string());
            }
            bytes.extend_from_slice(&chunk);
        }
        let parsed = serde_json::from_slice::<ModelsResponse>(&bytes)
            .map_err(|_| "model endpoint returned an unsupported response".to_string())?;
        let mut seen = HashSet::new();
        let mut models = parsed
            .data
            .into_iter()
            .filter_map(|entry| {
                let id = entry.id.trim().to_string();
                if validate_model_id(&id).is_err() || !seen.insert(id.clone()) {
                    return None;
                }
                Some(FetchedModel {
                    id,
                    owned_by: entry
                        .owned_by
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty()),
                })
            })
            .take(MAX_MODELS)
            .collect::<Vec<_>>();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        return Ok(ModelDiscovery {
            base_url: api_base_from_models_endpoint(&candidate),
            models,
        });
    }

    Err(format!(
        "model endpoint returned HTTP {}",
        last_not_found.unwrap_or(StatusCode::NOT_FOUND).as_u16()
    ))
}

fn api_base_from_models_endpoint(endpoint: &str) -> String {
    endpoint
        .strip_suffix("/models")
        .unwrap_or(endpoint)
        .trim_end_matches('/')
        .to_string()
}

pub fn model_endpoint_candidates(base_url: &str) -> std::result::Result<Vec<String>, String> {
    validate_base_url(base_url).map_err(|error| error.to_string())?;
    let normalized = base_url.trim().trim_end_matches('/');
    if normalized.ends_with("/models") {
        return Ok(vec![normalized.to_string()]);
    }

    let mut candidates = Vec::with_capacity(2);
    if let Some(version_index) = normalized.rfind("/v1/") {
        candidates.push(format!("{}/v1/models", &normalized[..version_index]));
    } else if ends_with_version_segment(normalized) {
        candidates.push(format!("{normalized}/models"));
    } else {
        candidates.push(format!("{normalized}/v1/models"));
        candidates.push(format!("{normalized}/models"));
    }
    candidates.dedup();
    Ok(candidates)
}

pub fn normalize_api_base_url(base_url: &str) -> std::result::Result<String, String> {
    validate_base_url(base_url).map_err(|error| error.to_string())?;
    let normalized = base_url.trim().trim_end_matches('/');
    Ok(normalized
        .strip_suffix("/models")
        .or_else(|| normalized.strip_suffix("/responses"))
        .unwrap_or(normalized)
        .trim_end_matches('/')
        .to_string())
}

fn ends_with_version_segment(url: &str) -> bool {
    url.rsplit('/')
        .next()
        .and_then(|segment| segment.strip_prefix('v'))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn destination_is_loopback(base_url: &str) -> bool {
    url::Url::parse(base_url)
        .ok()
        .and_then(|parsed| parsed.host().map(host_is_loopback))
        .unwrap_or(false)
}

fn host_is_loopback(host: url::Host<&str>) -> bool {
    match host {
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
        url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Duration;

    use super::*;

    #[test]
    fn derives_standard_model_endpoints() {
        assert_eq!(
            model_endpoint_candidates("https://api.example.test/v1").unwrap(),
            vec!["https://api.example.test/v1/models"]
        );
        assert_eq!(
            model_endpoint_candidates("https://api.example.test").unwrap(),
            vec![
                "https://api.example.test/v1/models",
                "https://api.example.test/models"
            ]
        );
        assert_eq!(
            model_endpoint_candidates("https://api.example.test/v1/responses").unwrap(),
            vec!["https://api.example.test/v1/models"]
        );
    }

    #[test]
    fn rejects_remote_plain_http_before_network_access() {
        assert!(destination_is_loopback("http://127.0.0.1:11434/v1"));
        assert!(destination_is_loopback("http://localhost:11434/v1"));
        assert!(!destination_is_loopback("https://api.example.test/v1"));
    }

    #[test]
    fn version_segments_append_models_without_an_extra_v1() {
        assert_eq!(
            model_endpoint_candidates("https://api.example.test/coding/v4").unwrap(),
            vec!["https://api.example.test/coding/v4/models"]
        );
    }

    #[test]
    fn successful_model_endpoint_resolves_the_codex_api_base() {
        assert_eq!(
            api_base_from_models_endpoint("https://api.example.test/v1/models"),
            "https://api.example.test/v1"
        );
        assert_eq!(
            api_base_from_models_endpoint("https://api.example.test/models"),
            "https://api.example.test"
        );
        assert_eq!(
            normalize_api_base_url("https://api.example.test/v1/responses").unwrap(),
            "https://api.example.test/v1"
        );
    }

    #[tokio::test]
    async fn fetches_standard_models_and_returns_the_resolved_api_base() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener
            .set_nonblocking(false)
            .expect("test listener should be blocking");
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /v1/models HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-key")
            );

            let body = r#"{"data":[{"id":"zeta","owned_by":"vendor"},{"id":"alpha"},{"id":"alpha"},{"id":"not valid"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });

        let discovery = fetch_models(&format!("http://{address}"), "test-key")
            .await
            .unwrap();
        server.join().unwrap();

        assert_eq!(discovery.base_url, format!("http://{address}/v1"));
        assert_eq!(
            discovery
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }

    #[tokio::test]
    async fn rejects_multiline_keys_before_network_access() {
        let error = fetch_models("https://api.example.test/v1", "unsafe\nkey")
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "API Key must be 1-8192 characters without line breaks"
        );
    }
}
