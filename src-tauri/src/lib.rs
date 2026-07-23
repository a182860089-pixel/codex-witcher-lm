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

use codex_provider_switcher_core::BackupManifest;
use codex_provider_switcher_core::BackupStatus;
use codex_provider_switcher_core::CurrentCodexConfig;
use codex_provider_switcher_core::FetchedModel;
use codex_provider_switcher_core::ProfileStore;
use codex_provider_switcher_core::ProviderProfile;
use codex_provider_switcher_core::apply_config_plan;
use codex_provider_switcher_core::backup_matches_applied;
use codex_provider_switcher_core::create_private_directory;
use codex_provider_switcher_core::credential_account_for;
use codex_provider_switcher_core::fetch_models;
use codex_provider_switcher_core::inspect_config;
use codex_provider_switcher_core::model_endpoint_candidates;
use codex_provider_switcher_core::normalize_api_base_url;
use codex_provider_switcher_core::parse_profile_store;
use codex_provider_switcher_core::plan_config;
use codex_provider_switcher_core::recover_prepared_backup;
use codex_provider_switcher_core::remove_profile;
use codex_provider_switcher_core::render_profile_store;
use codex_provider_switcher_core::restore_backup;
use codex_provider_switcher_core::upsert_profile;
use codex_provider_switcher_core::verify_credential_binding as verify_core_credential_binding;
use codex_provider_switcher_core::write_private_file;
use codex_provider_switcher_credentials as credentials;
use codex_provider_switcher_launcher::authorize_codex_parent;
use codex_provider_switcher_launcher::open_codex as launch_codex;
use directories::BaseDirs;
use fs4::FileExt;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tempfile::NamedTempFile;
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
    Ok(AppState {
        config_exists: paths.config.is_file(),
        latest_backup: latest_applied_manifest(&paths)?,
        recovery_warnings,
        config_path: paths.config,
        current,
        profiles,
        profile_warning,
    })
}

fn apply_profile(profile: ProviderProfile, selected_model: String) -> Result<ApplySummary, String> {
    if profile.credential_required {
        let account =
            credential_account_for(&profile.id, &profile.base_url).map_err(redacted_core_error)?;
        if !credentials::exists(&account)? {
            return Err(
                "store a credential for this exact provider endpoint before applying".to_string(),
            );
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
    let result =
        apply_config_plan(&paths.config, &paths.backups, &plan).map_err(redacted_core_error)?;
    Ok(ApplySummary {
        transaction_id: result.transaction_id.to_string(),
        backup_manifest: result.manifest_path,
        manifest_finalized: result.manifest_finalized,
    })
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
fn save_profile(
    input: SaveProfileInput,
    vault: tauri::State<'_, DiscoveryVault>,
) -> Result<(), String> {
    let paths = app_paths()?;
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

#[tauri::command]
fn apply_saved_profile(profile_id: String, selected_model: String) -> Result<ApplySummary, String> {
    let paths = app_paths()?;
    let profile = load_profiles(&paths)?
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "the saved connection no longer exists".to_string())?;
    apply_profile(profile, selected_model)
}

#[tauri::command]
fn delete_saved_profile(profile_id: String) -> Result<bool, String> {
    let paths = app_paths()?;
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
    require_clean_recovery_state(&paths)?;
    let manifest = latest_applied_manifest(&paths)?
        .ok_or_else(|| "no applied switcher backup is available".to_string())?;
    let result =
        restore_backup(&manifest, &paths.config, &paths.catalog).map_err(redacted_core_error)?;
    Ok(result.transaction_id.to_string())
}

#[tauri::command]
fn open_codex() -> Result<String, String> {
    launch_codex()
}

pub fn run() {
    tauri::Builder::default()
        .manage(DiscoveryVault::default())
        .invoke_handler(tauri::generate_handler![
            inspect_state,
            discover_models,
            stage_credential,
            cancel_discovery,
            save_profile,
            apply_saved_profile,
            delete_saved_profile,
            restore_latest,
            open_codex,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Codex Provider Switcher");
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

struct AppPaths {
    state: PathBuf,
    config: PathBuf,
    catalog: PathBuf,
    profiles: PathBuf,
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
        state: state.clone(),
        config: codex_home.join("config.toml"),
        catalog: state.join("models.json"),
        profiles: state.join("profiles.json"),
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
        if manifest.status == BackupStatus::Restored
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
