mod codex_account;

use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use codex_account::login_chatgpt;
use codex_account::logout_chatgpt;
use codex_account::read_account;
use codex_provider_switcher_core::AuthKind;
use codex_provider_switcher_core::BackupManifest;
use codex_provider_switcher_core::BackupStatus;
use codex_provider_switcher_core::ConfigPlan;
use codex_provider_switcher_core::CurrentCodexConfig;
use codex_provider_switcher_core::FetchedModel;
use codex_provider_switcher_core::LOCAL_PROXY_PROVIDER_ID;
use codex_provider_switcher_core::ModelSpec;
use codex_provider_switcher_core::OFFICIAL_PROFILE_DISPLAY_NAME;
use codex_provider_switcher_core::OfficialProfile;
use codex_provider_switcher_core::ProfileStore;
use codex_provider_switcher_core::ProviderProfile;
use codex_provider_switcher_core::ReasoningEffort;
use codex_provider_switcher_core::RecoveryOutcome;
use codex_provider_switcher_core::apply_config_plan;
use codex_provider_switcher_core::apply_config_plan_with_transaction_id;
use codex_provider_switcher_core::backup_matches_applied;
use codex_provider_switcher_core::create_private_directory;
use codex_provider_switcher_core::credential_account_for;
use codex_provider_switcher_core::fetch_models;
use codex_provider_switcher_core::inspect_config;
use codex_provider_switcher_core::model_endpoint_candidates;
use codex_provider_switcher_core::normalize_api_base_url;
use codex_provider_switcher_core::parse_profile_store;
use codex_provider_switcher_core::plan_config;
use codex_provider_switcher_core::plan_official_config;
use codex_provider_switcher_core::plan_proxy_config;
use codex_provider_switcher_core::proxy_credential_account_for;
use codex_provider_switcher_core::recover_prepared_backup;
use codex_provider_switcher_core::refresh_proxy_credential_helper_file;
use codex_provider_switcher_core::remove_profile;
use codex_provider_switcher_core::render_profile_store;
use codex_provider_switcher_core::restore_backup;
use codex_provider_switcher_core::restore_proxy_config_preserving_unrelated_changes;
use codex_provider_switcher_core::upsert_profile;
use codex_provider_switcher_core::validate_official_profile;
use codex_provider_switcher_core::verify_backup_integrity;
use codex_provider_switcher_core::verify_credential_binding as verify_core_credential_binding;
use codex_provider_switcher_core::verify_proxy_config_binding as verify_core_proxy_config_binding;
use codex_provider_switcher_core::verify_proxy_detach_recoverable;
use codex_provider_switcher_core::write_private_file;
use codex_provider_switcher_core::{CodexAccountStatus, CodexAuthMode};
use codex_provider_switcher_credentials as credentials;
use codex_provider_switcher_launcher::authorize_codex_parent;
use codex_provider_switcher_launcher::open_codex as launch_codex;
use codex_provider_switcher_local_proxy::BearerToken;
use codex_provider_switcher_local_proxy::LocalProxy;
use codex_provider_switcher_local_proxy::ModelDescriptor;
use codex_provider_switcher_local_proxy::ProxyHandle;
use codex_provider_switcher_local_proxy::ProxyStartOptions;
use codex_provider_switcher_local_proxy::ReasoningLevelDescriptor;
use codex_provider_switcher_local_proxy::RouteConfig;
use directories::BaseDirs;
use fs4::FileExt;
use rand::RngCore;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tempfile::NamedTempFile;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;
use zeroize::Zeroize;
use zeroize::Zeroizing;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppState {
    config_path: PathBuf,
    config_exists: bool,
    latest_backup: Option<PathBuf>,
    recovery_warnings: usize,
    current: CurrentCodexConfig,
    profiles: Vec<ProviderProfile>,
    profile_warning: Option<String>,
    official_profile: Option<OfficialProfile>,
    official_profile_warning: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplySummary {
    transaction_id: String,
    backup_manifest: PathBuf,
    manifest_finalized: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoverInput {
    base_url: String,
    secret: String,
}

impl Drop for DiscoverInput {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoverySummary {
    session_id: String,
    base_url: String,
    models: Vec<FetchedModel>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CredentialSessionSummary {
    session_id: String,
    base_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalProxyStatus {
    enabled: bool,
    running: bool,
    recovery_required: bool,
    manual_recovery_required: bool,
    current_profile_id: Option<String>,
    current_model_id: Option<String>,
    requires_codex_restart: bool,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredProxyState {
    schema_version: u32,
    enabled: bool,
    profile_id: Option<String>,
    model_id: Option<String>,
    #[serde(default)]
    activation_transaction_id: Option<Uuid>,
    port: u16,
    revision: u64,
    #[serde(default)]
    requires_codex_restart: bool,
}

impl Default for StoredProxyState {
    fn default() -> Self {
        Self {
            schema_version: PROXY_STATE_SCHEMA_VERSION,
            enabled: false,
            profile_id: None,
            model_id: None,
            activation_transaction_id: None,
            port: LOCAL_PROXY_PORT,
            revision: 0,
            requires_codex_restart: false,
        }
    }
}

#[derive(Default)]
struct ProxyRuntime {
    inner: AsyncMutex<ProxyRuntimeState>,
}

#[derive(Default)]
struct ProxyRuntimeState {
    handle: Option<ProxyHandle>,
    last_error: Option<String>,
}

#[derive(Default)]
struct AccountRuntime {
    inner: AsyncMutex<()>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveProfileInput {
    profile: ProviderProfile,
    discovery_id: Option<String>,
}

struct PendingDiscovery {
    base_url: String,
    secret: Zeroizing<String>,
    created_at: Instant,
}

#[derive(Clone, Default)]
struct DiscoveryVault(Arc<Mutex<HashMap<Uuid, PendingDiscovery>>>);

const DISCOVERY_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_DISCOVERY_SESSIONS: usize = 8;
const PROXY_STATE_SCHEMA_VERSION: u32 = 2;
const LOCAL_PROXY_PORT: u16 = 15_722;
const LOCAL_PROXY_MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const AUTOSTART_NAME: &str = "Codex Provider Switcher";
const MISSING_SAVED_PROVIDER_CREDENTIAL: &str =
    "saved provider credential is missing; edit the connection and enter the API Key again";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyConfigState {
    NotSelected,
    Managed,
    Changed,
}

#[tauri::command]
fn inspect_state() -> Result<AppState, String> {
    let paths = app_paths()?;
    let recovery_warnings = recover_prepared_manifests(&paths);
    let existing = read_config_safely(&paths.config)?;
    let current = inspect_config(&existing).map_err(redacted_core_error)?;
    let (profiles, profile_warning) = match load_profiles(&paths) {
        Ok(store) => (store.profiles, None),
        Err(_) => (
            Vec::new(),
            Some(
                "saved connections could not be read; the original file was left unchanged"
                    .to_string(),
            ),
        ),
    };
    let (official_profile, official_profile_warning) = match load_official_profile(&paths) {
        Ok(profile) => (profile, None),
        Err(_) => (
            None,
            Some(
                "saved official configuration could not be read; the original file was left unchanged"
                    .to_string(),
            ),
        ),
    };
    Ok(AppState {
        config_exists: paths.config.is_file(),
        latest_backup: latest_applied_manifest(&paths)?,
        recovery_warnings,
        config_path: paths.config,
        current,
        profiles,
        profile_warning,
        official_profile,
        official_profile_warning,
    })
}

fn apply_profile(profile: ProviderProfile, selected_model: String) -> Result<ApplySummary, String> {
    if profile.credential_required {
        let account =
            credential_account_for(&profile.id, &profile.base_url).map_err(redacted_core_error)?;
        if !credentials::exists(&account)? {
            return Err(MISSING_SAVED_PROVIDER_CREDENTIAL.to_string());
        }
    }
    let paths = app_paths()?;
    require_clean_recovery_state(&paths)?;
    if profile.credential_required {
        ensure_stable_helper(&paths)?;
    }
    let existing = read_config_safely(&paths.config)?;
    let plan = plan_config(
        &existing,
        &profile,
        &selected_model,
        &paths.catalog,
        Some(&paths.helper),
    )
    .map_err(redacted_core_error)?;
    apply_plan(&paths, &plan)
}

fn apply_plan(paths: &AppPaths, plan: &ConfigPlan) -> Result<ApplySummary, String> {
    let mut result =
        apply_config_plan(&paths.config, &paths.backups, plan).map_err(redacted_core_error)?;
    if !result.manifest_finalized {
        match recover_prepared_backup(&result.manifest_path, &paths.config, &paths.catalog)
            .map_err(redacted_core_error)?
        {
            RecoveryOutcome::FinalizedApplied => result.manifest_finalized = true,
            RecoveryOutcome::RolledBack => {
                return Err(
                    "the configuration changed incompletely and was safely rolled back".to_string(),
                );
            }
            RecoveryOutcome::NotNeeded => {
                return Err(
                    "the configuration recovery record did not reach its expected state"
                        .to_string(),
                );
            }
        }
    }
    Ok(ApplySummary {
        transaction_id: result.transaction_id.to_string(),
        backup_manifest: result.manifest_path,
        manifest_finalized: result.manifest_finalized,
    })
}

fn apply_official_profile(profile: OfficialProfile) -> Result<ApplySummary, String> {
    validate_official_profile(&profile).map_err(redacted_core_error)?;
    let paths = app_paths()?;
    require_clean_recovery_state(&paths)?;
    require_proxy_detached(&paths)?;
    let existing = read_config_safely(&paths.config)?;
    let existing_catalog = read_internal_catalog_safely(&paths.catalog)?;
    let plan = plan_official_config(&existing, &existing_catalog, &profile, &paths.catalog)
        .map_err(redacted_core_error)?;
    apply_plan(&paths, &plan)
}

fn require_proxy_detached(paths: &AppPaths) -> Result<(), String> {
    match load_proxy_state(paths) {
        Ok(state)
            if state.enabled
                || current_proxy_config_state(paths, LOCAL_PROXY_PORT)?
                    != ProxyConfigState::NotSelected =>
        {
            Err("close fast switching before activating the official login".to_string())
        }
        Err(_) => {
            Err("repair or close fast switching before activating the official login".to_string())
        }
        _ => Ok(()),
    }
}

#[tauri::command]
async fn discover_models(
    mut input: DiscoverInput,
    vault: tauri::State<'_, DiscoveryVault>,
) -> Result<DiscoverySummary, String> {
    let discovery = fetch_models(&input.base_url, &input.secret).await?;
    let base_url = discovery.base_url;
    let session_id = insert_discovery(&vault, base_url.clone(), std::mem::take(&mut input.secret))?;
    Ok(DiscoverySummary {
        session_id: session_id.to_string(),
        base_url,
        models: discovery.models,
    })
}

#[tauri::command]
fn stage_credential(
    mut input: DiscoverInput,
    vault: tauri::State<'_, DiscoveryVault>,
) -> Result<CredentialSessionSummary, String> {
    model_endpoint_candidates(&input.base_url)?;
    credentials::validate_secret(input.secret.trim())?;
    let base_url = normalize_api_base_url(&input.base_url)?;
    let session_id = insert_discovery(&vault, base_url.clone(), std::mem::take(&mut input.secret))?;
    Ok(CredentialSessionSummary {
        session_id: session_id.to_string(),
        base_url,
    })
}

#[tauri::command]
fn cancel_discovery(
    session_id: String,
    vault: tauri::State<'_, DiscoveryVault>,
) -> Result<(), String> {
    let session_id =
        Uuid::parse_str(&session_id).map_err(|_| "invalid model discovery session".to_string())?;
    let mut sessions = vault
        .0
        .lock()
        .map_err(|_| "temporary credential storage is unavailable".to_string())?;
    sessions.remove(&session_id);
    Ok(())
}

#[tauri::command]
fn load_profile_credential(profile_id: String) -> Result<String, String> {
    let paths = app_paths()?;
    let store = load_profiles(&paths)?;
    let account = profile_credential_account(&store, &profile_id)?;
    credentials::get(&account).map_err(map_missing_provider_credential)
}

fn profile_credential_account(store: &ProfileStore, profile_id: &str) -> Result<String, String> {
    let profile = store
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "the saved connection no longer exists".to_string())?;
    if !profile.credential_required {
        return Err("this saved connection does not use an API Key".to_string());
    }
    credential_account_for(&profile.id, &profile.base_url).map_err(redacted_core_error)
}

#[tauri::command]
fn save_profile(
    input: SaveProfileInput,
    vault: tauri::State<'_, DiscoveryVault>,
) -> Result<(), String> {
    let paths = app_paths()?;
    if let Ok(state) = load_proxy_state(&paths) {
        validate_active_profile_replacement(&state, &input.profile)?;
    }
    let _profile_lock = lock_profiles(&paths)?;
    let mut store = load_profiles(&paths)?;
    upsert_profile(&mut store, input.profile.clone()).map_err(redacted_core_error)?;
    let rendered = render_profile_store(&store).map_err(redacted_core_error)?;
    let pending = take_discovery(&vault, input.discovery_id)?;

    if input.profile.credential_required {
        let account = credential_account_for(&input.profile.id, &input.profile.base_url)
            .map_err(redacted_core_error)?;
        if let Some(pending) = pending {
            if normalized_endpoint(&pending.base_url)
                != normalized_endpoint(&input.profile.base_url)
            {
                return Err(
                    "the Base URL changed after models were fetched; fetch models again"
                        .to_string(),
                );
            }
            let previous = if credentials::exists(&account)? {
                Some(Zeroizing::new(credentials::get(&account)?))
            } else {
                None
            };
            credentials::store(&account, pending.secret.as_str())?;
            if let Err(error) = write_profiles(&paths, &rendered) {
                let rollback = match previous {
                    Some(secret) => credentials::store(&account, secret.as_str()),
                    None => credentials::delete(&account),
                };
                return Err(if rollback.is_ok() {
                    error
                } else {
                    "could not save the shortcut or restore the previous system credential"
                        .to_string()
                });
            }
            return Ok(());
        } else if !credentials::exists(&account)? {
            return Err("enter the API Key and fetch models before saving".to_string());
        }
    }

    write_profiles(&paths, &rendered)
}

fn validate_active_profile_replacement(
    state: &StoredProxyState,
    profile: &ProviderProfile,
) -> Result<(), String> {
    if !state.enabled || state.profile_id.as_deref() != Some(profile.id.as_str()) {
        return Ok(());
    }
    let current_model = state
        .model_id
        .as_deref()
        .ok_or_else(|| "the active fast-switch route has no selected model".to_string())?;
    if profile.models.iter().any(|model| model.id == current_model) {
        Ok(())
    } else {
        Err("keep the active model in this connection until a new model is selected".to_string())
    }
}

#[tauri::command]
fn apply_saved_profile(profile_id: String, selected_model: String) -> Result<ApplySummary, String> {
    let paths = app_paths()?;
    require_proxy_detached(&paths).map_err(|_| {
        "close or repair fast switching before writing a direct configuration".to_string()
    })?;
    let profile = load_profiles(&paths)?
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "the saved connection no longer exists".to_string())?;
    apply_profile(profile, selected_model)
}

#[tauri::command]
fn prepare_official_login() -> Result<ApplySummary, String> {
    apply_official_profile(OfficialProfile::default_named(None))
}

#[tauri::command]
fn activate_official_profile() -> Result<ApplySummary, String> {
    let paths = app_paths()?;
    let profile = load_official_profile(&paths)?
        .ok_or_else(|| "save an official configuration before activating it".to_string())?;
    apply_official_profile(profile)
}

#[tauri::command]
async fn codex_account_status(
    runtime: tauri::State<'_, AccountRuntime>,
) -> Result<CodexAccountStatus, String> {
    let paths = app_paths()?;
    let _guard = runtime.inner.lock().await;
    let codex_home = paths.codex_home.clone();
    let status = tauri::async_runtime::spawn_blocking(move || read_account(&codex_home))
        .await
        .map_err(|_| "Codex account inspection worker failed".to_string())??;
    if status.auth_mode == CodexAuthMode::Chatgpt {
        let current =
            inspect_config(&read_config_safely(&paths.config)?).map_err(redacted_core_error)?;
        if current.provider_id == "openai"
            && current.base_url.is_none()
            && current.auth_kind == AuthKind::OfficialLogin
        {
            refresh_official_account_metadata_if_readable(&paths, &current, &status)?;
        }
    }
    Ok(status)
}

#[tauri::command]
async fn login_official_account(
    runtime: tauri::State<'_, AccountRuntime>,
) -> Result<CodexAccountStatus, String> {
    let paths = app_paths()?;
    let _guard = runtime.inner.lock().await;
    require_builtin_openai_route(&paths)?;
    let codex_home = paths.codex_home.clone();
    let status = tauri::async_runtime::spawn_blocking(move || login_chatgpt(&codex_home))
        .await
        .map_err(|_| "Codex login worker failed".to_string())??;
    let current = require_builtin_openai_route(&paths).map_err(|error| {
        format!("Codex login succeeded, but the official configuration was not saved: {error}")
    })?;
    save_official_account_metadata(&paths, &current, &status).map_err(|error| {
        format!("Codex login succeeded, but the official configuration was not saved: {error}")
    })?;
    Ok(status)
}

#[tauri::command]
async fn logout_official_account(
    runtime: tauri::State<'_, AccountRuntime>,
) -> Result<CodexAccountStatus, String> {
    let paths = app_paths()?;
    let _guard = runtime.inner.lock().await;
    require_builtin_openai_route(&paths)?;
    let codex_home = paths.codex_home.clone();
    let status = tauri::async_runtime::spawn_blocking(move || logout_chatgpt(&codex_home))
        .await
        .map_err(|_| "Codex logout worker failed".to_string())??;
    remove_official_profile_metadata(&paths).map_err(|error| {
        format!(
            "Codex logged out the current ChatGPT account, but the saved official configuration could not be removed: {error}"
        )
    })?;
    Ok(status)
}

fn require_builtin_openai_route(paths: &AppPaths) -> Result<CurrentCodexConfig, String> {
    require_clean_recovery_state(paths)?;
    require_proxy_detached(paths)?;
    let current =
        inspect_config(&read_config_safely(&paths.config)?).map_err(redacted_core_error)?;
    if current.provider_id != "openai"
        || current.base_url.is_some()
        || current.auth_kind != AuthKind::OfficialLogin
    {
        return Err("prepare the built-in OpenAI route before managing official login".to_string());
    }
    Ok(current)
}

#[tauri::command]
async fn proxy_status(runtime: tauri::State<'_, ProxyRuntime>) -> Result<LocalProxyStatus, String> {
    let paths = app_paths()?;
    let mut runtime = runtime.inner.lock().await;
    let config_state = match current_proxy_config_state(&paths, LOCAL_PROXY_PORT) {
        Ok(state) => state,
        Err(error) => {
            if let Some(handle) = runtime.handle.take() {
                let _ = handle.shutdown().await;
            }
            return match load_proxy_state(&paths) {
                Ok(state) => {
                    runtime.last_error = Some(error);
                    let mut status = proxy_status_from(&state, &runtime);
                    status.recovery_required = true;
                    status.manual_recovery_required = true;
                    Ok(status)
                }
                Err(_) => Ok(LocalProxyStatus {
                    enabled: true,
                    running: false,
                    recovery_required: true,
                    manual_recovery_required: true,
                    current_profile_id: None,
                    current_model_id: None,
                    requires_codex_restart: false,
                    last_error: Some(
                        "Codex settings and fast-switch recovery state could not be read safely"
                            .to_string(),
                    ),
                }),
            };
        }
    };
    let config_selected = config_state != ProxyConfigState::NotSelected;
    let state = match load_proxy_state(&paths) {
        Ok(state) => state,
        Err(error) => {
            if let Some(handle) = runtime.handle.take() {
                let _ = handle.shutdown().await;
            }
            let fallback_state = StoredProxyState::default();
            return Ok(LocalProxyStatus {
                enabled: true,
                running: false,
                recovery_required: true,
                manual_recovery_required: config_selected
                    && !proxy_activation_can_restore_automatically(&paths, &fallback_state),
                current_profile_id: None,
                current_model_id: None,
                requires_codex_restart: config_selected,
                last_error: Some(error),
            });
        }
    };
    if state.enabled && config_selected {
        if let Err(error) = prepare_active_proxy_configuration(&paths, &state) {
            if let Some(handle) = runtime.handle.take() {
                let _ = handle.shutdown().await;
            }
            runtime.last_error = Some(error);
            let mut status = proxy_status_from(&state, &runtime);
            status.recovery_required = true;
            status.manual_recovery_required =
                !proxy_activation_can_restore_automatically(&paths, &state);
            return Ok(status);
        }
        if runtime
            .handle
            .as_ref()
            .is_none_or(|handle| !handle.health().running)
        {
            match load_proxy_route(&paths, &state) {
                Ok((_, route)) => {
                    if let Err(error) = start_proxy_handle(&state, &mut runtime, route, false).await
                    {
                        runtime.last_error = Some(error);
                    }
                }
                Err(error) => runtime.last_error = Some(error),
            }
        }
        if runtime
            .handle
            .as_ref()
            .is_some_and(|handle| handle.health().running)
        {
            runtime.last_error = None;
        }
    } else if state.enabled {
        if let Some(handle) = runtime.handle.take() {
            let _ = handle.shutdown().await;
        }
        runtime.last_error =
            Some("fast-switch setup is incomplete; choose a connection again".to_string());
        let mut status = proxy_status_from(&state, &runtime);
        status.recovery_required = true;
        if let Ok(manifest_path) = proxy_activation_manifest_path(&paths, &state)
            && let Ok(manifest) = read_proxy_activation_manifest_metadata(&paths, &manifest_path)
            && manifest.status != BackupStatus::Applied
        {
            status.manual_recovery_required =
                !proxy_activation_can_restore_automatically(&paths, &state);
        }
        return Ok(status);
    } else if config_selected {
        if let Some(handle) = runtime.handle.take() {
            let _ = handle.shutdown().await;
        }
        runtime.last_error = Some(
            "Codex points to the local proxy but its recovery state is incomplete".to_string(),
        );
        let mut status = proxy_status_from(&state, &runtime);
        status.enabled = true;
        status.recovery_required = true;
        status.manual_recovery_required =
            !proxy_activation_can_restore_automatically(&paths, &state);
        return Ok(status);
    } else if let Some(handle) = runtime.handle.take() {
        let _ = handle.shutdown().await;
    }
    Ok(proxy_status_from(&state, &runtime))
}

#[tauri::command]
async fn enable_proxy(
    profile_id: String,
    selected_model: String,
    runtime: tauri::State<'_, ProxyRuntime>,
    app: tauri::AppHandle,
) -> Result<LocalProxyStatus, String> {
    let paths = app_paths()?;
    let mut runtime = runtime.inner.lock().await;
    require_clean_recovery_state(&paths)?;
    ensure_stable_helper(&paths)?;
    let existing = read_config_safely(&paths.config)?;
    let current = inspect_config(&existing).map_err(redacted_core_error)?;
    let proxy_base_url = proxy_base_url(LOCAL_PROXY_PORT);
    let config_needs_write = current.provider_id != LOCAL_PROXY_PROVIDER_ID
        || current.base_url.as_deref() != Some(proxy_base_url.as_str());
    if current.provider_id == LOCAL_PROXY_PROVIDER_ID && config_needs_write {
        return Err(
            "the reserved local proxy provider has changed; repair it before enabling fast switching"
                .to_string(),
        );
    }
    let previous = load_proxy_state(&paths).map_err(|_| {
        "fast-switch recovery state is damaged; repair it before enabling fast switching"
            .to_string()
    })?;
    if previous.enabled && config_needs_write {
        return Err(
            "close the incomplete fast-switch activation before enabling a new one".to_string(),
        );
    }
    let requested = proxy_state_for_selection(&previous, &profile_id, &selected_model);
    let (profile, route) = load_proxy_route(&paths, &requested)?;
    let plan = config_needs_write
        .then(|| {
            plan_proxy_config(
                &existing,
                &profile,
                &selected_model,
                &paths.catalog,
                &paths.helper,
                &proxy_base_url,
            )
            .map_err(redacted_core_error)
        })
        .transpose()?;

    let mut next = requested;
    next.requires_codex_restart |= config_needs_write;
    next.activation_transaction_id = if config_needs_write {
        Some(Uuid::new_v4())
    } else {
        previous
            .activation_transaction_id
            .or(recover_proxy_activation_transaction(&paths)?)
    };
    if !config_needs_write && next.activation_transaction_id.is_none() {
        return Err(
            "Codex already points to the local proxy, but no safe restore point is available"
                .to_string(),
        );
    }
    if !config_needs_write {
        prepare_active_proxy_configuration(&paths, &next)?;
    }
    start_proxy_handle(&next, &mut runtime, route, true).await?;

    if enable_background_startup(&app).is_err() {
        let _ = disable_background_startup(&app);
        rollback_proxy_runtime(&paths, &previous, &mut runtime).await;
        return Err("could not enable automatic startup for fast switching".to_string());
    }
    if let Err(error) = write_proxy_state(&paths, &next) {
        if !previous.enabled {
            let _ = disable_background_startup(&app);
        }
        rollback_proxy_runtime(&paths, &previous, &mut runtime).await;
        return Err(error);
    }
    if let Some(plan) = plan {
        let transaction_id = next
            .activation_transaction_id
            .expect("a new proxy configuration has a preallocated transaction ID");
        match apply_config_plan_with_transaction_id(
            &paths.config,
            &paths.backups,
            &plan,
            transaction_id,
        ) {
            Ok(result) => {
                debug_assert_eq!(result.transaction_id, transaction_id);
                if !result.manifest_finalized {
                    match recover_prepared_backup(
                        &result.manifest_path,
                        &paths.config,
                        &paths.catalog,
                    ) {
                        Ok(RecoveryOutcome::FinalizedApplied) => {}
                        Ok(RecoveryOutcome::RolledBack) => {
                            let _ = write_proxy_state(&paths, &previous);
                            if !previous.enabled {
                                let _ = disable_background_startup(&app);
                            }
                            rollback_proxy_runtime(&paths, &previous, &mut runtime).await;
                            return Err(
                                "fast-switch configuration was safely rolled back before activation"
                                    .to_string(),
                            );
                        }
                        Ok(RecoveryOutcome::NotNeeded) | Err(_) => {
                            runtime.last_error = Some(
                                "fast-switch recovery record could not be finalized".to_string(),
                            );
                            return Err(
                                "fast switching was configured, but its recovery record could not be finalized; refresh status before continuing"
                                    .to_string(),
                            );
                        }
                    }
                }
            }
            Err(error) => {
                let _ = write_proxy_state(&paths, &previous);
                if !previous.enabled {
                    let _ = disable_background_startup(&app);
                }
                rollback_proxy_runtime(&paths, &previous, &mut runtime).await;
                return Err(redacted_core_error(error));
            }
        }
    }

    runtime.last_error = None;
    Ok(proxy_status_from(&next, &runtime))
}

#[tauri::command]
async fn switch_proxy_route(
    profile_id: String,
    selected_model: String,
    runtime: tauri::State<'_, ProxyRuntime>,
) -> Result<LocalProxyStatus, String> {
    let paths = app_paths()?;
    let mut runtime = runtime.inner.lock().await;
    let previous = load_proxy_state(&paths)?;
    if !previous.enabled {
        return Err("fast switching is not enabled".to_string());
    }
    if current_proxy_config_state(&paths, previous.port)? != ProxyConfigState::Managed {
        return Err(
            "fast-switch setup is incomplete; enable it again before switching models".to_string(),
        );
    }
    prepare_active_proxy_configuration(&paths, &previous)?;
    let next = proxy_state_for_selection(&previous, &profile_id, &selected_model);
    let (_, route) = load_proxy_route(&paths, &next)?;
    start_proxy_handle(&next, &mut runtime, route, false).await?;
    if let Err(error) = write_proxy_state(&paths, &next) {
        rollback_proxy_runtime(&paths, &previous, &mut runtime).await;
        return Err(error);
    }
    runtime.last_error = None;
    Ok(proxy_status_from(&next, &runtime))
}

#[tauri::command]
async fn disable_proxy(
    runtime: tauri::State<'_, ProxyRuntime>,
    app: tauri::AppHandle,
) -> Result<LocalProxyStatus, String> {
    let paths = app_paths()?;
    let mut runtime = runtime.inner.lock().await;
    let (previous, state_was_damaged) = match load_proxy_state(&paths) {
        Ok(state) => (state, false),
        Err(_) => (StoredProxyState::default(), true),
    };
    let config_state = current_proxy_config_state(&paths, previous.port)?;
    let config_selected = config_state != ProxyConfigState::NotSelected;
    if !previous.enabled && !config_selected && !state_was_damaged {
        let autostart_error = disable_background_startup(&app).err();
        if let Some(handle) = runtime.handle.take() {
            let _ = handle.shutdown().await;
        }
        runtime.last_error = autostart_error.map(|_| {
            "automatic startup could not be removed; fast switching remains disabled".into()
        });
        return Ok(proxy_status_from(&previous, &runtime));
    }

    if config_selected {
        restore_proxy_activation(&paths, &previous)?;
    } else if previous.enabled
        && let Ok(manifest_path) = proxy_activation_manifest_path(&paths, &previous)
        && read_proxy_activation_manifest_metadata(&paths, &manifest_path)?.status
            != BackupStatus::Applied
    {
        finalize_proxy_restore_journal(&paths, &manifest_path)?;
    }
    if current_proxy_config_state(&paths, previous.port)? == ProxyConfigState::Managed {
        return Err(
            "fast-switch restore completed without detaching the managed local proxy".to_string(),
        );
    }
    let next = StoredProxyState {
        enabled: false,
        profile_id: None,
        model_id: None,
        activation_transaction_id: None,
        revision: previous.revision.saturating_add(1),
        requires_codex_restart: config_selected
            || previous.enabled
            || previous.requires_codex_restart,
        ..previous
    };
    let state_write_error = write_proxy_state(&paths, &next).err();
    let autostart_error = disable_background_startup(&app).err();
    if let Some(handle) = runtime.handle.take() {
        let _ = handle.shutdown().await;
    }
    if state_write_error.is_some() {
        runtime.last_error =
            Some("Codex settings were restored, but fast-switch cleanup is incomplete".to_string());
        return Err(
            "Codex settings were restored, but fast-switch cleanup is incomplete; choose close fast switching again"
                .to_string(),
        );
    }
    runtime.last_error = autostart_error
        .map(|_| "automatic startup could not be removed; fast switching remains disabled".into());
    Ok(proxy_status_from(&next, &runtime))
}

#[tauri::command]
fn delete_saved_profile(profile_id: String) -> Result<bool, String> {
    let paths = app_paths()?;
    match load_proxy_state(&paths) {
        Ok(state) if state.enabled && state.profile_id.as_deref() == Some(profile_id.as_str()) => {
            return Err(
                "switch to another connection before removing the active fast-switch route"
                    .to_string(),
            );
        }
        Err(_) => {
            return Err(
                "repair or close fast switching before removing a saved connection".to_string(),
            );
        }
        _ => {}
    }
    let _profile_lock = lock_profiles(&paths)?;
    let mut store = load_profiles(&paths)?;
    let removed = remove_profile(&mut store, &profile_id).map_err(redacted_core_error)?;
    if removed {
        let rendered = render_profile_store(&store).map_err(redacted_core_error)?;
        write_profiles(&paths, &rendered)?;
    }
    Ok(removed)
}

#[tauri::command]
fn restore_latest() -> Result<String, String> {
    let paths = app_paths()?;
    let manifest = latest_applied_manifest(&paths)?
        .ok_or_else(|| "no applied switcher backup is available".to_string())?;
    let result =
        restore_backup(&manifest, &paths.config, &paths.catalog).map_err(redacted_core_error)?;
    if !result.manifest_finalized {
        recover_prepared_backup(&manifest, &paths.config, &paths.catalog)
            .map_err(redacted_core_error)?;
    }
    Ok(result.transaction_id.to_string())
}

#[tauri::command]
fn open_codex() -> Result<String, String> {
    let launched = launch_codex()?;
    if let Ok(paths) = app_paths()
        && let Ok(mut state) = load_proxy_state(&paths)
        && state.requires_codex_restart
    {
        state.requires_codex_restart = false;
        state.revision = state.revision.saturating_add(1);
        let _ = write_proxy_state(&paths, &state);
    }
    Ok(launched)
}

pub fn run() {
    let background =
        std::env::args_os().any(|argument| argument == std::ffi::OsStr::new("--background"));
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, arguments, _| {
            if !arguments.iter().any(|argument| argument == "--background") {
                show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .manage(DiscoveryVault::default())
        .manage(ProxyRuntime::default())
        .manage(AccountRuntime::default())
        .setup(move |app| {
            setup_tray(app)?;
            start_proxy_on_launch(app.handle().clone(), background);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            inspect_state,
            discover_models,
            stage_credential,
            cancel_discovery,
            load_profile_credential,
            save_profile,
            apply_saved_profile,
            prepare_official_login,
            activate_official_profile,
            codex_account_status,
            login_official_account,
            logout_official_account,
            proxy_status,
            enable_proxy,
            switch_proxy_route,
            disable_proxy,
            delete_saved_profile,
            restore_latest,
            open_codex,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Codex Provider Switcher");
    app.run(move |app, event| match event {
        tauri::RunEvent::Ready if !background => show_main_window(app),
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show_main_window(app),
        _ => {}
    });
}

fn start_proxy_on_launch(app: tauri::AppHandle, background: bool) {
    tauri::async_runtime::spawn(async move {
        use tauri::Manager as _;

        let result = async {
            let runtime = app.state::<ProxyRuntime>();
            let mut runtime = runtime.inner.lock().await;
            let paths = app_paths()?;
            let state = load_proxy_state(&paths)?;
            let config_state = current_proxy_config_state(&paths, state.port)?;
            if !state.enabled {
                if config_state != ProxyConfigState::NotSelected {
                    return Err(
                        "Codex points to the local proxy but its recovery state is incomplete"
                            .to_string(),
                    );
                }
                let _ = disable_background_startup(&app);
                if background {
                    app.exit(0);
                }
                return Ok(());
            }
            if config_state != ProxyConfigState::Managed {
                return Err(
                    "fast-switch setup is incomplete; choose a connection again".to_string()
                );
            }
            prepare_active_proxy_configuration(&paths, &state)?;
            let (_, route) = load_proxy_route(&paths, &state)?;
            start_proxy_handle(&state, &mut runtime, route, false).await?;
            enable_background_startup(&app)
        }
        .await;

        if let Err(error) = result {
            let runtime = app.state::<ProxyRuntime>();
            runtime.inner.lock().await.last_error = Some(error);
            if background {
                show_main_window(&app);
            }
        }
    });
}

fn setup_tray(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let open_item =
        tauri::menu::MenuItem::with_id(app, "open-switcher", "打开模型切换", true, None::<&str>)?;
    let quit_item =
        tauri::menu::MenuItem::with_id(app, "quit-switcher", "退出", true, None::<&str>)?;
    let menu = tauri::menu::Menu::with_items(app, &[&open_item, &quit_item])?;
    let mut tray = tauri::tray::TrayIconBuilder::new()
        .tooltip("Codex 模型切换")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open-switcher" => show_main_window(app),
            "quit-switcher" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    use tauri::Manager as _;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(windows)]
fn enable_background_startup(_app: &tauri::AppHandle) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::RegValue;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::enums::KEY_READ;
    use winreg::enums::KEY_SET_VALUE;
    use winreg::enums::RegType::REG_BINARY;

    const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    const STARTUP_APPROVED_KEY: &str =
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const ENABLED: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    let executable = std::env::current_exe()
        .map_err(|_| "could not resolve the app path for automatic startup".to_string())?;
    let executable = executable
        .to_str()
        .filter(|path| {
            !path
                .chars()
                .any(|character| matches!(character, '"' | '\r' | '\n'))
        })
        .ok_or_else(|| "the app path cannot be used for automatic startup".to_string())?;
    let command = format!("\"{executable}\" --background");
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let result = (|| -> io::Result<()> {
        let run_key = current_user.open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_SET_VALUE)?;
        run_key.set_value(AUTOSTART_NAME, &command)?;
        let stored_command: String = run_key.get_value(AUTOSTART_NAME)?;
        if stored_command != command {
            return Err(io::Error::other(
                "automatic startup command did not round-trip exactly",
            ));
        }

        let approved_key = current_user
            .create_subkey_with_flags(STARTUP_APPROVED_KEY, KEY_READ | KEY_SET_VALUE)?
            .0;
        approved_key.set_raw_value(
            AUTOSTART_NAME,
            &RegValue {
                vtype: REG_BINARY,
                bytes: ENABLED.to_vec(),
            },
        )?;
        let stored_approval = approved_key.get_raw_value(AUTOSTART_NAME)?;
        if stored_approval.vtype != REG_BINARY || stored_approval.bytes != ENABLED {
            return Err(io::Error::other(
                "automatic startup approval did not round-trip exactly",
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = delete_windows_registry_value(&current_user, RUN_KEY);
        let _ = delete_windows_registry_value(&current_user, STARTUP_APPROVED_KEY);
    }
    result.map_err(|_| "could not enable automatic startup".to_string())
}

#[cfg(not(windows))]
fn enable_background_startup(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt as _;

    app.autolaunch()
        .enable()
        .map_err(|_| "could not enable automatic startup".to_string())
}

#[cfg(windows)]
fn disable_background_startup(_app: &tauri::AppHandle) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    const STARTUP_APPROVED_KEY: &str =
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let run_result = delete_windows_registry_value(&current_user, RUN_KEY);
    let approval_result = delete_windows_registry_value(&current_user, STARTUP_APPROVED_KEY);
    if run_result.is_ok() && approval_result.is_ok() {
        Ok(())
    } else {
        Err("could not disable automatic startup".to_string())
    }
}

#[cfg(windows)]
fn delete_windows_registry_value(root: &winreg::RegKey, key_path: &str) -> io::Result<()> {
    use winreg::enums::KEY_SET_VALUE;

    let key = match root.open_subkey_with_flags(key_path, KEY_SET_VALUE) {
        Ok(key) => key,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match key.delete_value(AUTOSTART_NAME) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(windows))]
fn disable_background_startup(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt as _;

    match app.autolaunch().disable() {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().to_ascii_lowercase().contains("not found") => Ok(()),
        Err(_) => Err("could not disable automatic startup".to_string()),
    }
}

pub fn credential_cli() -> Option<i32> {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments.get(1).map(String::as_str) == Some("recovery") {
        if arguments.len() != 3 || arguments.get(2).map(String::as_str) != Some("restore-latest") {
            eprintln!("invalid recovery helper invocation");
            return Some(2);
        }
        return Some(match restore_latest() {
            Ok(transaction_id) => {
                if std::io::stdout()
                    .write_all(transaction_id.as_bytes())
                    .is_ok()
                {
                    0
                } else {
                    1
                }
            }
            Err(_) => {
                eprintln!("recovery helper failed without changing unrelated files");
                1
            }
        });
    }
    if arguments.get(1).map(String::as_str) != Some("credential") {
        return None;
    }
    if arguments.len() != 4 || arguments.get(2).map(String::as_str) != Some("get") {
        eprintln!("invalid credential helper invocation");
        return Some(2);
    }
    let account = &arguments[3];
    if let Err(error) = authorize_codex_parent() {
        eprintln!("{error}");
        return Some(1);
    }
    if verify_credential_binding(account).is_err() {
        eprintln!("credential helper account is not bound to a matching managed provider");
        return Some(1);
    }
    match credentials::get(account) {
        Ok(secret) => {
            let secret = Zeroizing::new(secret);
            if std::io::stdout().write_all(secret.as_bytes()).is_ok() {
                Some(0)
            } else {
                eprintln!("credential helper output failed");
                Some(1)
            }
        }
        Err(error) => {
            eprintln!("{error}");
            Some(1)
        }
    }
}

fn verify_credential_binding(account: &str) -> Result<(), String> {
    let paths = app_paths()?;
    let config = read_config_safely(&paths.config)?;
    let current_path = paths
        .executable
        .canonicalize()
        .map_err(|_| "credential helper executable path is unavailable".to_string())?;
    verify_core_credential_binding(&config, account, &current_path).map_err(redacted_core_error)
}

fn proxy_status_from(state: &StoredProxyState, runtime: &ProxyRuntimeState) -> LocalProxyStatus {
    let running = runtime
        .handle
        .as_ref()
        .is_some_and(|handle| handle.health().running);
    LocalProxyStatus {
        enabled: state.enabled,
        running,
        recovery_required: false,
        manual_recovery_required: false,
        current_profile_id: state.profile_id.clone(),
        current_model_id: state.model_id.clone(),
        requires_codex_restart: state.requires_codex_restart,
        last_error: runtime.last_error.clone(),
    }
}

fn proxy_state_for_selection(
    previous: &StoredProxyState,
    profile_id: &str,
    model_id: &str,
) -> StoredProxyState {
    StoredProxyState {
        schema_version: PROXY_STATE_SCHEMA_VERSION,
        enabled: true,
        profile_id: Some(profile_id.to_string()),
        model_id: Some(model_id.to_string()),
        activation_transaction_id: previous.activation_transaction_id,
        port: previous.port,
        revision: previous.revision.saturating_add(1),
        requires_codex_restart: previous.requires_codex_restart,
    }
}

fn proxy_base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1")
}

fn current_proxy_config_state(paths: &AppPaths, port: u16) -> Result<ProxyConfigState, String> {
    let existing = read_config_safely(&paths.config)?;
    let current = inspect_config(&existing).map_err(redacted_core_error)?;
    if current.provider_id != LOCAL_PROXY_PROVIDER_ID {
        Ok(ProxyConfigState::NotSelected)
    } else if current.base_url.as_deref() == Some(proxy_base_url(port).as_str()) {
        Ok(ProxyConfigState::Managed)
    } else {
        Ok(ProxyConfigState::Changed)
    }
}

fn proxy_activation_can_restore_automatically(paths: &AppPaths, state: &StoredProxyState) -> bool {
    let Ok(manifest_path) = proxy_activation_manifest_path(paths, state) else {
        return false;
    };
    let Ok(manifest) = read_proxy_activation_manifest_metadata(paths, &manifest_path) else {
        return false;
    };
    if verify_backup_integrity(&manifest_path, &paths.config, &paths.catalog).is_err() {
        return false;
    }
    match manifest.status {
        BackupStatus::Applied => {
            backup_matches_applied(&manifest).unwrap_or(false)
                || verify_active_proxy_configuration(paths, state).is_ok()
        }
        BackupStatus::Detaching => {
            verify_proxy_detach_recoverable(&manifest_path, &paths.config, &paths.catalog).is_ok()
        }
        BackupStatus::Prepared | BackupStatus::Restoring => {
            recover_prepared_backup(&manifest_path, &paths.config, &paths.catalog).is_ok()
                && proxy_activation_can_restore_automatically(paths, state)
        }
        BackupStatus::Restored => false,
    }
}

fn proxy_activation_manifest(
    paths: &AppPaths,
    state: &StoredProxyState,
) -> Result<PathBuf, String> {
    let manifest_path = proxy_activation_manifest_path(paths, state)?;
    recover_prepared_backup(&manifest_path, &paths.config, &paths.catalog)
        .map_err(redacted_core_error)?;
    read_proxy_activation_manifest(paths, &manifest_path)?;
    Ok(manifest_path)
}

fn proxy_activation_manifest_path(
    paths: &AppPaths,
    state: &StoredProxyState,
) -> Result<PathBuf, String> {
    let transaction_id = match state.activation_transaction_id {
        Some(transaction_id) => transaction_id,
        None => recover_proxy_activation_transaction(paths)?.ok_or_else(|| {
            "no safe restore point is available for this fast-switch activation".to_string()
        })?,
    };
    let manifest_path = paths
        .backups
        .join(transaction_id.to_string())
        .join("manifest.json");
    let manifest = read_proxy_activation_manifest_metadata(paths, &manifest_path)?;
    if manifest.transaction_id != transaction_id {
        return Err("fast-switch restore point does not match its transaction".to_string());
    }
    Ok(manifest_path)
}

fn restore_proxy_activation(paths: &AppPaths, state: &StoredProxyState) -> Result<(), String> {
    let manifest_path = proxy_activation_manifest_path(paths, state)?;
    let mut manifest = read_proxy_activation_manifest_metadata(paths, &manifest_path)?;
    if matches!(
        manifest.status,
        BackupStatus::Prepared | BackupStatus::Restoring
    ) {
        recover_prepared_backup(&manifest_path, &paths.config, &paths.catalog)
            .map_err(redacted_core_error)?;
        manifest = read_proxy_activation_manifest_metadata(paths, &manifest_path)?;
    }

    match manifest.status {
        BackupStatus::Applied => {
            let result = match restore_backup(&manifest_path, &paths.config, &paths.catalog) {
                Ok(result) => result,
                Err(exact_error) => {
                    if recover_prepared_backup(&manifest_path, &paths.config, &paths.catalog)
                        .is_ok()
                    {
                        let recovered =
                            read_proxy_activation_manifest_metadata(paths, &manifest_path)?;
                        if recovered.status == BackupStatus::Restored {
                            return Ok(());
                        }
                    }
                    verify_active_proxy_configuration(paths, state)?;
                    match restore_proxy_config_preserving_unrelated_changes(
                        &manifest_path,
                        &paths.config,
                        &paths.catalog,
                        &proxy_base_url(state.port),
                    ) {
                        Ok(result) => result,
                        Err(_) => return Err(redacted_core_error(exact_error)),
                    }
                }
            };
            if !result.manifest_finalized {
                finalize_proxy_restore_journal(paths, &manifest_path)?;
            }
        }
        BackupStatus::Detaching => {
            verify_proxy_detach_recoverable(&manifest_path, &paths.config, &paths.catalog)
                .map_err(redacted_core_error)?;
            let result = restore_proxy_config_preserving_unrelated_changes(
                &manifest_path,
                &paths.config,
                &paths.catalog,
                &proxy_base_url(state.port),
            )
            .map_err(redacted_core_error)?;
            if !result.manifest_finalized {
                finalize_proxy_restore_journal(paths, &manifest_path)?;
            }
        }
        BackupStatus::Restored => {}
        BackupStatus::Prepared | BackupStatus::Restoring => {
            return Err("fast-switch recovery journal is still incomplete".to_string());
        }
    }
    finalize_proxy_restore_journal(paths, &manifest_path)
}

fn finalize_proxy_restore_journal(paths: &AppPaths, manifest_path: &Path) -> Result<(), String> {
    for _ in 0..2 {
        let manifest = read_proxy_activation_manifest_metadata(paths, manifest_path)?;
        match manifest.status {
            BackupStatus::Restored => return Ok(()),
            BackupStatus::Prepared | BackupStatus::Restoring => {
                recover_prepared_backup(manifest_path, &paths.config, &paths.catalog)
                    .map_err(redacted_core_error)?;
            }
            BackupStatus::Detaching => {
                verify_proxy_detach_recoverable(manifest_path, &paths.config, &paths.catalog)
                    .map_err(redacted_core_error)?;
                restore_proxy_config_preserving_unrelated_changes(
                    manifest_path,
                    &paths.config,
                    &paths.catalog,
                    &proxy_base_url(LOCAL_PROXY_PORT),
                )
                .map_err(redacted_core_error)?;
            }
            BackupStatus::Applied => {
                return Err("fast-switch restore did not reach a final state".to_string());
            }
        }
    }
    let manifest = read_proxy_activation_manifest_metadata(paths, manifest_path)?;
    if manifest.status == BackupStatus::Restored {
        Ok(())
    } else {
        Err(
            "Codex settings were restored, but the recovery journal could not be finalized"
                .to_string(),
        )
    }
}

fn verify_active_proxy_configuration(
    paths: &AppPaths,
    state: &StoredProxyState,
) -> Result<(), String> {
    let transaction_id = state
        .activation_transaction_id
        .ok_or_else(|| "enabled fast-switch state has no activation transaction".to_string())?;
    let manifest_path = proxy_activation_manifest_path(paths, state)?;
    recover_prepared_backup(&manifest_path, &paths.config, &paths.catalog)
        .map_err(redacted_core_error)?;
    let manifest = read_proxy_activation_manifest_metadata(paths, &manifest_path)?;
    if manifest.transaction_id != transaction_id || manifest.status != BackupStatus::Applied {
        return Err("fast-switch restore point does not match its transaction".to_string());
    }
    verify_backup_integrity(&manifest_path, &paths.config, &paths.catalog)
        .map_err(redacted_core_error)?;
    let config = read_config_safely(&paths.config)?;
    let current = inspect_config(&config).map_err(redacted_core_error)?;
    let manifest_model = manifest
        .model_id
        .as_deref()
        .ok_or_else(|| "the managed local proxy restore point has no model".to_string())?;
    if current.model_id.as_deref() != Some(manifest_model) {
        return Err("the managed local proxy model changed after activation".to_string());
    }
    let helper = verify_core_proxy_config_binding(&config, &proxy_base_url(state.port))
        .map_err(redacted_core_error)?;
    verify_managed_helper(paths, &helper)?;
    Ok(())
}

fn prepare_active_proxy_configuration(
    paths: &AppPaths,
    state: &StoredProxyState,
) -> Result<(), String> {
    ensure_stable_helper(paths)?;
    refresh_proxy_credential_helper_file(&paths.config, &proxy_base_url(state.port), &paths.helper)
        .map_err(redacted_core_error)?;
    verify_active_proxy_configuration(paths, state)
}

fn recover_proxy_activation_transaction(paths: &AppPaths) -> Result<Option<Uuid>, String> {
    let Some(manifest_path) = latest_applied_manifest(paths)? else {
        return Ok(None);
    };
    let manifest = match read_proxy_activation_manifest(paths, &manifest_path) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(None),
    };
    Ok(Some(manifest.transaction_id))
}

fn read_proxy_activation_manifest(
    paths: &AppPaths,
    manifest_path: &Path,
) -> Result<BackupManifest, String> {
    let manifest = read_proxy_activation_manifest_metadata(paths, manifest_path)?;
    if manifest.status != BackupStatus::Applied
        || !backup_matches_applied(&manifest).map_err(redacted_core_error)?
    {
        return Err("fast-switch restore point no longer matches Codex settings".to_string());
    }
    Ok(manifest)
}

fn read_proxy_activation_manifest_metadata(
    paths: &AppPaths,
    manifest_path: &Path,
) -> Result<BackupManifest, String> {
    match fs::symlink_metadata(manifest_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("fast-switch restore point is not a safe regular file".to_string());
        }
        Ok(_) => {}
        Err(_) => return Err("fast-switch restore point is unavailable".to_string()),
    }
    let manifest = serde_json::from_slice::<BackupManifest>(
        &fs::read(manifest_path)
            .map_err(|_| "could not read the fast-switch restore point".to_string())?,
    )
    .map_err(|_| "fast-switch restore point is invalid".to_string())?;
    if manifest.schema_version != 1
        || manifest.provider_id != LOCAL_PROXY_PROVIDER_ID
        || manifest.model_id.as_deref().is_none_or(str::is_empty)
        || manifest.config.path != paths.config
        || manifest.catalog.path != paths.catalog
        || manifest_path.file_name().and_then(|name| name.to_str()) != Some("manifest.json")
        || manifest_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some(manifest.transaction_id.to_string().as_str())
    {
        return Err("fast-switch restore point metadata is not safely applicable".to_string());
    }
    Ok(manifest)
}

fn verify_managed_helper(paths: &AppPaths, helper: &Path) -> Result<(), String> {
    let helper_dir = helper
        .parent()
        .ok_or_else(|| "managed credential helper has no parent".to_string())?;
    let helper_root = helper_dir
        .parent()
        .ok_or_else(|| "managed credential helper has no private root".to_string())?;
    if helper_root != paths.state.join("helpers") {
        return Err("managed credential helper is outside the private helper root".to_string());
    }
    let fingerprint = helper_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "managed credential helper has an invalid fingerprint".to_string())?;
    let expected_name = if cfg!(windows) {
        "codex-provider-switcher-helper.exe"
    } else {
        "codex-provider-switcher-helper"
    };
    if helper.file_name().and_then(|name| name.to_str()) != Some(expected_name) {
        return Err("managed credential helper has an unexpected name".to_string());
    }
    for directory in [&paths.state, helper_root, helper_dir] {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
            _ => return Err("managed credential helper directory is unsafe".to_string()),
        }
    }
    ensure_regular_source(helper)?;
    if sha256_file(helper)? != fingerprint.to_ascii_lowercase() {
        return Err("managed credential helper failed its integrity check".to_string());
    }
    Ok(())
}

fn load_proxy_state(paths: &AppPaths) -> Result<StoredProxyState, String> {
    let mut state = match fs::symlink_metadata(&paths.proxy_state) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("fast-switch state path is not a safe regular file".to_string());
        }
        Ok(_) => {
            let contents = fs::read_to_string(&paths.proxy_state)
                .map_err(|_| "could not read fast-switch state".to_string())?;
            serde_json::from_str::<StoredProxyState>(&contents)
                .map_err(|_| "fast-switch state is invalid".to_string())?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => StoredProxyState::default(),
        Err(_) => return Err("could not inspect fast-switch state".to_string()),
    };
    if state.schema_version == 1 {
        if state.enabled && state.activation_transaction_id.is_none() {
            state.activation_transaction_id =
                Some(recover_proxy_activation_transaction(paths)?.ok_or_else(|| {
                    "legacy fast-switch state has no safely matching restore point".to_string()
                })?);
        }
        state.schema_version = PROXY_STATE_SCHEMA_VERSION;
        write_proxy_state(paths, &state)?;
    }
    validate_proxy_state(&state)?;
    Ok(state)
}

fn validate_proxy_state(state: &StoredProxyState) -> Result<(), String> {
    if state.schema_version != PROXY_STATE_SCHEMA_VERSION {
        return Err("fast-switch state uses an unsupported version".to_string());
    }
    if state.port != LOCAL_PROXY_PORT {
        return Err("fast-switch state contains an unsupported local port".to_string());
    }
    if state.enabled
        && (state.profile_id.as_deref().is_none_or(str::is_empty)
            || state.model_id.as_deref().is_none_or(str::is_empty)
            || state.activation_transaction_id.is_none_or(|id| id.is_nil()))
    {
        return Err(
            "enabled fast-switch state has no complete selection or activation transaction"
                .to_string(),
        );
    }
    if !state.enabled && state.activation_transaction_id.is_some() {
        return Err(
            "disabled fast-switch state cannot retain an activation transaction".to_string(),
        );
    }
    Ok(())
}

fn write_proxy_state(paths: &AppPaths, state: &StoredProxyState) -> Result<(), String> {
    validate_proxy_state(state)?;
    ensure_state_root(paths)?;
    let mut rendered = serde_json::to_vec_pretty(state)
        .map_err(|_| "could not encode fast-switch state".to_string())?;
    rendered.push(b'\n');
    write_private_file(&paths.proxy_state, &rendered)
        .map_err(|_| "could not save fast-switch state".to_string())
}

fn load_proxy_route(
    paths: &AppPaths,
    state: &StoredProxyState,
) -> Result<(ProviderProfile, RouteConfig), String> {
    let profile_id = state
        .profile_id
        .as_deref()
        .ok_or_else(|| "choose a saved connection before enabling fast switching".to_string())?;
    let model_id = state
        .model_id
        .as_deref()
        .ok_or_else(|| "choose a model before enabling fast switching".to_string())?;
    let profile = load_profiles(paths)?
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "the selected saved connection no longer exists".to_string())?;
    if !profile.models.iter().any(|model| model.id == model_id) {
        return Err("the selected model no longer belongs to this connection".to_string());
    }
    let account =
        credential_account_for(&profile.id, &profile.base_url).map_err(redacted_core_error)?;
    let secret =
        Zeroizing::new(credentials::get(&account).map_err(map_missing_provider_credential)?);
    let bearer = BearerToken::new(secret.as_str().to_string()).map_err(|_| {
        "the saved provider credential cannot be used as a bearer token".to_string()
    })?;
    let models = profile.models.iter().map(proxy_model_descriptor).collect();
    let route = RouteConfig::new(
        profile.id.clone(),
        &profile.base_url,
        model_id,
        models,
        bearer,
    )
    .map_err(|_| "the selected connection cannot be used by the local proxy".to_string())?;
    Ok((profile, route))
}

fn proxy_model_descriptor(model: &ModelSpec) -> ModelDescriptor {
    ModelDescriptor {
        slug: model.id.clone(),
        display_name: model.display_name.clone(),
        description: (!model.description.is_empty()).then(|| model.description.clone()),
        context_window: i64::try_from(model.context_window).ok(),
        max_context_window: i64::try_from(model.context_window).ok(),
        default_reasoning_level: Some(reasoning_effort_id(&model.default_reasoning).to_string()),
        supported_reasoning_levels: model
            .reasoning_levels
            .iter()
            .map(|effort| ReasoningLevelDescriptor {
                effort: reasoning_effort_id(effort).to_string(),
                description: effort.description().to_string(),
            })
            .collect(),
        supports_parallel_tool_calls: model.supports_parallel_tool_calls,
        supports_images: model.supports_images,
    }
}

fn reasoning_effort_id(effort: &ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "none",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
        ReasoningEffort::Ultra => "ultra",
    }
}

async fn start_proxy_handle(
    state: &StoredProxyState,
    runtime: &mut ProxyRuntimeState,
    route: RouteConfig,
    allow_create_token: bool,
) -> Result<(), String> {
    if runtime
        .handle
        .as_ref()
        .is_some_and(|handle| handle.health().running)
    {
        runtime
            .handle
            .as_ref()
            .expect("running handle checked above")
            .set_active_route(route);
        runtime.last_error = None;
        return Ok(());
    }
    if let Some(stale) = runtime.handle.take() {
        let _ = stale.shutdown().await;
    }

    let base_url = proxy_base_url(state.port);
    let account = proxy_credential_account_for(&base_url).map_err(redacted_core_error)?;
    let secret = if credentials::exists(&account)? {
        Zeroizing::new(credentials::get(&account)?)
    } else if allow_create_token {
        let secret = Zeroizing::new(generate_proxy_token());
        credentials::store(&account, secret.as_str())?;
        secret
    } else {
        return Err(
            "the local proxy credential is missing; enable fast switching again".to_string(),
        );
    };
    let entry_bearer = BearerToken::new(secret.as_str().to_string())
        .map_err(|_| "the local proxy credential is invalid".to_string())?;
    let options = ProxyStartOptions {
        port: state.port,
        max_request_bytes: LOCAL_PROXY_MAX_REQUEST_BYTES,
        ..ProxyStartOptions::default()
    };
    let handle = LocalProxy::start(options, entry_bearer)
        .await
        .map_err(|_| "the local proxy could not start on 127.0.0.1".to_string())?;
    if handle.listen_addr().port() != state.port {
        let _ = handle.shutdown().await;
        return Err("the local proxy started on an unexpected port".to_string());
    }
    handle.set_active_route(route);
    runtime.handle = Some(handle);
    runtime.last_error = None;
    Ok(())
}

async fn rollback_proxy_runtime(
    paths: &AppPaths,
    previous: &StoredProxyState,
    runtime: &mut ProxyRuntimeState,
) {
    if previous.enabled {
        let rollback = match load_proxy_route(paths, previous) {
            Ok((_, route)) => start_proxy_handle(previous, runtime, route, false).await,
            Err(error) => Err(error),
        };
        if let Err(error) = rollback {
            if let Some(handle) = runtime.handle.take() {
                let _ = handle.shutdown().await;
            }
            runtime.last_error = Some(error);
        }
    } else if let Some(handle) = runtime.handle.take() {
        let _ = handle.shutdown().await;
    }
}

fn generate_proxy_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    token
}

struct AppPaths {
    codex_home: PathBuf,
    state: PathBuf,
    config: PathBuf,
    catalog: PathBuf,
    profiles: PathBuf,
    official_profile: PathBuf,
    proxy_state: PathBuf,
    backups: PathBuf,
    executable: PathBuf,
    helper: PathBuf,
    helper_sha256: String,
}

fn app_paths() -> Result<AppPaths, String> {
    let codex_home = match std::env::var_os("CODEX_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(".codex"))
            .ok_or_else(|| "could not resolve the user home directory".to_string())?,
    };
    if !codex_home.is_absolute() {
        return Err("CODEX_HOME must be an absolute path".to_string());
    }
    let state = codex_home.join("provider-switcher");
    let executable = std::env::current_exe()
        .map_err(|_| "could not resolve the switcher executable".to_string())?;
    let helper_sha256 = sha256_file(&executable)?;
    let helper_name = if cfg!(windows) {
        "codex-provider-switcher-helper.exe"
    } else {
        "codex-provider-switcher-helper"
    };
    let helper = state.join("helpers").join(&helper_sha256).join(helper_name);
    Ok(AppPaths {
        codex_home: codex_home.clone(),
        state: state.clone(),
        config: codex_home.join("config.toml"),
        catalog: state.join("models.json"),
        profiles: state.join("profiles.json"),
        official_profile: state.join("official-profile.json"),
        proxy_state: state.join("proxy.json"),
        backups: state.join("backups"),
        executable,
        helper,
        helper_sha256,
    })
}

fn load_profiles(paths: &AppPaths) -> Result<ProfileStore, String> {
    match fs::symlink_metadata(&paths.profiles) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("saved connections path is not a safe regular file".to_string())
        }
        Ok(_) => {
            let contents = fs::read_to_string(&paths.profiles)
                .map_err(|_| "could not read saved connections".to_string())?;
            parse_profile_store(&contents)
                .map_err(|_| "saved connections file is invalid".to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ProfileStore::default()),
        Err(_) => Err("could not inspect saved connections".to_string()),
    }
}

fn write_profiles(paths: &AppPaths, rendered: &[u8]) -> Result<(), String> {
    ensure_state_root(paths)?;
    write_private_file(&paths.profiles, rendered)
        .map_err(|_| "could not save saved connections".to_string())
}

fn load_official_profile(paths: &AppPaths) -> Result<Option<OfficialProfile>, String> {
    match fs::symlink_metadata(&paths.official_profile) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("official profile path is not a safe regular file".to_string())
        }
        Ok(_) => {
            let contents = fs::read_to_string(&paths.official_profile)
                .map_err(|_| "could not read the saved official configuration".to_string())?;
            let profile = serde_json::from_str::<OfficialProfile>(&contents)
                .map_err(|_| "saved official configuration is invalid".to_string())?;
            validate_official_profile(&profile)
                .map_err(|_| "saved official configuration is invalid".to_string())?;
            Ok(Some(profile))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("could not inspect the saved official configuration".to_string()),
    }
}

fn write_official_profile(paths: &AppPaths, profile: &OfficialProfile) -> Result<(), String> {
    validate_official_profile(profile).map_err(redacted_core_error)?;
    ensure_state_root(paths)?;
    let mut rendered = serde_json::to_vec_pretty(profile)
        .map_err(|_| "could not encode the official configuration".to_string())?;
    rendered.push(b'\n');
    write_private_file(&paths.official_profile, &rendered)
        .map_err(|_| "could not save the official configuration".to_string())
}

fn remove_official_profile_metadata(paths: &AppPaths) -> Result<(), String> {
    match fs::symlink_metadata(&paths.state) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err("provider switcher state path is not a safe directory".to_string());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("could not inspect provider switcher state".to_string()),
    }
    match fs::symlink_metadata(&paths.official_profile) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("official profile path is not a safe regular file".to_string())
        }
        Ok(_) => fs::remove_file(&paths.official_profile)
            .map_err(|_| "could not remove the saved official configuration".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("could not inspect the saved official configuration".to_string()),
    }
}

fn official_profile_from_current_account(
    current: &CurrentCodexConfig,
    account: &CodexAccountStatus,
) -> Result<OfficialProfile, String> {
    if current.provider_id != "openai"
        || current.base_url.is_some()
        || current.auth_kind != AuthKind::OfficialLogin
    {
        return Err(
            "switch Codex to its built-in OpenAI login before saving the official configuration"
                .to_string(),
        );
    }
    if account.auth_mode != CodexAuthMode::Chatgpt {
        return Err("Codex does not have an active ChatGPT login".to_string());
    }
    let display_name = account
        .email
        .as_ref()
        .filter(|email| email.chars().count() <= 80)
        .cloned()
        .unwrap_or_else(|| OFFICIAL_PROFILE_DISPLAY_NAME.to_string());
    let profile = OfficialProfile::with_account(
        display_name,
        current.model_id.clone(),
        account.email.clone(),
        account.plan_type.clone(),
    );
    validate_official_profile(&profile).map_err(redacted_core_error)?;
    Ok(profile)
}

fn save_official_account_metadata(
    paths: &AppPaths,
    current: &CurrentCodexConfig,
    account: &CodexAccountStatus,
) -> Result<OfficialProfile, String> {
    let profile = official_profile_from_current_account(current, account)?;
    let existing = load_official_profile(paths)?;
    if existing.as_ref() != Some(&profile) {
        write_official_profile(paths, &profile)?;
    }
    Ok(profile)
}

fn refresh_official_account_metadata_if_readable(
    paths: &AppPaths,
    current: &CurrentCodexConfig,
    account: &CodexAccountStatus,
) -> Result<(), String> {
    let profile = official_profile_from_current_account(current, account)?;
    let Ok(existing) = load_official_profile(paths) else {
        return Ok(());
    };
    if existing.as_ref() != Some(&profile) {
        write_official_profile(paths, &profile)?;
    }
    Ok(())
}

fn ensure_state_root(paths: &AppPaths) -> Result<(), String> {
    match fs::symlink_metadata(&paths.state) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err("provider switcher state path is not a safe directory".to_string())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_private_directory(&paths.state).map_err(redacted_core_error)
        }
        Err(_) => Err("could not inspect provider switcher state".to_string()),
    }
}

fn lock_profiles(paths: &AppPaths) -> Result<File, String> {
    ensure_state_root(paths)?;
    let lock_path = paths.profiles.with_extension("lock");
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("saved connections lock is not a safe regular file".to_string());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("could not inspect the saved connections lock".to_string()),
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .map_err(|_| "could not open the saved connections lock".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "could not secure the saved connections lock".to_string())?;
    }
    FileExt::lock(&lock).map_err(|_| "could not lock saved connections".to_string())?;
    Ok(lock)
}

fn insert_discovery(
    vault: &DiscoveryVault,
    base_url: String,
    secret: String,
) -> Result<Uuid, String> {
    let original_secret = Zeroizing::new(secret);
    let normalized_secret = Zeroizing::new(original_secret.trim().to_string());
    credentials::validate_secret(normalized_secret.as_str())?;
    model_endpoint_candidates(&base_url)?;
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    let session_id = Uuid::new_v4();
    {
        let mut sessions = vault
            .0
            .lock()
            .map_err(|_| "temporary credential storage is unavailable".to_string())?;
        prune_discoveries(&mut sessions);
        if sessions.len() >= MAX_DISCOVERY_SESSIONS
            && let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, pending)| pending.created_at)
                .map(|(id, _)| *id)
        {
            sessions.remove(&oldest);
        }
        sessions.insert(
            session_id,
            PendingDiscovery {
                base_url,
                secret: normalized_secret,
                created_at: Instant::now(),
            },
        );
    }

    let sessions = Arc::clone(&vault.0);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(DISCOVERY_TTL).await;
        if let Ok(mut sessions) = sessions.lock() {
            sessions.remove(&session_id);
        }
    });
    Ok(session_id)
}

fn take_discovery(
    vault: &DiscoveryVault,
    discovery_id: Option<String>,
) -> Result<Option<PendingDiscovery>, String> {
    let Some(discovery_id) = discovery_id else {
        return Ok(None);
    };
    let discovery_id = Uuid::parse_str(&discovery_id)
        .map_err(|_| "invalid model discovery session".to_string())?;
    let mut sessions = vault
        .0
        .lock()
        .map_err(|_| "temporary credential storage is unavailable".to_string())?;
    prune_discoveries(&mut sessions);
    sessions
        .remove(&discovery_id)
        .map(Some)
        .ok_or_else(|| "the model discovery session expired; enter the API Key again".to_string())
}

fn prune_discoveries(sessions: &mut HashMap<Uuid, PendingDiscovery>) {
    sessions.retain(|_, pending| pending.created_at.elapsed() <= DISCOVERY_TTL);
}

fn normalized_endpoint(value: &str) -> &str {
    value.trim().trim_end_matches('/')
}

fn map_missing_provider_credential(error: String) -> String {
    if error == "credential is not stored" {
        MISSING_SAVED_PROVIDER_CREDENTIAL.to_string()
    } else {
        error
    }
}

fn ensure_stable_helper(paths: &AppPaths) -> Result<(), String> {
    ensure_state_root(paths)?;
    ensure_regular_source(&paths.executable)?;
    if paths.helper.exists() {
        ensure_regular_source(&paths.helper)?;
        if sha256_file(&paths.helper)? == paths.helper_sha256 {
            return Ok(());
        }
        return Err("the installed credential helper failed its integrity check".to_string());
    }

    let helper_dir = paths
        .helper
        .parent()
        .ok_or_else(|| "credential helper path has no parent".to_string())?;
    let helper_root = helper_dir
        .parent()
        .ok_or_else(|| "credential helper path has no private root".to_string())?;
    fs::create_dir_all(helper_root)
        .map_err(|_| "could not create the credential helper root".to_string())?;
    match fs::symlink_metadata(helper_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err("credential helper root is not a safe directory".to_string());
        }
        Ok(_) => {}
        Err(_) => return Err("could not inspect the credential helper root".to_string()),
    }
    create_private_directory(helper_dir)
        .map_err(|_| "could not create a private credential helper directory".to_string())?;

    let mut source = File::open(&paths.executable)
        .map_err(|_| "could not read the app executable".to_string())?;
    let mut temporary = NamedTempFile::new_in(helper_dir)
        .map_err(|_| "could not stage the credential helper".to_string())?;
    io::copy(&mut source, temporary.as_file_mut())
        .map_err(|_| "could not copy the credential helper".to_string())?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|_| "could not sync the credential helper".to_string())?;
    set_helper_permissions(temporary.as_file())
        .map_err(|_| "could not secure the credential helper".to_string())?;
    match temporary.persist_noclobber(&paths.helper) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_regular_source(&paths.helper)?;
        }
        Err(_) => return Err("could not install the credential helper".to_string()),
    }
    if sha256_file(&paths.helper)? != paths.helper_sha256 {
        return Err("the installed credential helper failed its integrity check".to_string());
    }
    sync_helper_parent(&paths.helper)
        .map_err(|_| "could not finalize the credential helper".to_string())
}

fn ensure_regular_source(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("credential helper source is not a regular file".to_string())
        }
        Ok(_) => Ok(()),
        Err(_) => Err("credential helper source is unavailable".to_string()),
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|_| "could not read the executable".to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "could not hash the executable".to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(unix)]
fn set_helper_permissions(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_helper_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_helper_parent(path: &Path) -> io::Result<()> {
    File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("helper path has no parent"))?,
    )?
    .sync_all()
}

#[cfg(not(unix))]
fn sync_helper_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn read_config_safely(path: &Path) -> Result<String, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("refusing to read a symlinked Codex configuration".to_string())
        }
        Ok(metadata) if !metadata.is_file() => {
            Err("Codex configuration path is not a regular file".to_string())
        }
        Ok(_) => fs::read_to_string(path)
            .map_err(|_| "could not read the Codex configuration as UTF-8".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(_) => Err("could not inspect the Codex configuration".to_string()),
    }
}

fn read_internal_catalog_safely(path: &Path) -> Result<String, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("switcher model catalog path is not a safe regular file".to_string())
        }
        Ok(_) => fs::read_to_string(path)
            .map_err(|_| "could not read the switcher model catalog as UTF-8".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok("{\n  \"models\": []\n}\n".to_string())
        }
        Err(_) => Err("could not inspect the switcher model catalog".to_string()),
    }
}

fn latest_applied_manifest(paths: &AppPaths) -> Result<Option<PathBuf>, String> {
    let entries = match fs::read_dir(&paths.backups) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("could not inspect switcher backups".to_string()),
    };
    let mut latest: Option<(u128, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path().join("manifest.json");
        if recover_prepared_backup(&path, &paths.config, &paths.catalog).is_err() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<BackupManifest>(&bytes) else {
            continue;
        };
        if manifest.status != BackupStatus::Applied
            || !backup_matches_applied(&manifest).unwrap_or(false)
        {
            continue;
        }
        if latest
            .as_ref()
            .is_none_or(|(created, _)| manifest.created_unix_ms > *created)
        {
            latest = Some((manifest.created_unix_ms, path));
        }
    }
    Ok(latest.map(|(_, path)| path))
}

fn recover_prepared_manifests(paths: &AppPaths) -> usize {
    let entries = match fs::read_dir(&paths.backups) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(_) => return 1,
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("manifest.json"))
        .filter(|path| path.exists())
        .filter(|path| recover_prepared_backup(path, &paths.config, &paths.catalog).is_err())
        .count()
}

fn require_clean_recovery_state(paths: &AppPaths) -> Result<(), String> {
    let warnings = recover_prepared_manifests(paths);
    if warnings == 0 {
        Ok(())
    } else {
        Err(format!(
            "{warnings} interrupted or invalid transaction(s) require manual review"
        ))
    }
}

fn redacted_core_error(error: codex_provider_switcher_core::SwitcherError) -> String {
    match error {
        codex_provider_switcher_core::SwitcherError::Validation(message)
        | codex_provider_switcher_core::SwitcherError::Conflict(message) => message,
        codex_provider_switcher_core::SwitcherError::SymbolicLink(_)
        | codex_provider_switcher_core::SwitcherError::NotRegularFile(_) => {
            "refusing an unsafe configuration path".to_string()
        }
        codex_provider_switcher_core::SwitcherError::InvalidUtf8 => {
            "Codex configuration is not valid UTF-8".to_string()
        }
        _ => "local configuration operation failed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_with_models(id: &str, models: &[&str]) -> ProviderProfile {
        ProviderProfile {
            id: id.to_string(),
            display_name: "Test API".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            models: models
                .iter()
                .map(|id| ModelSpec {
                    id: (*id).to_string(),
                    display_name: (*id).to_string(),
                    description: String::new(),
                    context_window: 128_000,
                    default_reasoning: ReasoningEffort::Medium,
                    reasoning_levels: vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::Medium,
                        ReasoningEffort::High,
                    ],
                    supports_parallel_tool_calls: true,
                    supports_images: false,
                })
                .collect(),
            supports_websockets: false,
            credential_required: true,
        }
    }

    #[test]
    fn proxy_state_requires_a_fixed_loopback_port_and_complete_selection() {
        let mut state = StoredProxyState::default();
        assert!(validate_proxy_state(&state).is_ok());

        state.enabled = true;
        assert!(validate_proxy_state(&state).is_err());
        state.profile_id = Some("profile".to_string());
        state.model_id = Some("model".to_string());
        assert!(validate_proxy_state(&state).is_err());
        state.activation_transaction_id = Some(Uuid::new_v4());
        assert!(validate_proxy_state(&state).is_ok());
        state.port += 1;
        assert!(validate_proxy_state(&state).is_err());

        let mut disabled_with_transaction = StoredProxyState::default();
        disabled_with_transaction.activation_transaction_id = Some(Uuid::new_v4());
        assert!(validate_proxy_state(&disabled_with_transaction).is_err());
    }

    #[test]
    fn route_selection_is_keyless_and_preserves_restart_notice() {
        let activation_transaction_id = Uuid::new_v4();
        let previous = StoredProxyState {
            requires_codex_restart: true,
            revision: 7,
            activation_transaction_id: Some(activation_transaction_id),
            enabled: true,
            profile_id: Some("profile-old".to_string()),
            model_id: Some("model-old".to_string()),
            ..StoredProxyState::default()
        };
        let selected = proxy_state_for_selection(&previous, "profile-a", "model-a");

        assert!(selected.enabled);
        assert_eq!(selected.profile_id.as_deref(), Some("profile-a"));
        assert_eq!(selected.model_id.as_deref(), Some("model-a"));
        assert_eq!(selected.revision, 8);
        assert!(selected.requires_codex_restart);
        assert_eq!(
            selected.activation_transaction_id,
            Some(activation_transaction_id)
        );
        let rendered = serde_json::to_string(&selected).unwrap();
        assert!(!rendered.to_ascii_lowercase().contains("api_key"));
        assert!(!rendered.to_ascii_lowercase().contains("bearer"));
    }

    #[test]
    fn active_profile_can_be_edited_only_while_it_keeps_the_current_model() {
        let state = StoredProxyState {
            enabled: true,
            profile_id: Some("profile-a".to_string()),
            model_id: Some("model-current".to_string()),
            activation_transaction_id: Some(Uuid::new_v4()),
            ..StoredProxyState::default()
        };

        assert!(
            validate_active_profile_replacement(
                &state,
                &profile_with_models("profile-a", &["model-current", "model-new"]),
            )
            .is_ok()
        );
        assert!(
            validate_active_profile_replacement(
                &state,
                &profile_with_models("profile-a", &["model-new"]),
            )
            .is_err()
        );
        assert!(
            validate_active_profile_replacement(
                &state,
                &profile_with_models("profile-b", &["model-new"]),
            )
            .is_ok()
        );
    }

    #[test]
    fn profile_credential_lookup_is_limited_to_saved_keyed_connections() {
        let mut store = ProfileStore::default();
        store
            .profiles
            .push(profile_with_models("profile-a", &["model-a"]));

        assert!(profile_credential_account(&store, "profile-a").is_ok());
        assert!(profile_credential_account(&store, "profile-missing").is_err());

        store.profiles[0].credential_required = false;
        assert!(profile_credential_account(&store, "profile-a").is_err());
    }

    #[test]
    fn missing_provider_credential_has_an_actionable_error() {
        assert_eq!(
            map_missing_provider_credential("credential is not stored".to_string()),
            MISSING_SAVED_PROVIDER_CREDENTIAL
        );
        assert_eq!(
            map_missing_provider_credential("native credential store operation failed".to_string()),
            "native credential store operation failed"
        );
    }

    #[test]
    fn captures_only_the_builtin_official_route() {
        let official = CurrentCodexConfig {
            provider_id: "openai".to_string(),
            provider_name: "OpenAI".to_string(),
            model_id: Some("gpt-5.6-sol".to_string()),
            base_url: None,
            auth_kind: AuthKind::OfficialLogin,
            catalog_path: None,
        };
        let account = CodexAccountStatus {
            auth_mode: CodexAuthMode::Chatgpt,
            email: Some("user@example.com".to_string()),
            plan_type: Some("plus".to_string()),
            requires_openai_auth: true,
            codex_access_token_environment_present: false,
        };
        let captured = official_profile_from_current_account(&official, &account).unwrap();
        assert_eq!(captured.display_name, "user@example.com");
        assert_eq!(captured.model_id.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(captured.plan_type.as_deref(), Some("plus"));

        let mut redirected = official;
        redirected.base_url = Some("https://redirect.example/v1".to_string());
        assert!(official_profile_from_current_account(&redirected, &account).is_err());
    }
}
