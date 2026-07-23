use serde::Deserialize;
use serde::Serialize;
use toml_edit::DocumentMut;

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CurrentCodexConfig {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: Option<String>,
    pub base_url: Option<String>,
    pub auth_kind: AuthKind,
    pub catalog_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AuthKind {
    OfficialLogin,
    SystemCredential,
    EnvironmentVariable,
    InlineToken,
    CommandCredential,
    ProviderManaged,
    Unknown,
}

pub fn inspect_config(config: &str) -> Result<CurrentCodexConfig> {
    let document = if config.trim().is_empty() {
        DocumentMut::new()
    } else {
        config.parse::<DocumentMut>()?
    };

    let provider_id =
        string_value(&document, "model_provider").unwrap_or_else(|| "openai".to_string());
    let provider = document
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(&provider_id))
        .and_then(|item| item.as_table());

    let provider_name = provider
        .and_then(|table| table.get("name"))
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if provider_id == "openai" {
                "OpenAI".to_string()
            } else {
                provider_id.clone()
            }
        });

    let base_url = provider
        .and_then(|table| table.get("base_url"))
        .and_then(|item| item.as_str())
        .or_else(|| {
            if provider_id == "openai" {
                document
                    .get("openai_base_url")
                    .and_then(|item| item.as_str())
            } else {
                None
            }
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let auth_kind = if let Some(kind) = provider.and_then(command_auth_kind) {
        kind
    } else {
        match provider {
            Some(table)
                if table
                    .get("env_key")
                    .and_then(|item| item.as_str())
                    .is_some_and(|value| !value.trim().is_empty()) =>
            {
                AuthKind::EnvironmentVariable
            }
            Some(table)
                if table
                    .get("experimental_bearer_token")
                    .and_then(|item| item.as_str())
                    .is_some_and(|value| !value.trim().is_empty()) =>
            {
                AuthKind::InlineToken
            }
            Some(table)
                if table
                    .get("requires_openai_auth")
                    .and_then(|item| item.as_bool())
                    == Some(true) =>
            {
                AuthKind::OfficialLogin
            }
            _ if provider_id == "openai" => AuthKind::OfficialLogin,
            Some(_) if base_url.is_some() => AuthKind::ProviderManaged,
            Some(_) => AuthKind::Unknown,
            None if base_url.is_some() => AuthKind::ProviderManaged,
            None => AuthKind::Unknown,
        }
    };

    Ok(CurrentCodexConfig {
        provider_id,
        provider_name,
        model_id: string_value(&document, "model"),
        base_url,
        auth_kind,
        catalog_path: string_value(&document, "model_catalog_json"),
    })
}

fn command_auth_kind(provider: &toml_edit::Table) -> Option<AuthKind> {
    let auth = provider.get("auth")?;
    let command = auth
        .as_table()
        .and_then(|table| table.get("command"))
        .and_then(|item| item.as_str())
        .or_else(|| {
            auth.as_inline_table()
                .and_then(|table| table.get("command"))
                .and_then(|value| value.as_str())
        });
    if !auth.is_table() && !auth.is_inline_table() {
        return None;
    }
    Some(
        if command.is_some_and(|value| {
            value
                .to_ascii_lowercase()
                .contains("codex-provider-switcher-helper")
        }) {
            AuthKind::SystemCredential
        } else {
            AuthKind::CommandCredential
        },
    )
}

fn string_value(document: &DocumentMut, key: &str) -> Option<String> {
    document
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_reports_official_defaults_without_inventing_a_model() {
        let current = inspect_config("").unwrap();
        assert_eq!(current.provider_id, "openai");
        assert_eq!(current.provider_name, "OpenAI");
        assert_eq!(current.model_id, None);
        assert_eq!(current.base_url, None);
        assert_eq!(current.auth_kind, AuthKind::OfficialLogin);
    }

    #[test]
    fn reads_the_active_provider_only() {
        let current = inspect_config(
            r#"
model = "vendor/code"
model_provider = "switcher-vendor"

[model_providers.unrelated]
name = "Do not show"
base_url = "https://unrelated.example/v1"

[model_providers.switcher-vendor]
name = "Vendor API"
base_url = "https://api.vendor.example/v1"
wire_api = "responses"

[model_providers.switcher-vendor.auth]
command = "/Applications/Codex Provider Switcher.app/codex-provider-switcher-helper"
args = ["credential", "get", "endpoint-v1-example"]
"#,
        )
        .unwrap();

        assert_eq!(current.provider_id, "switcher-vendor");
        assert_eq!(current.provider_name, "Vendor API");
        assert_eq!(current.model_id.as_deref(), Some("vendor/code"));
        assert_eq!(
            current.base_url.as_deref(),
            Some("https://api.vendor.example/v1")
        );
        assert_eq!(current.auth_kind, AuthKind::SystemCredential);
    }

    #[test]
    fn reports_auth_method_without_returning_secret_material() {
        let current = inspect_config(
            r#"
model_provider = "legacy"
[model_providers.legacy]
name = "Legacy"
base_url = "https://legacy.example/v1"
experimental_bearer_token = "must-not-be-returned"
"#,
        )
        .unwrap();
        let serialized = serde_json::to_string(&current).unwrap();

        assert_eq!(current.auth_kind, AuthKind::InlineToken);
        assert!(!serialized.contains("must-not-be-returned"));
    }

    #[test]
    fn distinguishes_an_external_auth_command_from_the_managed_keyring_helper() {
        let current = inspect_config(
            r#"
model_provider = "external"
[model_providers.external]
base_url = "https://external.example/v1"
auth = { command = "/usr/local/bin/vendor-token" }
"#,
        )
        .unwrap();

        assert_eq!(current.auth_kind, AuthKind::CommandCredential);
    }
}
