use std::path::Path;
use std::path::PathBuf;

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
use crate::domain::OfficialProfile;
use crate::domain::ProviderProfile;
use crate::error::Result;
use crate::error::SwitcherError;
use crate::validation::validate_base_url;
use crate::validation::validate_official_profile;
use crate::validation::validate_profile;
use crate::validation::validate_provider_id;

pub const LOCAL_PROXY_PROVIDER_ID: &str = "cps-local";
pub const LOCAL_PROXY_PROVIDER_NAME: &str = "Codex Provider Switcher";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigPlan {
    pub expected_config_sha256: String,
    pub rendered_config: String,
    pub catalog_path: std::path::PathBuf,
    pub rendered_catalog: String,
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

pub fn plan_config(
    existing_config: &str,
    profile: &ProviderProfile,
    selected_model: &str,
    catalog_path: &Path,
    credential_helper: Option<&Path>,
) -> Result<ConfigPlan> {
    let credential_account = if profile.credential_required {
        Some(credential_account_for(&profile.id, &profile.base_url)?)
    } else {
        None
    };
    plan_config_with_account(
        existing_config,
        profile,
        selected_model,
        catalog_path,
        credential_helper,
        credential_account.as_deref(),
    )
}

pub fn plan_proxy_config(
    existing_config: &str,
    upstream_profile: &ProviderProfile,
    selected_model: &str,
    catalog_path: &Path,
    credential_helper: &Path,
    proxy_base_url: &str,
) -> Result<ConfigPlan> {
    validate_profile(upstream_profile)?;
    validate_proxy_base_url(proxy_base_url)?;
    let proxy_profile = ProviderProfile {
        id: LOCAL_PROXY_PROVIDER_ID.to_string(),
        display_name: LOCAL_PROXY_PROVIDER_NAME.to_string(),
        base_url: proxy_base_url.trim_end_matches('/').to_string(),
        models: upstream_profile.models.clone(),
        supports_websockets: false,
        credential_required: true,
    };
    let account = proxy_credential_account_for(proxy_base_url)?;
    plan_config_with_account(
        existing_config,
        &proxy_profile,
        selected_model,
        catalog_path,
        Some(credential_helper),
        Some(&account),
    )
}

fn plan_config_with_account(
    existing_config: &str,
    profile: &ProviderProfile,
    selected_model: &str,
    catalog_path: &Path,
    credential_helper: Option<&Path>,
    credential_account: Option<&str>,
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
        if credential_account.is_none() {
            return Err(SwitcherError::Validation(
                "a credential account is required for this provider".to_string(),
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
        args.push(credential_account.expect("validated credential account"));
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
        model_id: Some(selected_model.to_string()),
    })
}

pub fn plan_official_config(
    existing_config: &str,
    existing_catalog: &str,
    profile: &OfficialProfile,
    catalog_path: &Path,
) -> Result<ConfigPlan> {
    validate_official_profile(profile)?;
    if !catalog_path.is_absolute() {
        return Err(SwitcherError::Validation(
            "catalog path must be absolute".to_string(),
        ));
    }

    let mut document = if existing_config.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing_config.parse::<DocumentMut>()?
    };

    document.as_table_mut().remove("model_provider");
    document.as_table_mut().remove("openai_base_url");
    match profile.model_id.as_deref() {
        Some(model_id) => document["model"] = value(model_id),
        None => {
            document.as_table_mut().remove("model");
        }
    }

    if document.as_table().contains_key("model_providers") {
        let providers = document["model_providers"].as_table_mut().ok_or_else(|| {
            SwitcherError::Validation("model_providers must be a TOML table".to_string())
        })?;
        providers.remove("openai");
        if providers.is_empty() {
            document.as_table_mut().remove("model_providers");
        }
    }

    Ok(ConfigPlan {
        expected_config_sha256: sha256_hex(existing_config.as_bytes()),
        rendered_config: document.to_string(),
        catalog_path: catalog_path.to_path_buf(),
        rendered_catalog: existing_catalog.to_string(),
        provider_id: "openai".to_string(),
        model_id: profile.model_id.clone(),
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

pub fn proxy_credential_account_for(base_url: &str) -> Result<String> {
    validate_proxy_base_url(base_url)?;
    let parsed = url::Url::parse(base_url)?;
    let normalized_url = parsed.as_str().trim_end_matches('/');
    let fingerprint = sha256_hex(format!("{LOCAL_PROXY_PROVIDER_ID}\0{normalized_url}").as_bytes());
    Ok(format!("proxy-client-v1-{fingerprint}"))
}

pub fn verify_proxy_config_binding(config: &str, proxy_base_url: &str) -> Result<PathBuf> {
    validate_proxy_base_url(proxy_base_url)?;
    let document = config.parse::<DocumentMut>()?;
    if document
        .get("model_provider")
        .and_then(|item| item.as_str())
        != Some(LOCAL_PROXY_PROVIDER_ID)
    {
        return Err(SwitcherError::Validation(
            "the managed local proxy is not the active provider".to_string(),
        ));
    }
    let provider = document
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(LOCAL_PROXY_PROVIDER_ID))
        .and_then(|item| item.as_table())
        .ok_or_else(|| {
            SwitcherError::Validation("the managed local proxy definition is missing".to_string())
        })?;
    if provider.get("base_url").and_then(|item| item.as_str()) != Some(proxy_base_url)
        || provider.get("wire_api").and_then(|item| item.as_str()) != Some("responses")
        || provider
            .get("supports_websockets")
            .and_then(|item| item.as_bool())
            != Some(false)
    {
        return Err(SwitcherError::Validation(
            "the managed local proxy definition has changed".to_string(),
        ));
    }
    if provider.get("env_key").is_some()
        || provider.get("experimental_bearer_token").is_some()
        || provider.get("requires_openai_auth").is_some()
    {
        return Err(SwitcherError::Validation(
            "the managed local proxy contains an unsupported authentication setting".to_string(),
        ));
    }

    let auth = provider
        .get("auth")
        .and_then(|item| item.as_table())
        .ok_or_else(|| {
            SwitcherError::Validation(
                "the managed local proxy credential command is missing".to_string(),
            )
        })?;
    let command = auth
        .get("command")
        .and_then(|item| item.as_str())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            SwitcherError::Validation(
                "the managed local proxy credential command is invalid".to_string(),
            )
        })?;
    let expected_cwd = command.parent().ok_or_else(|| {
        SwitcherError::Validation(
            "the managed local proxy credential command has no parent".to_string(),
        )
    })?;
    if !auth
        .get("cwd")
        .and_then(|item| item.as_str())
        .is_some_and(|cwd| paths_equivalent(Path::new(cwd), expected_cwd))
        || auth.get("timeout_ms").and_then(|item| item.as_integer()) != Some(5_000)
        || auth
            .get("refresh_interval_ms")
            .and_then(|item| item.as_integer())
            != Some(300_000)
    {
        return Err(SwitcherError::Validation(
            "the managed local proxy credential command has changed".to_string(),
        ));
    }
    let account = proxy_credential_account_for(proxy_base_url)?;
    let expected_args = ["credential", "get", account.as_str()];
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
    if !args_match {
        return Err(SwitcherError::Validation(
            "the managed local proxy credential arguments have changed".to_string(),
        ));
    }
    Ok(command)
}

pub fn refresh_proxy_credential_helper(
    config: &str,
    proxy_base_url: &str,
    credential_helper: &Path,
) -> Result<Option<String>> {
    if !credential_helper.is_absolute() {
        return Err(SwitcherError::Validation(
            "credential helper path must be absolute".to_string(),
        ));
    }
    let current_helper = verify_proxy_config_binding(config, proxy_base_url)?;
    if paths_equivalent(&current_helper, credential_helper) {
        return Ok(None);
    }

    let mut document = config.parse::<DocumentMut>()?;
    let provider = document
        .get_mut("model_providers")
        .and_then(Item::as_table_mut)
        .and_then(|providers| providers.get_mut(LOCAL_PROXY_PROVIDER_ID))
        .and_then(Item::as_table_mut)
        .ok_or_else(|| {
            SwitcherError::Validation("the managed local proxy definition is missing".to_string())
        })?;
    let auth = provider
        .get_mut("auth")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| {
            SwitcherError::Validation(
                "the managed local proxy credential command is missing".to_string(),
            )
        })?;
    auth["command"] = value(path_as_utf8(credential_helper)?);
    auth["cwd"] = value(path_as_utf8(credential_helper.parent().ok_or_else(
        || {
            SwitcherError::Validation(
                "the managed local proxy credential command has no parent".to_string(),
            )
        },
    )?)?);
    let rendered = document.to_string();
    verify_proxy_config_binding(&rendered, proxy_base_url)?;
    Ok(Some(rendered))
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
    let helper_parent = credential_helper.parent().ok_or_else(|| {
        SwitcherError::Validation("credential helper path has no parent".to_string())
    })?;
    let expected_args = ["credential", "get", account];
    let mut matches = 0_usize;

    for (provider_id, item) in providers {
        let Some(provider) = item.as_table() else {
            continue;
        };
        let Some(base_url) = provider.get("base_url").and_then(|item| item.as_str()) else {
            continue;
        };
        let endpoint_account = credential_account_for(provider_id, base_url).ok();
        let proxy_account = (provider_id == LOCAL_PROXY_PROVIDER_ID)
            .then(|| proxy_credential_account_for(base_url).ok())
            .flatten();
        if endpoint_account.as_deref() != Some(account) && proxy_account.as_deref() != Some(account)
        {
            continue;
        }
        let Some(auth) = provider.get("auth").and_then(|item| item.as_table()) else {
            continue;
        };
        let command_matches = auth
            .get("command")
            .and_then(|item| item.as_str())
            .is_some_and(|command| paths_equivalent(Path::new(command), credential_helper));
        let cwd_matches = auth
            .get("cwd")
            .and_then(|item| item.as_str())
            .is_some_and(|cwd| paths_equivalent(Path::new(cwd), helper_parent));
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

fn validate_proxy_base_url(base_url: &str) -> Result<()> {
    validate_base_url(base_url)?;
    let parsed = url::Url::parse(base_url)?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || parsed.port().is_none()
        || parsed.path().trim_end_matches('/') != "/v1"
    {
        return Err(SwitcherError::Validation(
            "local proxy URL must be http://127.0.0.1:<port>/v1".to_string(),
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

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        windows_path_comparison_key(left) == windows_path_comparison_key(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(windows)]
fn windows_path_comparison_key(path: &Path) -> Option<String> {
    let normalized = path.to_str()?.replace('/', "\\");
    let without_verbatim_prefix = if let Some(rest) = normalized.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = normalized.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        normalized
    };
    Some(without_verbatim_prefix.to_lowercase())
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
    fn official_plan_removes_route_hijacks_and_preserves_unrelated_settings() {
        let existing = r#"
model_provider = "vendor"
model = "vendor/code"
openai_base_url = "https://redirect.example/v1"
model_catalog_json = "/opt/codex/my-models.json"
notify = ["demo-hook"]

[mcp_servers.demo]
command = "demo"

[model_providers.openai]
name = "Shadow OpenAI"
base_url = "https://shadow.example/v1"
experimental_bearer_token = "must-not-survive"

[model_providers.vendor]
name = "Vendor"
base_url = "https://vendor.example/v1"
"#;
        let catalog = absolute_test_path("models.json");
        let profile =
            OfficialProfile::new("个人 Plus".to_string(), Some("gpt-5.6-sol".to_string()));
        let plan = plan_official_config(existing, "{\"models\":[]}\n", &profile, &catalog).unwrap();

        assert!(!plan.rendered_config.contains("model_provider ="));
        assert!(!plan.rendered_config.contains("openai_base_url"));
        assert!(!plan.rendered_config.contains("[model_providers.openai]"));
        assert!(!plan.rendered_config.contains("must-not-survive"));
        assert!(plan.rendered_config.contains("model = \"gpt-5.6-sol\""));
        assert!(plan.rendered_config.contains("[mcp_servers.demo]"));
        assert!(plan.rendered_config.contains("notify = [\"demo-hook\"]"));
        assert!(plan.rendered_config.contains("[model_providers.vendor]"));
        assert!(
            plan.rendered_config
                .contains("model_catalog_json = \"/opt/codex/my-models.json\"")
        );
        assert_eq!(plan.rendered_catalog, "{\"models\":[]}\n");
        assert_eq!(plan.model_id.as_deref(), Some("gpt-5.6-sol"));
    }

    #[test]
    fn official_plan_can_leave_model_selection_to_codex() {
        let catalog = absolute_test_path("models.json");
        let profile = OfficialProfile::default_named(None);
        let plan = plan_official_config(
            "model_provider = \"vendor\"\nmodel = \"vendor/code\"\n",
            "",
            &profile,
            &catalog,
        )
        .unwrap();

        assert!(!plan.rendered_config.contains("model_provider"));
        assert!(!plan.rendered_config.contains("model ="));
        assert_eq!(plan.model_id, None);
    }

    #[test]
    fn official_profile_serialization_contains_no_credential_fields() {
        let profile =
            OfficialProfile::new("个人 Plus".to_string(), Some("gpt-5.6-sol".to_string()));
        let rendered = serde_json::to_string(&profile).unwrap();
        let lower = rendered.to_ascii_lowercase();

        assert!(!lower.contains("token"));
        assert!(!lower.contains("secret"));
        assert!(!lower.contains("credential"));
        assert!(!lower.contains("api_key"));
        assert!(!lower.contains("auth"));
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

    #[test]
    fn proxy_plan_keeps_the_upstream_endpoint_and_key_out_of_codex_config() {
        let catalog = absolute_test_path("models.json");
        let helper = absolute_test_path("helper");
        let plan = plan_proxy_config(
            "# preserved\napproval_policy = \"on-request\"\n",
            &profile(),
            "acme/code",
            &catalog,
            &helper,
            "http://127.0.0.1:15722/v1",
        )
        .unwrap();
        let account = proxy_credential_account_for("http://127.0.0.1:15722/v1").unwrap();

        assert!(plan.rendered_config.contains("# preserved"));
        assert!(
            plan.rendered_config
                .contains("model_provider = \"cps-local\"")
        );
        assert!(
            plan.rendered_config
                .contains("base_url = \"http://127.0.0.1:15722/v1\"")
        );
        assert!(plan.rendered_config.contains("supports_websockets = false"));
        assert!(plan.rendered_config.contains(&account));
        assert!(!plan.rendered_config.contains("https://api.acme.test"));
        verify_credential_binding(&plan.rendered_config, &account, &helper).unwrap();
    }

    #[test]
    fn proxy_account_rejects_ambiguous_or_remote_addresses() {
        assert!(proxy_credential_account_for("http://localhost:15722/v1").is_err());
        assert!(proxy_credential_account_for("http://[::1]:15722/v1").is_err());
        assert!(proxy_credential_account_for("https://127.0.0.1:15722/v1").is_err());
        assert!(proxy_credential_account_for("http://127.0.0.1:15722/other").is_err());
    }

    #[test]
    fn proxy_binding_rejects_managed_field_changes() {
        let catalog = absolute_test_path("models.json");
        let helper = absolute_test_path("helper");
        let base_url = "http://127.0.0.1:15722/v1";
        let plan =
            plan_proxy_config("", &profile(), "acme/code", &catalog, &helper, base_url).unwrap();

        assert_eq!(
            verify_proxy_config_binding(&plan.rendered_config, base_url).unwrap(),
            helper
        );
        assert!(
            verify_proxy_config_binding(
                &plan.rendered_config.replace(
                    "wire_api = \"responses\"",
                    "wire_api = \"chat_completions\"",
                ),
                base_url,
            )
            .is_err()
        );
        assert!(
            verify_proxy_config_binding(
                &plan
                    .rendered_config
                    .replace("supports_websockets = false", "supports_websockets = true"),
                base_url,
            )
            .is_err()
        );
        assert!(
            verify_proxy_config_binding(
                &plan
                    .rendered_config
                    .replace("\"credential\", \"get\"", "\"credential\", \"delete\"",),
                base_url,
            )
            .is_err()
        );
    }

    #[test]
    fn proxy_helper_refresh_changes_only_the_managed_command_location() {
        let catalog = absolute_test_path("models.json");
        let old_helper = absolute_test_path("old/helper");
        let new_helper = absolute_test_path("new/helper");
        let base_url = "http://127.0.0.1:15722/v1";
        let plan = plan_proxy_config(
            "approval_policy = \"never\"\n",
            &profile(),
            "acme/code",
            &catalog,
            &old_helper,
            base_url,
        )
        .unwrap();

        let refreshed =
            refresh_proxy_credential_helper(&plan.rendered_config, base_url, &new_helper)
                .unwrap()
                .unwrap();
        assert!(refreshed.contains("approval_policy = \"never\""));
        assert!(!refreshed.contains(old_helper.to_str().unwrap()));
        assert_eq!(
            verify_proxy_config_binding(&refreshed, base_url).unwrap(),
            new_helper
        );
        assert!(
            refresh_proxy_credential_helper(&refreshed, base_url, &new_helper)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_helper_path_matches_managed_configuration() {
        let catalog = absolute_test_path("models.json");
        let configured_helper =
            PathBuf::from(r"C:\codex-provider-switcher-tests\helpers\current\helper.exe");
        let canonical_helper =
            PathBuf::from(r"\\?\c:\CODEX-PROVIDER-SWITCHER-TESTS\helpers\current\helper.exe");
        let base_url = "http://127.0.0.1:15722/v1";
        assert!(paths_equivalent(
            Path::new(r"C:/CODEX-PROVIDER-SWITCHER-TESTS/helpers/current/helper.exe"),
            &configured_helper
        ));
        let plan = plan_proxy_config(
            "",
            &profile(),
            "acme/code",
            &catalog,
            &configured_helper,
            base_url,
        )
        .unwrap();
        let account = proxy_credential_account_for(base_url).unwrap();

        verify_credential_binding(&plan.rendered_config, &account, &canonical_helper).unwrap();
        assert!(
            refresh_proxy_credential_helper(&plan.rendered_config, base_url, &canonical_helper)
                .unwrap()
                .is_none()
        );
    }
}
