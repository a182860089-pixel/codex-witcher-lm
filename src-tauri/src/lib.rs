mod app_update;
mod codex_account;
mod outbound_proxy;

use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
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
use codex_provider_switcher_core::UserNoProxyPersistPlan;
use codex_provider_switcher_core::apply_config_plan;
use codex_provider_switcher_core::apply_config_plan_with_transaction_id;
use codex_provider_switcher_core::backup_matches_applied;
use codex_provider_switcher_core::create_private_directory;
use codex_provider_switcher_core::credential_account_for;
use codex_provider_switcher_core::fetch_models;
use codex_provider_switcher_core::inspect_config;
use codex_provider_switcher_core::leftover_backup_requires_manual_review;
use codex_provider_switcher_core::merge_no_proxy;
use codex_provider_switcher_core::model_endpoint_candidates;
#[cfg(target_os = "macos")]
use codex_provider_switcher_core::no_proxy_covers_loopback;
use codex_provider_switcher_core::normalize_api_base_url;
use codex_provider_switcher_core::parse_profile_store_with_migration;
use codex_provider_switcher_core::plan_config;
use codex_provider_switcher_core::plan_official_config;
use codex_provider_switcher_core::plan_proxy_config;
use codex_provider_switcher_core::plan_user_no_proxy_persist;
use codex_provider_switcher_core::proxy_credential_account_for;
use codex_provider_switcher_core::recover_prepared_backup;
use codex_provider_switcher_core::refresh_proxy_credential_helper_file;
use codex_provider_switcher_core::remove_profile;
use codex_provider_switcher_core::render_profile_store;
use codex_provider_switcher_core::restore_backup;
use codex_provider_switcher_core::restore_proxy_config_preserving_unrelated_changes;
use codex_provider_switcher_core::retarget_local_proxy_base_url_file;
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
#[cfg(windows)]
use codex_provider_switcher_launcher::notify_user_environment_changed;
use codex_provider_switcher_launcher::open_codex as launch_codex;
use codex_provider_switcher_launcher::restart_codex as launch_restart_codex;
use codex_provider_switcher_local_proxy::BearerToken;
use codex_provider_switcher_local_proxy::LocalProxy;
use codex_provider_switcher_local_proxy::ModelDescriptor;
use codex_provider_switcher_local_proxy::ProxyHandle;
use codex_provider_switcher_local_proxy::ProxyStartOptions;
use codex_provider_switcher_local_proxy::ReasoningLevelDescriptor;
use codex_provider_switcher_local_proxy::RouteConfig;
use codex_provider_switcher_local_proxy::read_proxy_bindings;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum OutboundProxyMode {
    #[default]
    Auto,
    Direct,
    System,
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
    cc_switch_detected: bool,
    outbound_proxy_mode: OutboundProxyMode,
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
    #[serde(default)]
    outbound_proxy_mode: OutboundProxyMode,
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
            outbound_proxy_mode: OutboundProxyMode::Auto,
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
    use_system_proxy: Option<bool>,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepairFastSwitchReport {
    steps: Vec<String>,
    status: LocalProxyStatus,
}

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
                    cc_switch_detected: cc_switch_process_running(),
                    outbound_proxy_mode: OutboundProxyMode::Auto,
                }),
            };
        }
    };
    let config_selected = config_state != ProxyConfigState::NotSelected;
    let mut state = match load_proxy_state(&paths) {
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
                cc_switch_detected: cc_switch_process_running(),
                outbound_proxy_mode: fallback_state.outbound_proxy_mode,
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
        let bypass_changed = match load_proxy_route(&paths, &state) {
            Ok((_, route)) => {
                match start_proxy_handle(&paths, &state, &mut runtime, route, false).await {
                    Ok(changed) => changed,
                    Err(error) => {
                        runtime.last_error = Some(error);
                        false
                    }
                }
            }
            Err(error) => {
                runtime.last_error = Some(error);
                false
            }
        };
        if bypass_changed {
            remember_codex_restart_for_loopback_bypass(&paths, &mut state);
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
    let bypass_changed = start_proxy_handle(&paths, &next, &mut runtime, route, true).await?;
    next.requires_codex_restart |= bypass_changed;

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
    let mut next = proxy_state_for_selection(&previous, &profile_id, &selected_model);
    let (_, route) = load_proxy_route(&paths, &next)?;
    let bypass_changed = start_proxy_handle(&paths, &next, &mut runtime, route, false).await?;
    next.requires_codex_restart |= bypass_changed;
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
async fn repair_fast_switch(
    runtime: tauri::State<'_, ProxyRuntime>,
) -> Result<RepairFastSwitchReport, String> {
    let paths = app_paths()?;
    let mut runtime = runtime.inner.lock().await;
    let mut steps = Vec::new();

    let persist_changed = ensure_loopback_proxy_bypass(&paths);
    steps.push(if persist_changed {
        "\u{5df2}\u{622a}\u{65ad}\u{5e76}\u{89c4}\u{8303}\u{5316} NO_PROXY".to_string()
    } else {
        "NO_PROXY \u{5df2}\u{5305}\u{542b}\u{56de}\u{73af}\u{5730}\u{5740}".to_string()
    });

    let quarantined = quarantine_invalid_backups(&paths);
    steps.push(format!(
        "\u{5df2}\u{9694}\u{79bb} {quarantined} \u{4e2a}\u{65e0}\u{6548}\u{5907}\u{4efd}\u{76ee}\u{5f55}"
    ));

    let (mut state, salvaged) = match load_proxy_state(&paths) {
        Ok(state) => (state, false),
        Err(_) => (salvage_proxy_state(&paths), true),
    };
    if salvaged {
        steps.push(
            "proxy.json \u{65e0}\u{6cd5}\u{89e3}\u{6790}\u{ff0c}\u{5df2}\u{4f7f}\u{7528}\u{53ef}\u{4fee}\u{590d}\u{7684}\u{9ed8}\u{8ba4}\u{72b6}\u{6001}"
                .to_string(),
        );
    }

    let mut profiles_migrated = false;
    if let Ok((store, migrated)) = load_profiles_with_migration(&paths) {
        profiles_migrated = migrated;
        if let Some(profile_id) = state.profile_id.clone()
            && let Some(profile) = store
                .profiles
                .iter()
                .find(|profile| profile.id == profile_id)
        {
            let applied = applied_transaction_model_id(&paths, &state);
            if let Some(model_id) =
                fallback_proxy_model_id(state.model_id.as_deref(), profile, applied.as_deref())
                && state.model_id.as_deref() != Some(model_id.as_str())
            {
                state.model_id = Some(model_id);
                steps.push(
                    "\u{5df2}\u{5c06}\u{5931}\u{6548}\u{6a21}\u{578b}\u{56de}\u{9000}\u{5230}\u{53ef}\u{7528}\u{6a21}\u{578b}"
                        .to_string(),
                );
            }
        }
    }
    if profiles_migrated {
        steps.push(
            "\u{5df2}\u{4e3a}\u{5df2}\u{4fdd}\u{5b58}\u{63a5}\u{5165}\u{5f00}\u{542f}\u{56fe}\u{7247}\u{8f93}\u{5165}"
                .to_string(),
        );
        state.requires_codex_restart = true;
    }
    coerce_proxy_state_for_repair(&mut state);
    state.revision = state.revision.saturating_add(1);
    write_proxy_state(&paths, &state)?;
    steps.push(
        "\u{5df2}\u{91cd}\u{5199} proxy.json\u{ff08}\u{53bb}\u{9664} BOM\u{ff09}".to_string(),
    );

    let proxy_url = proxy_base_url(LOCAL_PROXY_PORT);
    match retarget_local_proxy_base_url_file(&paths.config, &proxy_url) {
        Ok(true) => steps.push(
            "\u{5df2}\u{5c06} Codex cps-local \u{6307}\u{56de} 127.0.0.1:15722".to_string(),
        ),
        Ok(false) => steps.push(
            "Codex \u{672c}\u{5730}\u{4ee3}\u{7406}\u{5730}\u{5740}\u{65e0}\u{9700}\u{4fee}\u{6539}".to_string(),
        ),
        Err(error) => steps.push(format!(
            "\u{672a}\u{80fd}\u{6539}\u{5199} Codex \u{672c}\u{5730}\u{4ee3}\u{7406}\u{5730}\u{5740}: {}",
            redacted_core_error(error)
        )),
    }

    if persist_changed {
        remember_codex_restart_for_loopback_bypass(&paths, &mut state);
    }

    if state.enabled {
        match load_proxy_route(&paths, &state) {
            Ok((_, route)) => {
                match start_proxy_handle(&paths, &state, &mut runtime, route, true).await {
                    Ok(changed) => {
                        if changed {
                            remember_codex_restart_for_loopback_bypass(&paths, &mut state);
                        }
                        steps.push(
                            "\u{5df2}\u{91cd}\u{542f}\u{672c}\u{5730} 15722 \u{76d1}\u{542c}"
                                .to_string(),
                        );
                    }
                    Err(error) => {
                        runtime.last_error = Some(error.clone());
                        steps.push(format!(
                            "\u{91cd}\u{542f}\u{672c}\u{5730}\u{76d1}\u{542c}\u{5931}\u{8d25}: {error}"
                        ));
                    }
                }
            }
            Err(error) => {
                runtime.last_error = Some(error.clone());
                steps.push(format!(
                    "\u{65e0}\u{6cd5}\u{52a0}\u{8f7d}\u{7ebf}\u{8def}: {error}"
                ));
            }
        }
    } else {
        steps.push(
            "\u{5feb}\u{901f}\u{5207}\u{6362}\u{672a}\u{542f}\u{7528}\u{ff0c}\u{672a}\u{542f}\u{52a8}\u{672c}\u{5730}\u{76d1}\u{542c}"
                .to_string(),
        );
    }

    let running = runtime
        .handle
        .as_ref()
        .is_some_and(|handle| handle.health().running);
    if state.enabled && !running {
        let message = match runtime.last_error.take() {
            Some(existing) if existing.contains("15722") => existing,
            Some(existing) => format!("{existing}\u{ff1b}15722 \u{672a}\u{76d1}\u{542c}"),
            None => "15722 \u{672a}\u{76d1}\u{542c}".to_string(),
        };
        runtime.last_error = Some(message);
    }

    if cc_switch_process_running() {
        steps.push(
            "\u{68c0}\u{6d4b}\u{5230} cc-switch\u{ff0c}\u{8bf7}\u{4e0d}\u{8981}\u{540c}\u{65f6}\u{5f00}\u{542f}\u{4e24}\u{4e2a}\u{5207}\u{6362}\u{5668}"
                .to_string(),
        );
    }

    Ok(RepairFastSwitchReport {
        steps,
        status: proxy_status_from(&state, &runtime),
    })
}

#[tauri::command]
async fn set_outbound_proxy_mode(
    mode: String,
    runtime: tauri::State<'_, ProxyRuntime>,
) -> Result<LocalProxyStatus, String> {
    let parsed = parse_outbound_proxy_mode(&mode)?;
    let paths = app_paths()?;
    let mut runtime = runtime.inner.lock().await;
    let mut state = load_proxy_state(&paths)?;
    let mode_changed = state.outbound_proxy_mode != parsed;
    state.outbound_proxy_mode = parsed;
    if mode_changed {
        state.revision = state.revision.saturating_add(1);
        runtime.use_system_proxy = None;
    }
    write_proxy_state(&paths, &state)?;
    if state.enabled {
        match load_proxy_route(&paths, &state) {
            Ok((_, route)) => {
                match start_proxy_handle(&paths, &state, &mut runtime, route, false).await {
                    Ok(changed) => {
                        if changed {
                            remember_codex_restart_for_loopback_bypass(&paths, &mut state);
                        }
                    }
                    Err(error) => runtime.last_error = Some(error),
                }
            }
            Err(error) => runtime.last_error = Some(error),
        }
    }
    Ok(proxy_status_from(&state, &runtime))
}

fn salvage_proxy_state(paths: &AppPaths) -> StoredProxyState {
    let Ok(contents) = fs::read_to_string(&paths.proxy_state) else {
        return StoredProxyState::default();
    };
    serde_json::from_str::<StoredProxyState>(strip_utf8_bom(&contents)).unwrap_or_default()
}

fn coerce_proxy_state_for_repair(state: &mut StoredProxyState) {
    state.schema_version = PROXY_STATE_SCHEMA_VERSION;
    state.port = LOCAL_PROXY_PORT;
    if validate_proxy_state(state).is_ok() {
        return;
    }
    state.enabled = false;
    state.activation_transaction_id = None;
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

#[tauri::command]
fn restart_codex() -> Result<String, String> {
    let launched = launch_restart_codex()?;
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

#[tauri::command]
fn detect_outbound_proxy() -> outbound_proxy::OutboundProxyStatus {
    outbound_proxy::detect_outbound_proxy()
}

#[tauri::command]
async fn diagnose_outbound_network(
    probe_base_url: Option<String>,
    try_start: Option<bool>,
) -> outbound_proxy::OutboundNetworkReport {
    outbound_proxy::diagnose_outbound_network(outbound_proxy::DiagnoseOutboundInput {
        probe_base_url,
        try_start,
    })
    .await
}

#[tauri::command]
async fn ensure_outbound_proxy() -> outbound_proxy::OutboundNetworkReport {
    outbound_proxy::ensure_outbound_proxy().await
}

#[tauri::command]
async fn check_app_update() -> Result<app_update::AppUpdateStatus, String> {
    let skipped = load_skipped_update_version()?;
    app_update::check_for_update(app_update::current_version(), skipped.as_deref()).await
}

#[tauri::command]
fn skip_app_update(version: String) -> Result<(), String> {
    app_update::validate_version_string(&version)?;
    save_skipped_update_version(&version)
}

#[tauri::command]
fn open_app_update(url: String) -> Result<(), String> {
    app_update::open_release_url(&url)
}

#[tauri::command]
async fn install_app_update(
    app: tauri::AppHandle,
    url: String,
    asset_name: Option<String>,
) -> Result<(), String> {
    app_update::download_and_launch_installer(&url, asset_name.as_deref()).await?;
    app.exit(0);
    Ok(())
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
            repair_fast_switch,
            set_outbound_proxy_mode,
            delete_saved_profile,
            restore_latest,
            open_codex,
            restart_codex,
            detect_outbound_proxy,
            diagnose_outbound_network,
            ensure_outbound_proxy,
            check_app_update,
            skip_app_update,
            open_app_update,
            install_app_update,
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
            let persist_changed = ensure_loopback_proxy_bypass(&paths);
            let mut state = load_proxy_state(&paths)?;
            if persist_changed {
                remember_codex_restart_for_loopback_bypass(&paths, &mut state);
            }
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
            if let Ok((_, migrated)) = load_profiles_with_migration(&paths)
                && migrated
            {
                state.requires_codex_restart = true;
                state.revision = state.revision.saturating_add(1);
                let _ = write_proxy_state(&paths, &state);
            }
            prepare_active_proxy_configuration(&paths, &state)?;
            let (_, route) = load_proxy_route(&paths, &state)?;
            let bypass_changed =
                start_proxy_handle(&paths, &state, &mut runtime, route, false).await?;
            if bypass_changed {
                remember_codex_restart_for_loopback_bypass(&paths, &mut state);
            }
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
        .tooltip("LM Codex Switch")
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
        cc_switch_detected: cc_switch_process_running(),
        outbound_proxy_mode: state.outbound_proxy_mode,
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
        outbound_proxy_mode: previous.outbound_proxy_mode,
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

fn adopted_proxy_model(
    current_model_id: Option<&str>,
    profile: &ProviderProfile,
    config_model: &str,
) -> Option<String> {
    if current_model_id == Some(config_model) {
        return None;
    }
    profile
        .models
        .iter()
        .any(|model| model.id == config_model)
        .then(|| config_model.to_string())
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
    let dir_name = helper_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "managed credential helper has an invalid fingerprint".to_string())?;
    let expected_name = helper_file_name();
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
    let digest = sha256_file(helper)?;
    if dir_name == CURRENT_HELPER_DIR {
        if digest == paths.helper_sha256 {
            return Ok(());
        }
        let versioned = helper_root.join(&digest).join(expected_name);
        if versioned != *helper
            && versioned.exists()
            && sha256_file(&versioned).ok().as_deref() == Some(digest.as_str())
        {
            return Ok(());
        }
        return Err("managed credential helper failed its integrity check".to_string());
    }
    let fingerprint = dir_name.to_ascii_lowercase();
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("managed credential helper has an invalid fingerprint".to_string());
    }
    if digest != fingerprint {
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
            serde_json::from_str::<StoredProxyState>(strip_utf8_bom(&contents))
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
    let requested_model = state
        .model_id
        .as_deref()
        .ok_or_else(|| "choose a model before enabling fast switching".to_string())?;
    let profile = load_profiles(paths)?
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "the selected saved connection no longer exists".to_string())?;
    let applied_model = applied_transaction_model_id(paths, state);
    let model_id =
        fallback_proxy_model_id(Some(requested_model), &profile, applied_model.as_deref())
            .ok_or_else(|| "this connection has no models".to_string())?;
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
        &model_id,
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

fn remember_codex_restart_for_loopback_bypass(paths: &AppPaths, state: &mut StoredProxyState) {
    if state.requires_codex_restart {
        return;
    }
    state.requires_codex_restart = true;
    state.revision = state.revision.saturating_add(1);
    let _ = write_proxy_state(paths, state);
}

fn ensure_loopback_proxy_bypass(paths: &AppPaths) -> bool {
    let user_upper = windows_user_env("NO_PROXY");
    let user_lower = windows_user_env("no_proxy");
    let process = process_no_proxy_value();
    let plan = plan_user_no_proxy_persist(
        user_upper.as_deref(),
        user_lower.as_deref(),
        process.as_deref(),
    );
    set_process_no_proxy(&plan.process_value);
    if let Some(original) = plan.backup_original.as_ref() {
        let _ = backup_truncated_no_proxy(paths, original);
    }
    persist_loopback_no_proxy(&plan)
}

fn set_process_no_proxy(value: &str) {
    unsafe {
        std::env::set_var("NO_PROXY", value);
        std::env::set_var("no_proxy", value);
    }
}

fn effective_no_proxy_value() -> Option<String> {
    for key in ["NO_PROXY", "no_proxy"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    windows_user_or_machine_env("NO_PROXY").or_else(|| windows_user_or_machine_env("no_proxy"))
}

fn persist_loopback_no_proxy(plan: &UserNoProxyPersistPlan) -> bool {
    persist_loopback_no_proxy_platform(plan)
}

#[cfg(windows)]
fn persist_loopback_no_proxy_platform(plan: &UserNoProxyPersistPlan) -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::enums::KEY_READ;
    use winreg::enums::KEY_SET_VALUE;

    if !plan.persist_changed {
        return false;
    }
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(env_key) = current_user.open_subkey_with_flags("Environment", KEY_READ | KEY_SET_VALUE)
    else {
        return false;
    };
    if env_key
        .set_value("NO_PROXY", &plan.persist_user_no_proxy)
        .is_err()
    {
        return false;
    }
    if plan.delete_user_no_proxy_alt {
        let _ = env_key.delete_value("no_proxy");
    }
    notify_user_environment_changed();
    true
}

#[cfg(target_os = "macos")]
fn persist_loopback_no_proxy_platform(plan: &UserNoProxyPersistPlan) -> bool {
    fn launchctl_getenv(name: &str) -> Option<String> {
        let output = std::process::Command::new("/usr/bin/launchctl")
            .args(["getenv", name])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8(output.stdout).ok()?;
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    }

    let existing = launchctl_getenv("NO_PROXY").or_else(|| launchctl_getenv("no_proxy"));
    if no_proxy_covers_loopback(existing.as_deref())
        && existing.as_deref() == Some(plan.process_value.as_str())
    {
        return false;
    }
    let persisted = plan.process_value.clone();
    let upper = std::process::Command::new("/usr/bin/launchctl")
        .args(["setenv", "NO_PROXY", &persisted])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    let lower = std::process::Command::new("/usr/bin/launchctl")
        .args(["setenv", "no_proxy", &persisted])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    let _ = plan.process_value;
    upper || lower
}

#[cfg(not(any(windows, target_os = "macos")))]
fn persist_loopback_no_proxy_platform(_plan: &UserNoProxyPersistPlan) -> bool {
    false
}

#[cfg(windows)]
fn windows_user_or_machine_env(name: &str) -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::enums::KEY_READ;

    let user = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ)
        .ok()
        .and_then(|key| key.get_value::<String, _>(name).ok());
    if let Some(value) = user.filter(|value| !value.trim().is_empty()) {
        return Some(value);
    }
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
            KEY_READ,
        )
        .ok()
        .and_then(|key| key.get_value::<String, _>(name).ok())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(not(windows))]
fn windows_user_or_machine_env(_name: &str) -> Option<String> {
    None
}

fn detected_http_proxy_url() -> Option<String> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    windows_user_or_machine_env("HTTPS_PROXY")
        .or_else(|| windows_user_or_machine_env("https_proxy"))
        .or_else(|| windows_user_or_machine_env("HTTP_PROXY"))
        .or_else(|| windows_user_or_machine_env("http_proxy"))
}

async fn verify_loopback_not_intercepted(port: u16) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{port}/health");
    let merged = merge_no_proxy(effective_no_proxy_value().as_deref());
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .http1_only();
    if let Some(proxy_url) = detected_http_proxy_url()
        && let Ok(proxy) = reqwest::Proxy::all(proxy_url)
    {
        builder = builder.proxy(proxy.no_proxy(reqwest::NoProxy::from_string(&merged)));
    } else {
        builder = builder.no_proxy();
    }
    let client = builder
        .build()
        .map_err(|_| "could not verify the local proxy listener".to_string())?;
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(_) => return Ok(()),
    };
    if response.status() == reqwest::StatusCode::BAD_GATEWAY {
        return Err(
            "a local system proxy is intercepting 127.0.0.1; fully restart Codex so it can reach the fast-switch listener"
                .to_string(),
        );
    }
    Ok(())
}

async fn start_proxy_handle(
    paths: &AppPaths,
    state: &StoredProxyState,
    runtime: &mut ProxyRuntimeState,
    route: RouteConfig,
    allow_create_token: bool,
) -> Result<bool, String> {
    let persistent_changed = ensure_loopback_proxy_bypass(paths);
    let use_system_proxy = resolve_use_system_proxy(state);
    let running_same_proxy = runtime
        .handle
        .as_ref()
        .is_some_and(|handle| handle.health().running)
        && runtime.use_system_proxy == Some(use_system_proxy);
    if running_same_proxy {
        let handle = runtime
            .handle
            .as_ref()
            .expect("running handle checked above");
        handle.set_active_route(route);
        runtime.last_error = None;
        return Ok(persistent_changed);
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
        use_system_proxy,
        bindings_path: Some(proxy_bindings_path(paths)),
        ..ProxyStartOptions::default()
    };
    let handle = LocalProxy::start(options, entry_bearer)
        .await
        .map_err(|_| "the local proxy could not start on 127.0.0.1".to_string())?;
    if handle.listen_addr().port() != state.port {
        let _ = handle.shutdown().await;
        return Err("the local proxy started on an unexpected port".to_string());
    }
    remember_saved_proxy_routes(paths, &handle);
    handle.set_active_route(route);
    if let Err(error) = verify_loopback_not_intercepted(state.port).await {
        let _ = handle.shutdown().await;
        return Err(error);
    }
    runtime.handle = Some(handle);
    runtime.use_system_proxy = Some(use_system_proxy);
    runtime.last_error = None;
    Ok(persistent_changed)
}

async fn rollback_proxy_runtime(
    paths: &AppPaths,
    previous: &StoredProxyState,
    runtime: &mut ProxyRuntimeState,
) {
    if previous.enabled {
        let rollback = match load_proxy_route(paths, previous) {
            Ok((_, route)) => start_proxy_handle(paths, previous, runtime, route, false)
                .await
                .map(|_| ()),
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

fn proxy_bindings_path(paths: &AppPaths) -> PathBuf {
    paths.state.join("proxy-bindings.json")
}

fn remember_saved_proxy_routes(paths: &AppPaths, handle: &ProxyHandle) {
    let Some(bindings) = read_proxy_bindings(&proxy_bindings_path(paths)) else {
        return;
    };
    let mut keys = Vec::new();
    for binding in bindings.recent {
        keys.push((binding.route_id, binding.selected_model));
    }
    for binding in bindings.threads {
        keys.push((binding.route_id, binding.selected_model));
    }
    keys.sort();
    keys.dedup();
    for (profile_id, model_id) in keys {
        let snapshot = StoredProxyState {
            profile_id: Some(profile_id),
            model_id: Some(model_id),
            enabled: true,
            ..StoredProxyState::default()
        };
        if let Ok((_, route)) = load_proxy_route(paths, &snapshot) {
            handle.remember_route(route);
        }
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
    update_preference: PathBuf,
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
    let helper = state
        .join("helpers")
        .join(CURRENT_HELPER_DIR)
        .join(helper_file_name());
    Ok(AppPaths {
        codex_home: codex_home.clone(),
        state: state.clone(),
        config: codex_home.join("config.toml"),
        catalog: state.join("models.json"),
        profiles: state.join("profiles.json"),
        official_profile: state.join("official-profile.json"),
        proxy_state: state.join("proxy.json"),
        update_preference: state.join("update-preference.json"),
        backups: state.join("backups"),
        executable,
        helper,
        helper_sha256,
    })
}

fn load_profiles(paths: &AppPaths) -> Result<ProfileStore, String> {
    Ok(load_profiles_with_migration(paths)?.0)
}

fn load_profiles_with_migration(paths: &AppPaths) -> Result<(ProfileStore, bool), String> {
    match fs::symlink_metadata(&paths.profiles) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("saved connections path is not a safe regular file".to_string())
        }
        Ok(_) => {
            let contents = fs::read_to_string(&paths.profiles)
                .map_err(|_| "could not read saved connections".to_string())?;
            let (store, migrated) = parse_profile_store_with_migration(&contents)
                .map_err(|_| "saved connections file is invalid".to_string())?;
            if migrated
                && let Ok(rendered) = render_profile_store(&store)
            {
                let _ = write_profiles(paths, &rendered);
            }
            Ok((store, migrated))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok((ProfileStore::default(), false))
        }
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

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePreference {
    schema_version: u32,
    skipped_version: Option<String>,
}

fn load_skipped_update_version() -> Result<Option<String>, String> {
    let paths = app_paths()?;
    match fs::symlink_metadata(&paths.update_preference) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("update preference path is not a safe regular file".to_string())
        }
        Ok(_) => {
            let contents = fs::read_to_string(&paths.update_preference)
                .map_err(|_| "could not read update preference".to_string())?;
            let parsed = serde_json::from_str::<UpdatePreference>(&contents)
                .map_err(|_| "update preference is invalid".to_string())?;
            if parsed.schema_version != 1 {
                return Ok(None);
            }
            Ok(parsed
                .skipped_version
                .filter(|value| app_update::validate_version_string(value).is_ok()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("could not inspect update preference".to_string()),
    }
}

fn save_skipped_update_version(version: &str) -> Result<(), String> {
    let paths = app_paths()?;
    ensure_state_root(&paths)?;
    let rendered = serde_json::to_vec_pretty(&UpdatePreference {
        schema_version: 1,
        skipped_version: Some(version.to_string()),
    })
    .map_err(|_| "could not encode update preference".to_string())?;
    write_private_file(&paths.update_preference, &rendered)
        .map_err(|_| "could not save update preference".to_string())
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

const CURRENT_HELPER_DIR: &str = "current";

fn helper_file_name() -> &'static str {
    if cfg!(windows) {
        "codex-provider-switcher-helper.exe"
    } else {
        "codex-provider-switcher-helper"
    }
}

fn versioned_helper_path(paths: &AppPaths) -> PathBuf {
    paths
        .state
        .join("helpers")
        .join(&paths.helper_sha256)
        .join(helper_file_name())
}

fn ensure_stable_helper(paths: &AppPaths) -> Result<(), String> {
    ensure_state_root(paths)?;
    ensure_regular_source(&paths.executable)?;
    let versioned = versioned_helper_path(paths);
    install_helper_copy(&paths.executable, &versioned, &paths.helper_sha256, false)?;
    match install_helper_copy(&versioned, &paths.helper, &paths.helper_sha256, true) {
        Ok(()) => Ok(()),
        Err(_) if paths.helper.exists() => {
            ensure_regular_source(&paths.helper)?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn install_helper_copy(
    source: &Path,
    destination: &Path,
    expected_sha: &str,
    replace: bool,
) -> Result<(), String> {
    ensure_regular_source(source)?;
    if destination.exists() {
        ensure_regular_source(destination)?;
        if sha256_file(destination)? == expected_sha {
            return Ok(());
        }
        if !replace {
            return Err("the installed credential helper failed its integrity check".to_string());
        }
    }

    let helper_dir = destination
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

    let mut last_error = "could not install the credential helper".to_string();
    for _ in 0..5 {
        if destination.exists() {
            if fs::remove_file(destination).is_err() {
                last_error = "could not replace the credential helper".to_string();
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        }
        let mut reader =
            File::open(source).map_err(|_| "could not read the app executable".to_string())?;
        let mut temporary = match NamedTempFile::new_in(helper_dir) {
            Ok(temporary) => temporary,
            Err(_) => {
                last_error = "could not stage the credential helper".to_string();
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if io::copy(&mut reader, temporary.as_file_mut()).is_err() {
            last_error = "could not copy the credential helper".to_string();
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        if temporary.as_file_mut().sync_all().is_err() {
            last_error = "could not sync the credential helper".to_string();
            continue;
        }
        if set_helper_permissions(temporary.as_file()).is_err() {
            last_error = "could not secure the credential helper".to_string();
            continue;
        }
        match temporary.persist_noclobber(destination) {
            Ok(_) => {
                if sha256_file(destination)? != expected_sha {
                    return Err(
                        "the installed credential helper failed its integrity check".to_string(),
                    );
                }
                sync_helper_parent(destination)
                    .map_err(|_| "could not finalize the credential helper".to_string())?;
                return Ok(());
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if destination.exists()
                    && sha256_file(destination).ok().as_deref() == Some(expected_sha)
                {
                    return Ok(());
                }
                last_error = "could not install the credential helper".to_string();
            }
            Err(_) => last_error = "could not install the credential helper".to_string(),
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(last_error)
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
            .map(|contents| strip_utf8_bom(&contents).to_string())
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
        if entry.file_name() == std::ffi::OsStr::new(".quarantine") {
            continue;
        }
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

fn strip_utf8_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn process_no_proxy_value() -> Option<String> {
    for key in ["NO_PROXY", "no_proxy"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn backup_truncated_no_proxy(paths: &AppPaths, original: &str) -> Result<(), String> {
    let _ = fs::create_dir_all(&paths.state);
    write_private_file(&paths.state.join("no_proxy.bak.txt"), original.as_bytes())
        .map_err(|_| "could not backup NO_PROXY".to_string())
}

#[cfg(windows)]
fn windows_user_env(name: &str) -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::enums::KEY_READ;

    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ)
        .ok()
        .and_then(|key| key.get_value::<String, _>(name).ok())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(not(windows))]
fn windows_user_env(_name: &str) -> Option<String> {
    None
}

fn fallback_proxy_model_id(
    requested: Option<&str>,
    profile: &ProviderProfile,
    applied: Option<&str>,
) -> Option<String> {
    for candidate in [requested, applied].into_iter().flatten() {
        if profile.models.iter().any(|model| model.id == candidate) {
            return Some(candidate.to_string());
        }
    }
    profile.models.first().map(|model| model.id.clone())
}

fn applied_transaction_model_id(paths: &AppPaths, state: &StoredProxyState) -> Option<String> {
    let path = proxy_activation_manifest_path(paths, state).ok()?;
    read_proxy_activation_manifest_metadata(paths, &path)
        .ok()
        .and_then(|manifest| manifest.model_id)
}

fn parse_outbound_proxy_mode(value: &str) -> Result<OutboundProxyMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(OutboundProxyMode::Auto),
        "direct" => Ok(OutboundProxyMode::Direct),
        "system" => Ok(OutboundProxyMode::System),
        _ => Err("outbound proxy mode must be auto, direct, or system".to_string()),
    }
}

fn resolve_use_system_proxy(state: &StoredProxyState) -> bool {
    match state.outbound_proxy_mode {
        OutboundProxyMode::Direct => false,
        OutboundProxyMode::System => true,
        OutboundProxyMode::Auto => cached_outbound_listening(),
    }
}

fn cached_outbound_listening() -> bool {
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    if let Some((at, listening)) = *cache
        && at.elapsed() < Duration::from_secs(2)
    {
        return listening;
    }
    let listening = outbound_proxy::detect_outbound_proxy().listening == Some(true);
    *cache = Some((Instant::now(), listening));
    listening
}

fn cc_switch_process_running() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq cc-switch.exe", "/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let Ok(output) = output else {
            return false;
        };
        String::from_utf8_lossy(&output.stdout)
            .to_ascii_lowercase()
            .contains("cc-switch")
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn should_quarantine_backup_directory(
    dir_name: &str,
    requires_manual: bool,
    manifest_readable: bool,
) -> bool {
    if dir_name == ".quarantine" || dir_name.is_empty() {
        return false;
    }
    !requires_manual && (!manifest_readable || Uuid::parse_str(dir_name).is_err())
}

fn quarantine_invalid_backups(paths: &AppPaths) -> usize {
    let entries = match fs::read_dir(&paths.backups) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let dest_root = paths.backups.join(".quarantine").join(stamp.to_string());
    let mut moved = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(".quarantine") {
            continue;
        }
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("manifest.json");
        let requires_manual =
            leftover_backup_requires_manual_review(&manifest_path, &paths.config, &paths.catalog);
        let readable = fs::read(&manifest_path)
            .ok()
            .and_then(|bytes| {
                serde_json::from_str::<BackupManifest>(strip_utf8_bom(
                    std::str::from_utf8(&bytes).ok()?,
                ))
                .ok()
            })
            .is_some();
        let name_str = name.to_string_lossy();
        if !should_quarantine_backup_directory(name_str.as_ref(), requires_manual, readable) {
            continue;
        }
        if moved == 0 {
            let _ = create_private_directory(&dest_root);
        }
        if fs::rename(&dir, dest_root.join(&name)).is_ok() {
            moved += 1;
        }
    }
    moved
}

fn recover_prepared_manifests(paths: &AppPaths) -> usize {
    recover_prepared_manifests_in(&paths.backups, &paths.config, &paths.catalog)
}

fn recover_prepared_manifests_in(backups: &Path, config: &Path, catalog: &Path) -> usize {
    let entries = match fs::read_dir(backups) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(_) => return 1,
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name() != std::ffi::OsStr::new(".quarantine"))
        .filter(|entry| {
            leftover_backup_requires_manual_review(
                &entry.path().join("manifest.json"),
                config,
                catalog,
            )
        })
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
    fn proxy_adopts_a_codex_model_when_it_still_belongs_to_the_connection() {
        let profile = profile_with_models("luming", &["grok-4.6", "gpt-5.6-sol"]);
        assert_eq!(
            adopted_proxy_model(Some("grok-4.6"), &profile, "gpt-5.6-sol").as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            adopted_proxy_model(Some("gpt-5.6-sol"), &profile, "gpt-5.6-sol"),
            None
        );
        assert_eq!(
            adopted_proxy_model(Some("grok-4.6"), &profile, "unknown-model"),
            None
        );
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

    #[test]
    fn strip_utf8_bom_removes_only_the_prefix() {
        assert_eq!(strip_utf8_bom("plain"), "plain");
        assert_eq!(
            strip_utf8_bom("\u{feff}{\"enabled\":false}"),
            "{\"enabled\":false}"
        );
    }

    #[test]
    fn proxy_state_json_can_be_parsed_after_stripping_bom() {
        let json = "\u{feff}{\n  \"schemaVersion\": 2,\n  \"enabled\": false,\n  \"port\": 15722,\n  \"revision\": 0\n}\n";
        let state: StoredProxyState = serde_json::from_str(strip_utf8_bom(json)).expect("bom json");
        assert!(!state.enabled);
        assert_eq!(state.port, LOCAL_PROXY_PORT);
        assert_eq!(state.outbound_proxy_mode, OutboundProxyMode::Auto);
    }

    #[test]
    fn fallback_proxy_model_prefers_requested_then_applied_then_first() {
        let profile = profile_with_models("luming", &["grok-4.6", "gpt-5.6-sol"]);
        assert_eq!(
            fallback_proxy_model_id(Some("gpt-5.6-sol"), &profile, Some("grok-4.6")).as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            fallback_proxy_model_id(Some("missing"), &profile, Some("grok-4.6")).as_deref(),
            Some("grok-4.6")
        );
        assert_eq!(
            fallback_proxy_model_id(Some("missing"), &profile, Some("also-missing")).as_deref(),
            Some("grok-4.6")
        );
        let empty = profile_with_models("empty", &[]);
        assert_eq!(fallback_proxy_model_id(Some("x"), &empty, None), None);
    }

    #[test]
    fn invalid_backup_directories_are_quarantined_except_true_leftovers() {
        assert!(!should_quarantine_backup_directory(
            ".quarantine",
            false,
            false
        ));
        assert!(should_quarantine_backup_directory(
            "orphan-dir",
            false,
            false
        ));
        let uuid = Uuid::new_v4().to_string();
        assert!(!should_quarantine_backup_directory(&uuid, true, true));
        assert!(!should_quarantine_backup_directory(&uuid, false, true));
        assert!(should_quarantine_backup_directory(&uuid, false, false));
    }

    #[test]
    fn recover_prepared_manifests_ignores_orphan_backup_directories() {
        let root = tempfile::tempdir().expect("tempdir");
        let backups = root.path().join("backups");
        let config = root.path().join("config.toml");
        let catalog = root.path().join("models.json");
        std::fs::create_dir_all(backups.join("not-a-uuid")).unwrap();
        std::fs::create_dir_all(backups.join(".quarantine").join("1")).unwrap();
        std::fs::write(&config, "model = \"x\"\n").unwrap();
        std::fs::write(&catalog, "{\n  \"models\": []\n}\n").unwrap();
        assert_eq!(
            recover_prepared_manifests_in(&backups, &config, &catalog),
            0
        );
    }

    #[test]
    fn outbound_proxy_mode_parses_known_values() {
        assert_eq!(
            parse_outbound_proxy_mode("AUTO").unwrap(),
            OutboundProxyMode::Auto
        );
        assert_eq!(
            parse_outbound_proxy_mode("direct").unwrap(),
            OutboundProxyMode::Direct
        );
        assert_eq!(
            parse_outbound_proxy_mode("system").unwrap(),
            OutboundProxyMode::System
        );
        assert!(parse_outbound_proxy_mode("clash").is_err());
    }
}

