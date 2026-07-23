use std::path::Path;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use toml_edit::Array;
use toml_edit::DocumentMut;
use toml_edit::Item;
use toml_edit::Table;
use toml_edit::value;

use crate::catalog::render_model_catalog;
use crate::domain::ProviderProfile;
use crate::error::Result;
use crate::error::SwitcherError;
use crate::validation::validate_base_url;
use crate::validation::validate_profile;
use crate::validation::validate_provider_id;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigPlan {
    pub expected_config_sha256: String,
    pub rendered_config: String,
    pub catalog_path: std::path::PathBuf,
    pub rendered_catalog: String,
    pub provider_id: String,
    pub model_id: String,
}

pub fn plan_config(
    existing_config: &str,
    profile: &ProviderProfile,
    selected_model: &str,
    catalog_path: &Path,
    credential_helper: Option<&Path>,
) -> Result<ConfigPlan> {
    validate_profile(profile)?;
    if !catalog_path.is_absolute() {
        return Err(SwitcherError::Validation(
            "catalog path must be absolute".to_string(),
        ));
    }
    if !profile
        .models
        .iter()
        .any(|model| model.id == selected_model)
    {
        return Err(SwitcherError::Validation(format!(
            "selected model {selected_model} does not belong to provider {}",
            profile.id
        )));
    }

    if profile.credential_required {
        let helper = credential_helper.ok_or_else(|| {
            SwitcherError::Validation(
                "a credential helper path is required for this provider".to_string(),
            )
        })?;
        if !helper.is_absolute() {
            return Err(SwitcherError::Validation(
                "credential helper path must be absolute".to_string(),
            ));
        }
    }

    let mut document = if existing_config.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing_config.parse::<DocumentMut>()?
    };

    document["model_provider"] = value(profile.id.clone());
    document["model"] = value(selected_model);

    if !document.as_table().contains_key("model_providers") {
        document["model_providers"] = Item::Table(Table::new());
    }
    let providers = document["model_providers"].as_table_mut().ok_or_else(|| {
        SwitcherError::Validation("model_providers must be a TOML table".to_string())
    })?;

    let mut provider = Table::new();
    provider["name"] = value(profile.display_name.clone());
    provider["base_url"] = value(profile.base_url.trim_end_matches('/'));
    provider["wire_api"] = value("responses");
    provider["supports_websockets"] = value(profile.supports_websockets);

    if let Some(helper) = credential_helper.filter(|_| profile.credential_required) {
        let mut auth = Table::new();
        auth["command"] = value(path_as_utf8(helper)?);
        auth["cwd"] = value(path_as_utf8(helper.parent().ok_or_else(|| {
            SwitcherError::Validation("credential helper path has no parent".to_string())
        })?)?);
        let mut args = Array::new();
        args.push("credential");
        args.push("get");
        args.push(credential_account_for(&profile.id, &profile.base_url)?);
        auth["args"] = value(args);
        auth["timeout_ms"] = value(5_000);
        auth["refresh_interval_ms"] = value(300_000);
        provider["auth"] = Item::Table(auth);
    }

    providers.insert(&profile.id, Item::Table(provider));

    Ok(ConfigPlan {
        expected_config_sha256: sha256_hex(existing_config.as_bytes()),
        rendered_config: document.to_string(),
        catalog_path: catalog_path.to_path_buf(),
        rendered_catalog: render_model_catalog(profile)?,
        provider_id: profile.id.clone(),
        model_id: selected_model.to_string(),
    })
}

pub fn credential_account_for(provider_id: &str, base_url: &str) -> Result<String> {
    validate_provider_id(provider_id)?;
    validate_base_url(base_url)?;
    let parsed = url::Url::parse(base_url)?;
    let normalized_url = parsed.as_str().trim_end_matches('/');
    let fingerprint = sha256_hex(format!("{provider_id}\0{normalized_url}").as_bytes());
    Ok(format!("endpoint-v1-{fingerprint}"))
}

pub fn verify_credential_binding(
    config: &str,
    account: &str,
    credential_helper: &Path,
) -> Result<()> {
    let document = config.parse::<DocumentMut>()?;
    let providers = document
        .get("model_providers")
        .and_then(|item| item.as_table())
        .ok_or_else(|| {
            SwitcherError::Validation("model provider configuration is missing".to_string())
        })?;
    let helper = path_as_utf8(credential_helper)?;
    let helper_parent = path_as_utf8(credential_helper.parent().ok_or_else(|| {
        SwitcherError::Validation("credential helper path has no parent".to_string())
    })?)?;
    let expected_args = ["credential", "get", account];
    let mut matches = 0_usize;

    for (provider_id, item) in providers {
        let Some(provider) = item.as_table() else {
            continue;
        };
        let Some(base_url) = provider.get("base_url").and_then(|item| item.as_str()) else {
            continue;
        };
        let Ok(expected_account) = credential_account_for(provider_id, base_url) else {
            continue;
        };
        if expected_account != account {
            continue;
        }
        let Some(auth) = provider.get("auth").and_then(|item| item.as_table()) else {
            continue;
        };
        let command_matches =
            auth.get("command").and_then(|item| item.as_str()) == Some(helper.as_str());
        let cwd_matches =
            auth.get("cwd").and_then(|item| item.as_str()) == Some(helper_parent.as_str());
        let args_match = auth
            .get("args")
            .and_then(|item| item.as_array())
            .is_some_and(|args| {
                args.len() == expected_args.len()
                    && args
                        .iter()
                        .zip(expected_args)
                        .all(|(value, expected)| value.as_str() == Some(expected))
            });
        if command_matches && cwd_matches && args_match {
            matches += 1;
        }
    }
    if matches != 1 {
        return Err(SwitcherError::Validation(
            "credential account is not bound to exactly one managed provider".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn path_as_utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| SwitcherError::Validation("path is not valid UTF-8".to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::domain::ModelSpec;
    use crate::domain::ReasoningEffort;

    use super::*;

    fn profile() -> ProviderProfile {
        ProviderProfile {
            id: "acme".into(),
            display_name: "Acme Gateway".into(),
            base_url: "https://api.acme.test/v1/".into(),
            supports_websockets: false,
            credential_required: true,
            models: vec![ModelSpec {
                id: "acme/code".into(),
                display_name: "Acme Code".into(),
                description: String::new(),
                context_window: 128_000,
                default_reasoning: ReasoningEffort::Medium,
                reasoning_levels: vec![ReasoningEffort::Medium],
                supports_parallel_tool_calls: true,
                supports_images: false,
            }],
        }
    }

    fn absolute_test_path(name: &str) -> PathBuf {
        #[cfg(windows)]
        {
            Path::new(r"C:\codex-provider-switcher-tests").join(name)
        }
        #[cfg(not(windows))]
        {
            Path::new("/tmp/codex-provider-switcher-tests").join(name)
        }
    }

    #[test]
    fn preserves_unrelated_configuration_and_adds_command_auth() {
        let existing = "# keep me\napproval_policy = \"on-request\"\n\n[mcp_servers.demo]\ncommand = \"demo\"\n";
        let catalog = absolute_test_path("models.json");
        let helper = absolute_test_path("helper");
        let plan = plan_config(existing, &profile(), "acme/code", &catalog, Some(&helper)).unwrap();

        assert!(plan.rendered_config.contains("# keep me"));
        assert!(plan.rendered_config.contains("[mcp_servers.demo]"));
        assert!(plan.rendered_config.contains("[model_providers.acme.auth]"));
        assert!(
            plan.rendered_config
                .contains("\"credential\", \"get\", \"endpoint-v1-")
        );
        assert!(!plan.rendered_config.contains("model_catalog_json"));
        assert!(!plan.rendered_config.contains("requires_openai_auth"));
        assert!(!plan.rendered_config.contains("api_key"));
    }

    #[test]
    fn preserves_a_user_owned_model_catalog_override() {
        let existing = "model_catalog_json = \"/opt/codex/my-models.json\"\n";
        let catalog = absolute_test_path("switcher-active-profile.json");
        let helper = absolute_test_path("helper");
        let plan = plan_config(existing, &profile(), "acme/code", &catalog, Some(&helper)).unwrap();

        assert!(
            plan.rendered_config
                .contains("model_catalog_json = \"/opt/codex/my-models.json\"")
        );
        assert!(
            !plan.rendered_config.contains(
                catalog
                    .to_str()
                    .expect("test catalog path should be valid UTF-8")
            )
        );
    }

    #[test]
    fn rejects_model_from_another_profile() {
        let catalog = absolute_test_path("models.json");
        let helper = absolute_test_path("helper");
        assert!(plan_config("", &profile(), "other", &catalog, Some(&helper)).is_err());
    }

    #[test]
    fn credential_accounts_are_endpoint_bound_and_canonicalized() {
        let first = credential_account_for("acme", "https://API.example.test:443/v1/").unwrap();
        let equivalent = credential_account_for("acme", "https://api.example.test/v1").unwrap();
        let other_host = credential_account_for("acme", "https://other.example.test/v1").unwrap();

        assert_eq!(first, equivalent);
        assert_ne!(first, other_host);
        assert!(first.starts_with("endpoint-v1-"));
    }

    #[test]
    fn credential_binding_supports_a_non_default_thread_provider() {
        let helper = absolute_test_path("helper");
        let catalog = absolute_test_path("models.json");
        let first_profile = profile();
        let first_account =
            credential_account_for(&first_profile.id, &first_profile.base_url).unwrap();
        let first = plan_config("", &first_profile, "acme/code", &catalog, Some(&helper)).unwrap();

        let mut second_profile = profile();
        second_profile.id = "other".into();
        second_profile.display_name = "Other".into();
        second_profile.base_url = "https://other.example.test/v1".into();
        let second = plan_config(
            &first.rendered_config,
            &second_profile,
            "acme/code",
            &catalog,
            Some(&helper),
        )
        .unwrap();

        verify_credential_binding(&second.rendered_config, &first_account, &helper).unwrap();
        let tampered = second
            .rendered_config
            .replace("https://api.acme.test/v1", "https://evil.example.test/v1");
        assert!(verify_credential_binding(&tampered, &first_account, &helper).is_err());
    }
}
