use std::collections::HashSet;
use std::net::IpAddr;

use url::Host;
use url::Url;

use crate::domain::OFFICIAL_PROFILE_DISPLAY_NAME;
use crate::domain::OFFICIAL_PROFILE_SCHEMA_VERSION;
use crate::domain::OfficialProfile;
use crate::domain::ProviderProfile;
use crate::error::Result;
use crate::error::SwitcherError;

const RESERVED_PROVIDER_IDS: &[&str] = &[
    "openai",
    "amazon-bedrock",
    "ollama",
    "ollama-chat",
    "lmstudio",
];

pub fn validate_profile(profile: &ProviderProfile) -> Result<()> {
    validate_provider_id(&profile.id)?;
    validate_display_name("provider display name", &profile.display_name)?;
    validate_base_url(&profile.base_url)?;

    if profile.models.is_empty() {
        return Err(SwitcherError::Validation(
            "at least one model is required".to_string(),
        ));
    }

    let mut ids = HashSet::new();
    for model in &profile.models {
        validate_model_id(&model.id)?;
        validate_display_name("model display name", &model.display_name)?;
        if !ids.insert(model.id.as_str()) {
            return Err(SwitcherError::Validation(format!(
                "duplicate model id: {}",
                model.id
            )));
        }
        if !(4_096..=2_000_000).contains(&model.context_window) {
            return Err(SwitcherError::Validation(format!(
                "model {} context window must be between 4096 and 2000000",
                model.id
            )));
        }
        if model.reasoning_levels.is_empty() {
            return Err(SwitcherError::Validation(format!(
                "model {} must expose at least one reasoning level",
                model.id
            )));
        }
        if !model
            .reasoning_levels
            .iter()
            .any(|level| level == &model.default_reasoning)
        {
            return Err(SwitcherError::Validation(format!(
                "model {} default reasoning level is not in reasoning_levels",
                model.id
            )));
        }
    }

    Ok(())
}

pub fn validate_official_profile(profile: &OfficialProfile) -> Result<()> {
    match profile.schema_version {
        1 if profile.display_name != OFFICIAL_PROFILE_DISPLAY_NAME => {
            return Err(SwitcherError::Validation(
                "the legacy official profile display name is invalid".to_string(),
            ));
        }
        1 | OFFICIAL_PROFILE_SCHEMA_VERSION => {}
        _ => {
            return Err(SwitcherError::Validation(
                "unsupported official profile schema version".to_string(),
            ));
        }
    }
    validate_display_name("official profile display name", &profile.display_name)?;
    if let Some(model_id) = profile.model_id.as_deref() {
        validate_model_id(model_id)?;
    }
    Ok(())
}

pub(crate) fn validate_provider_id(id: &str) -> Result<()> {
    if id.len() < 2
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        || !id
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(SwitcherError::Validation(
            "provider id must be 2-64 lowercase ASCII letters, digits, or hyphens".to_string(),
        ));
    }
    if RESERVED_PROVIDER_IDS.contains(&id) {
        return Err(SwitcherError::Validation(format!(
            "provider id {id} is reserved by Codex"
        )));
    }
    Ok(())
}

pub(crate) fn validate_model_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
    {
        return Err(SwitcherError::Validation(format!("invalid model id: {id}")));
    }
    Ok(())
}

fn validate_display_name(field: &str, value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 80 || trimmed.chars().any(char::is_control) {
        return Err(SwitcherError::Validation(format!(
            "{field} must be 1-80 printable characters"
        )));
    }
    Ok(())
}

pub(crate) fn validate_base_url(value: &str) -> Result<()> {
    let url = Url::parse(value)?;
    if url.cannot_be_a_base()
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SwitcherError::Validation(
            "base URL must not contain credentials, query parameters, or a fragment".to_string(),
        ));
    }

    let is_loopback = match url.host() {
        Some(Host::Ipv4(address)) => IpAddr::V4(address).is_loopback(),
        Some(Host::Ipv6(address)) => IpAddr::V6(address).is_loopback(),
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        None => false,
    };

    match url.scheme() {
        "https" => {}
        "http" if is_loopback => {}
        "http" => {
            return Err(SwitcherError::Validation(
                "non-loopback provider URLs must use HTTPS".to_string(),
            ));
        }
        _ => {
            return Err(SwitcherError::Validation(
                "provider URL scheme must be HTTPS, or HTTP on loopback".to_string(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::domain::ModelSpec;
    use crate::domain::ReasoningEffort;

    use super::*;

    fn profile(base_url: &str) -> ProviderProfile {
        ProviderProfile {
            id: "my-provider".into(),
            display_name: "My Provider".into(),
            base_url: base_url.into(),
            supports_websockets: false,
            credential_required: true,
            models: vec![ModelSpec {
                id: "vendor/model-1".into(),
                display_name: "Model 1".into(),
                description: String::new(),
                context_window: 128_000,
                default_reasoning: ReasoningEffort::Medium,
                reasoning_levels: vec![ReasoningEffort::Low, ReasoningEffort::Medium],
                supports_parallel_tool_calls: true,
                supports_images: false,
            }],
        }
    }

    #[test]
    fn accepts_https_and_loopback_http() {
        validate_profile(&profile("https://api.example.test/v1")).unwrap();
        validate_profile(&profile("http://127.0.0.1:4141/v1")).unwrap();
        validate_profile(&profile("http://[::1]:4141/v1")).unwrap();
    }

    #[test]
    fn rejects_remote_http_and_reserved_provider() {
        assert!(validate_profile(&profile("http://api.example.test/v1")).is_err());
        let mut value = profile("https://api.example.test/v1");
        value.id = "openai".into();
        assert!(validate_profile(&value).is_err());
    }

    #[test]
    fn rejects_duplicate_models() {
        let mut value = profile("https://api.example.test/v1");
        value.models.push(value.models[0].clone());
        assert!(validate_profile(&value).is_err());
    }

    #[test]
    fn official_profile_accepts_named_v2_and_legacy_v1() {
        validate_official_profile(&OfficialProfile::new(
            "个人 Plus".to_string(),
            Some("gpt-5.6-sol".to_string()),
        ))
        .unwrap();
        validate_official_profile(&OfficialProfile {
            schema_version: 1,
            display_name: OFFICIAL_PROFILE_DISPLAY_NAME.to_string(),
            model_id: None,
        })
        .unwrap();
    }

    #[test]
    fn official_profile_rejects_invalid_names_and_legacy_renames() {
        assert!(validate_official_profile(&OfficialProfile::new(" \n".to_string(), None)).is_err());
        assert!(
            validate_official_profile(&OfficialProfile {
                schema_version: 1,
                display_name: "个人 Plus".to_string(),
                model_id: None,
            })
            .is_err()
        );
    }
}
