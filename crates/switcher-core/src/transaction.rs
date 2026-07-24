use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use fs4::FileExt;
use serde::Deserialize;
use serde::Serialize;
use tempfile::NamedTempFile;
use toml_edit::DocumentMut;
use toml_edit::Item;
use toml_edit::Table;
use uuid::Uuid;

use crate::config::ConfigPlan;
use crate::config::LOCAL_PROXY_PROVIDER_ID;
use crate::config::sha256_hex;
use crate::config::verify_proxy_config_binding;
use crate::error::Result;
use crate::error::SwitcherError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackupStatus {
    Prepared,
    Applied,
    Restoring,
    Detaching,
    Restored,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub existed: bool,
    pub original_sha256: Option<String>,
    pub applied_sha256: String,
    pub backup_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManifest {
    pub schema_version: u32,
    pub transaction_id: Uuid,
    pub created_unix_ms: u128,
    pub provider_id: String,
    pub model_id: String,
    pub status: BackupStatus,
    pub config: FileSnapshot,
    pub catalog: FileSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_detach: Option<ProxyDetachJournal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyDetachJournal {
    pub before_sha256: String,
    pub after_existed: bool,
    pub after_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyResult {
    pub manifest_path: PathBuf,
    pub transaction_id: Uuid,
    pub manifest_finalized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestoreResult {
    pub transaction_id: Uuid,
    pub manifest_finalized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOutcome {
    NotNeeded,
    FinalizedApplied,
    RolledBack,
}

pub fn create_private_directory(path: &Path) -> Result<()> {
    if path.exists() {
        return Err(SwitcherError::Conflict(format!(
            "{} already exists",
            path.display()
        )));
    }
    fs::create_dir_all(parent(path)?)?;
    create_private_transaction_dir(path)
}

pub fn apply_config_plan(
    config_path: &Path,
    backup_root: &Path,
    plan: &ConfigPlan,
) -> Result<ApplyResult> {
    apply_config_plan_with_transaction_id(config_path, backup_root, plan, Uuid::new_v4())
}

pub fn apply_config_plan_with_transaction_id(
    config_path: &Path,
    backup_root: &Path,
    plan: &ConfigPlan,
    transaction_id: Uuid,
) -> Result<ApplyResult> {
    if transaction_id.is_nil() {
        return Err(SwitcherError::Validation(
            "transaction ID must not be nil".to_string(),
        ));
    }
    ensure_regular_or_missing(config_path)?;
    ensure_regular_or_missing(&plan.catalog_path)?;
    fs::create_dir_all(parent(config_path)?)?;
    fs::create_dir_all(parent(&plan.catalog_path)?)?;
    fs::create_dir_all(backup_root)?;

    let lock_path = lock_path_for(config_path);
    ensure_regular_or_missing(&lock_path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)?;
    FileExt::lock(&lock)?;

    let result = apply_locked(config_path, backup_root, plan, transaction_id);
    FileExt::unlock(&lock)?;
    result
}

fn apply_locked(
    config_path: &Path,
    backup_root: &Path,
    plan: &ConfigPlan,
    transaction_id: Uuid,
) -> Result<ApplyResult> {
    let current_config = read_optional(config_path)?;
    let current_config_contents = current_config.as_deref().unwrap_or_default();
    let current_hash = sha256_hex(current_config_contents);
    if current_hash != plan.expected_config_sha256 {
        return Err(SwitcherError::Conflict(format!(
            "{} no longer matches the planned SHA-256",
            config_path.display()
        )));
    }
    std::str::from_utf8(current_config_contents).map_err(|_| SwitcherError::InvalidUtf8)?;

    let current_catalog = read_optional(&plan.catalog_path)?;
    let expected_config = ExpectedFileState::from_contents(current_config.as_deref());
    let expected_catalog = ExpectedFileState::from_contents(current_catalog.as_deref());
    let transaction_dir = backup_root.join(transaction_id.to_string());
    create_private_transaction_dir(&transaction_dir)?;
    let config_backup = snapshot_backup(
        current_config_contents,
        current_config.is_some(),
        &transaction_dir,
        "config.before",
    )?;
    let catalog_backup = snapshot_backup(
        current_catalog.as_deref().unwrap_or_default(),
        current_catalog.is_some(),
        &transaction_dir,
        "catalog.before",
    )?;

    let mut manifest = BackupManifest {
        schema_version: 1,
        transaction_id,
        created_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        provider_id: plan.provider_id.clone(),
        model_id: plan.model_id.clone(),
        status: BackupStatus::Prepared,
        config: FileSnapshot {
            path: config_path.to_path_buf(),
            existed: current_config.is_some(),
            original_sha256: config_backup
                .as_ref()
                .map(|_| sha256_hex(current_config_contents)),
            applied_sha256: sha256_hex(plan.rendered_config.as_bytes()),
            backup_file: config_backup,
        },
        catalog: FileSnapshot {
            path: plan.catalog_path.clone(),
            existed: current_catalog.is_some(),
            original_sha256: catalog_backup
                .as_ref()
                .map(|_| sha256_hex(current_catalog.as_deref().unwrap_or_default())),
            applied_sha256: sha256_hex(plan.rendered_catalog.as_bytes()),
            backup_file: catalog_backup,
        },
        proxy_detach: None,
    };
    let manifest_path = transaction_dir.join("manifest.json");
    write_manifest(&manifest_path, &manifest)?;

    atomic_write_expected(
        &plan.catalog_path,
        &expected_catalog,
        plan.rendered_catalog.as_bytes(),
    )?;
    if let Err(error) = atomic_write_expected(
        config_path,
        &expected_config,
        plan.rendered_config.as_bytes(),
    ) {
        let catalog_original = read_verified_backup(&manifest.catalog)?;
        restore_snapshot_expected(
            &manifest.catalog,
            KnownSnapshotState::Applied,
            catalog_original.as_deref(),
        )?;
        return Err(error);
    }

    manifest.status = BackupStatus::Applied;
    let manifest_finalized = write_manifest(&manifest_path, &manifest).is_ok();
    Ok(ApplyResult {
        manifest_path,
        transaction_id,
        manifest_finalized,
    })
}

pub fn restore_backup(
    manifest_path: &Path,
    expected_config_path: &Path,
    expected_catalog_path: &Path,
) -> Result<RestoreResult> {
    let manifest =
        load_scoped_manifest(manifest_path, expected_config_path, expected_catalog_path)?;
    ensure_regular_or_missing(&manifest.config.path)?;
    ensure_regular_or_missing(&manifest.catalog.path)?;

    let lock_path = lock_path_for(&manifest.config.path);
    ensure_regular_or_missing(&lock_path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)?;
    FileExt::lock(&lock)?;

    let result = restore_locked(manifest_path, manifest);
    FileExt::unlock(&lock)?;
    result
}

pub fn restore_proxy_config_preserving_unrelated_changes(
    manifest_path: &Path,
    expected_config_path: &Path,
    expected_catalog_path: &Path,
    expected_proxy_base_url: &str,
) -> Result<RestoreResult> {
    let manifest =
        load_scoped_manifest(manifest_path, expected_config_path, expected_catalog_path)?;
    ensure_regular_or_missing(&manifest.config.path)?;

    let lock_path = lock_path_for(&manifest.config.path);
    ensure_regular_or_missing(&lock_path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)?;
    FileExt::lock(&lock)?;

    let result = detach_proxy_config_locked(manifest_path, manifest, expected_proxy_base_url);
    FileExt::unlock(&lock)?;
    result
}

pub fn recover_prepared_backup(
    manifest_path: &Path,
    expected_config_path: &Path,
    expected_catalog_path: &Path,
) -> Result<RecoveryOutcome> {
    let mut manifest =
        load_scoped_manifest(manifest_path, expected_config_path, expected_catalog_path)?;
    if !matches!(
        manifest.status,
        BackupStatus::Prepared | BackupStatus::Restoring
    ) {
        return Ok(RecoveryOutcome::NotNeeded);
    }

    let lock_path = lock_path_for(&manifest.config.path);
    ensure_regular_or_missing(&lock_path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)?;
    FileExt::lock(&lock)?;

    let config_state = known_snapshot_state(&manifest.config)?;
    let catalog_state = known_snapshot_state(&manifest.catalog)?;
    let outcome = if manifest.status == BackupStatus::Prepared
        && config_state.is_applied()
        && catalog_state.is_applied()
    {
        manifest.status = BackupStatus::Applied;
        write_manifest(manifest_path, &manifest)?;
        RecoveryOutcome::FinalizedApplied
    } else if config_state.is_known() && catalog_state.is_known() {
        let config_original = read_verified_backup(&manifest.config)?;
        let catalog_original = read_verified_backup(&manifest.catalog)?;
        restore_snapshot_expected(&manifest.config, config_state, config_original.as_deref())?;
        restore_snapshot_expected(
            &manifest.catalog,
            catalog_state,
            catalog_original.as_deref(),
        )?;
        verify_original_state(&manifest.config)?;
        verify_original_state(&manifest.catalog)?;
        manifest.status = BackupStatus::Restored;
        write_manifest(manifest_path, &manifest)?;
        RecoveryOutcome::RolledBack
    } else {
        FileExt::unlock(&lock)?;
        return Err(SwitcherError::Conflict(
            "an interrupted transaction contains an unknown file state".to_string(),
        ));
    };
    FileExt::unlock(&lock)?;
    Ok(outcome)
}

pub fn backup_matches_applied(manifest: &BackupManifest) -> Result<bool> {
    Ok(snapshot_matches_applied(&manifest.config)? && snapshot_matches_applied(&manifest.catalog)?)
}

pub fn verify_backup_integrity(
    manifest_path: &Path,
    expected_config_path: &Path,
    expected_catalog_path: &Path,
) -> Result<()> {
    let manifest =
        load_scoped_manifest(manifest_path, expected_config_path, expected_catalog_path)?;
    read_verified_backup(&manifest.config)?;
    read_verified_backup(&manifest.catalog)?;
    Ok(())
}

pub fn verify_proxy_detach_recoverable(
    manifest_path: &Path,
    expected_config_path: &Path,
    expected_catalog_path: &Path,
) -> Result<()> {
    let manifest =
        load_scoped_manifest(manifest_path, expected_config_path, expected_catalog_path)?;
    if manifest.status != BackupStatus::Detaching {
        return Err(SwitcherError::Validation(
            "the proxy detach journal is not active".to_string(),
        ));
    }
    read_verified_backup(&manifest.config)?;
    read_verified_backup(&manifest.catalog)?;
    let journal = manifest.proxy_detach.as_ref().ok_or_else(|| {
        SwitcherError::Validation("the proxy detach journal is incomplete".to_string())
    })?;
    let current = read_optional(&manifest.config.path)?;
    if current
        .as_deref()
        .is_some_and(|contents| sha256_hex(contents) == journal.before_sha256)
        || detached_state_matches(journal, current.as_deref())
    {
        Ok(())
    } else {
        Err(SwitcherError::Conflict(
            "the Codex configuration changed while the proxy was being detached".to_string(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KnownSnapshotState {
    Original,
    Applied,
    OriginalAndApplied,
    Unknown,
}

impl KnownSnapshotState {
    fn is_original(self) -> bool {
        matches!(self, Self::Original | Self::OriginalAndApplied)
    }

    fn is_applied(self) -> bool {
        matches!(self, Self::Applied | Self::OriginalAndApplied)
    }

    fn is_known(self) -> bool {
        self != Self::Unknown
    }
}

fn known_snapshot_state(snapshot: &FileSnapshot) -> Result<KnownSnapshotState> {
    let current = read_optional(&snapshot.path)?;
    let is_applied = current
        .as_deref()
        .is_some_and(|contents| sha256_hex(contents) == snapshot.applied_sha256);
    let is_original = snapshot_matches_original_contents(snapshot, current.as_deref());
    Ok(match (is_original, is_applied) {
        (true, true) => KnownSnapshotState::OriginalAndApplied,
        (true, false) => KnownSnapshotState::Original,
        (false, true) => KnownSnapshotState::Applied,
        (false, false) => KnownSnapshotState::Unknown,
    })
}

fn snapshot_matches_original_contents(snapshot: &FileSnapshot, current: Option<&[u8]>) -> bool {
    if snapshot.existed {
        current.is_some_and(|contents| {
            snapshot
                .original_sha256
                .as_ref()
                .is_some_and(|expected| sha256_hex(contents) == *expected)
        })
    } else {
        current.is_none() && snapshot.original_sha256.is_none() && snapshot.backup_file.is_none()
    }
}

fn snapshot_matches_original(snapshot: &FileSnapshot) -> Result<bool> {
    let current = read_optional(&snapshot.path)?;
    Ok(snapshot_matches_original_contents(
        snapshot,
        current.as_deref(),
    ))
}

fn load_scoped_manifest(
    manifest_path: &Path,
    expected_config_path: &Path,
    expected_catalog_path: &Path,
) -> Result<BackupManifest> {
    ensure_regular_file(manifest_path)?;
    let manifest: BackupManifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    if manifest.schema_version != 1 {
        return Err(SwitcherError::Validation(format!(
            "unsupported backup schema version {}",
            manifest.schema_version
        )));
    }
    if manifest.config.path != expected_config_path
        || manifest.catalog.path != expected_catalog_path
    {
        return Err(SwitcherError::Validation(
            "backup manifest is outside the expected Codex configuration scope".to_string(),
        ));
    }
    let transaction_dir = manifest_path.parent().ok_or_else(|| {
        SwitcherError::Validation("backup manifest has no transaction directory".to_string())
    })?;
    if manifest_path.file_name().and_then(|name| name.to_str()) != Some("manifest.json")
        || transaction_dir.file_name().and_then(|name| name.to_str())
            != Some(manifest.transaction_id.to_string().as_str())
        || !valid_backup_file(&manifest.config, &transaction_dir.join("config.before"))
        || !valid_backup_file(&manifest.catalog, &transaction_dir.join("catalog.before"))
    {
        return Err(SwitcherError::Validation(
            "backup manifest layout is invalid".to_string(),
        ));
    }
    Ok(manifest)
}

fn valid_backup_file(snapshot: &FileSnapshot, expected: &Path) -> bool {
    match (snapshot.existed, snapshot.backup_file.as_deref()) {
        (true, Some(path)) => path == expected,
        (false, None) => true,
        _ => false,
    }
}

fn restore_locked(manifest_path: &Path, mut manifest: BackupManifest) -> Result<RestoreResult> {
    if manifest.status != BackupStatus::Applied {
        return Err(SwitcherError::Validation(
            "only an applied transaction can be restored".to_string(),
        ));
    }
    verify_applied_hash(&manifest.config)?;
    verify_applied_hash(&manifest.catalog)?;
    let config_original = read_verified_backup(&manifest.config)?;
    let catalog_original = read_verified_backup(&manifest.catalog)?;

    manifest.status = BackupStatus::Restoring;
    write_manifest(manifest_path, &manifest)?;

    restore_snapshot_expected(
        &manifest.config,
        KnownSnapshotState::Applied,
        config_original.as_deref(),
    )?;
    restore_snapshot_expected(
        &manifest.catalog,
        KnownSnapshotState::Applied,
        catalog_original.as_deref(),
    )?;
    verify_original_state(&manifest.config)?;
    verify_original_state(&manifest.catalog)?;

    manifest.status = BackupStatus::Restored;
    let manifest_finalized = write_manifest(manifest_path, &manifest).is_ok();
    Ok(RestoreResult {
        transaction_id: manifest.transaction_id,
        manifest_finalized,
    })
}

fn detach_proxy_config_locked(
    manifest_path: &Path,
    mut manifest: BackupManifest,
    expected_proxy_base_url: &str,
) -> Result<RestoreResult> {
    if manifest.provider_id != LOCAL_PROXY_PROVIDER_ID
        || !matches!(
            manifest.status,
            BackupStatus::Applied | BackupStatus::Detaching
        )
    {
        return Err(SwitcherError::Validation(
            "only an active local-proxy transaction can be safely detached".to_string(),
        ));
    }
    let original = read_verified_backup(&manifest.config)?;
    read_verified_backup(&manifest.catalog)?;
    let current = read_optional(&manifest.config.path)?;
    if manifest.status == BackupStatus::Applied {
        let current = current.as_deref().ok_or_else(|| {
            SwitcherError::Conflict("the active Codex configuration is missing".to_string())
        })?;
        let current_text = std::str::from_utf8(&current).map_err(|_| SwitcherError::InvalidUtf8)?;
        verify_proxy_config_binding(current_text, expected_proxy_base_url)?;
        let current_document = current_text.parse::<DocumentMut>()?;
        if current_document.get("model").and_then(Item::as_str) != Some(manifest.model_id.as_str())
        {
            return Err(SwitcherError::Conflict(
                "the managed local proxy model changed after activation".to_string(),
            ));
        }
        let merged = merge_proxy_owned_config(current, original.as_deref().unwrap_or_default())?;
        let remove_after = !manifest.config.existed && merged.trim().is_empty();
        manifest.proxy_detach = Some(ProxyDetachJournal {
            before_sha256: sha256_hex(current),
            after_existed: !remove_after,
            after_sha256: (!remove_after).then(|| sha256_hex(merged.as_bytes())),
        });
        manifest.status = BackupStatus::Detaching;
        write_manifest(manifest_path, &manifest)?;
    }

    let journal = manifest.proxy_detach.as_ref().ok_or_else(|| {
        SwitcherError::Validation("the proxy detach journal is incomplete".to_string())
    })?;
    let current_is_before = current
        .as_deref()
        .is_some_and(|contents| sha256_hex(contents) == journal.before_sha256);
    let current_is_after = detached_state_matches(journal, current.as_deref());
    if !current_is_before && !current_is_after {
        return Err(SwitcherError::Conflict(
            "the Codex configuration changed while the proxy was being detached".to_string(),
        ));
    }
    if current_is_before {
        let current = current
            .as_deref()
            .expect("a before-state hash requires existing contents");
        let merged = merge_proxy_owned_config(current, original.as_deref().unwrap_or_default())?;
        let remove_after = !manifest.config.existed && merged.trim().is_empty();
        let after_sha256 = (!remove_after).then(|| sha256_hex(merged.as_bytes()));
        if journal.after_existed != !remove_after || journal.after_sha256 != after_sha256 {
            return Err(SwitcherError::Conflict(
                "the proxy detach plan no longer matches its journal".to_string(),
            ));
        }
        let expected = ExpectedFileState::from_contents(Some(current));
        if remove_after {
            remove_file_expected(&manifest.config.path, &expected)?;
        } else if merged.as_bytes() != current {
            atomic_write_expected(&manifest.config.path, &expected, merged.as_bytes())?;
        }
    }

    manifest.status = BackupStatus::Restored;
    let manifest_finalized = write_manifest(manifest_path, &manifest).is_ok();
    Ok(RestoreResult {
        transaction_id: manifest.transaction_id,
        manifest_finalized,
    })
}

fn detached_state_matches(journal: &ProxyDetachJournal, current: Option<&[u8]>) -> bool {
    match (
        journal.after_existed,
        journal.after_sha256.as_deref(),
        current,
    ) {
        (false, None, None) => true,
        (true, Some(expected), Some(contents)) => sha256_hex(contents) == expected,
        _ => false,
    }
}

fn merge_proxy_owned_config(current: &[u8], original: &[u8]) -> Result<String> {
    let current = std::str::from_utf8(current).map_err(|_| SwitcherError::InvalidUtf8)?;
    let original = std::str::from_utf8(original).map_err(|_| SwitcherError::InvalidUtf8)?;
    let mut current = if current.trim().is_empty() {
        DocumentMut::new()
    } else {
        current.parse::<DocumentMut>()?
    };
    let original = if original.trim().is_empty() {
        DocumentMut::new()
    } else {
        original.parse::<DocumentMut>()?
    };

    for key in ["model_provider", "model"] {
        restore_document_item(&mut current, &original, key);
    }

    let original_had_providers = original.as_table().contains_key("model_providers");
    let original_proxy = original
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(LOCAL_PROXY_PROVIDER_ID))
        .cloned();
    if current.as_table().contains_key("model_providers") {
        let providers = current["model_providers"].as_table_mut().ok_or_else(|| {
            SwitcherError::Validation("model_providers must remain a TOML table".to_string())
        })?;
        match original_proxy {
            Some(item) => {
                providers.insert(LOCAL_PROXY_PROVIDER_ID, item);
            }
            None => {
                providers.remove(LOCAL_PROXY_PROVIDER_ID);
            }
        }
        if !original_had_providers && providers.is_empty() {
            current.as_table_mut().remove("model_providers");
        }
    } else if let Some(item) = original_proxy {
        let mut providers = Table::new();
        providers.insert(LOCAL_PROXY_PROVIDER_ID, item);
        current["model_providers"] = Item::Table(providers);
    }

    Ok(current.to_string())
}

fn restore_document_item(current: &mut DocumentMut, original: &DocumentMut, key: &str) {
    if let Some(item) = original.get(key) {
        current.as_table_mut().insert(key, item.clone());
    } else {
        current.as_table_mut().remove(key);
    }
}

fn verify_applied_hash(snapshot: &FileSnapshot) -> Result<()> {
    if !snapshot_matches_applied(snapshot)? {
        return Err(SwitcherError::Conflict(format!(
            "{} changed after the switch; refusing to overwrite it",
            snapshot.path.display()
        )));
    }
    Ok(())
}

fn snapshot_matches_applied(snapshot: &FileSnapshot) -> Result<bool> {
    let current = read_optional(&snapshot.path)?;
    Ok(current
        .as_deref()
        .is_some_and(|contents| sha256_hex(contents) == snapshot.applied_sha256))
}

fn snapshot_backup(
    contents: &[u8],
    existed: bool,
    transaction_dir: &Path,
    file_name: &str,
) -> Result<Option<PathBuf>> {
    if !existed {
        return Ok(None);
    }
    let backup_path = transaction_dir.join(file_name);
    atomic_write_private(&backup_path, contents)?;
    Ok(Some(backup_path))
}

fn read_verified_backup(snapshot: &FileSnapshot) -> Result<Option<Vec<u8>>> {
    if snapshot.existed {
        let backup_path = snapshot.backup_file.as_ref().ok_or_else(|| {
            SwitcherError::Validation(format!("backup is missing for {}", snapshot.path.display()))
        })?;
        let expected_hash = snapshot.original_sha256.as_ref().ok_or_else(|| {
            SwitcherError::Validation(format!(
                "backup hash is missing for {}",
                snapshot.path.display()
            ))
        })?;
        ensure_regular_file(backup_path)?;
        let contents = fs::read(backup_path)?;
        if sha256_hex(&contents) != *expected_hash {
            return Err(SwitcherError::Conflict(format!(
                "backup integrity check failed for {}",
                snapshot.path.display()
            )));
        }
        Ok(Some(contents))
    } else {
        if snapshot.original_sha256.is_some() || snapshot.backup_file.is_some() {
            return Err(SwitcherError::Validation(format!(
                "unexpected backup metadata for {}",
                snapshot.path.display()
            )));
        }
        Ok(None)
    }
}

fn restore_snapshot_expected(
    snapshot: &FileSnapshot,
    current_state: KnownSnapshotState,
    original: Option<&[u8]>,
) -> Result<()> {
    if current_state.is_original() {
        return Ok(());
    }
    if !current_state.is_applied() {
        return Err(SwitcherError::Conflict(format!(
            "{} is not in a recoverable transaction state",
            snapshot.path.display()
        )));
    }
    let expected = ExpectedFileState::Sha256(snapshot.applied_sha256.clone());
    if snapshot.existed {
        let original = original.ok_or_else(|| {
            SwitcherError::Validation(format!("backup is missing for {}", snapshot.path.display()))
        })?;
        atomic_write_expected(&snapshot.path, &expected, original)
    } else {
        remove_file_expected(&snapshot.path, &expected)
    }
}

fn verify_original_state(snapshot: &FileSnapshot) -> Result<()> {
    if !snapshot_matches_original(snapshot)? {
        return Err(SwitcherError::Conflict(format!(
            "{} changed while the transaction was being restored",
            snapshot.path.display()
        )));
    }
    Ok(())
}

fn write_manifest(path: &Path, manifest: &BackupManifest) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    atomic_write_private(path, &bytes)
}

fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    atomic_write_inner(path, contents, true)
}

pub fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    atomic_write_private(path, contents)
}

fn atomic_write_inner(path: &Path, contents: &[u8], force_private: bool) -> Result<()> {
    ensure_regular_or_missing(path)?;
    let parent = parent(path)?;
    fs::create_dir_all(parent)?;
    let existing_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let target_existed = existing_permissions.is_some();
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;

    if force_private {
        set_private_permissions(temporary.as_file())?;
    } else if let Some(permissions) = existing_permissions {
        temporary.as_file().set_permissions(permissions)?;
    } else {
        set_private_permissions(temporary.as_file())?;
    }

    persist_temporary(temporary, path, target_existed)?;
    sync_parent(path)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpectedFileState {
    Missing,
    Sha256(String),
}

impl ExpectedFileState {
    fn from_contents(contents: Option<&[u8]>) -> Self {
        contents
            .map(|contents| Self::Sha256(sha256_hex(contents)))
            .unwrap_or(Self::Missing)
    }
}

fn atomic_write_expected(path: &Path, expected: &ExpectedFileState, contents: &[u8]) -> Result<()> {
    atomic_write_expected_with_hook(path, expected, contents, || {})
}

fn atomic_write_expected_with_hook(
    path: &Path,
    expected: &ExpectedFileState,
    contents: &[u8],
    before_final_validation: impl FnOnce(),
) -> Result<()> {
    ensure_regular_or_missing(path)?;
    let parent = parent(path)?;
    fs::create_dir_all(parent)?;
    let existing_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;

    if let Some(permissions) = existing_permissions {
        temporary.as_file().set_permissions(permissions)?;
    } else {
        set_private_permissions(temporary.as_file())?;
    }

    before_final_validation();
    verify_expected_state(path, expected)?;
    let target_existed = matches!(expected, ExpectedFileState::Sha256(_));
    persist_temporary(temporary, path, target_existed)?;
    sync_parent(path)?;
    Ok(())
}

#[cfg(not(windows))]
fn persist_temporary(temporary: NamedTempFile, path: &Path, _target_existed: bool) -> Result<()> {
    temporary.persist(path)?;
    Ok(())
}

#[cfg(windows)]
fn persist_temporary(temporary: NamedTempFile, path: &Path, target_existed: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
    use windows::Win32::Storage::FileSystem::MoveFileExW;
    use windows::Win32::Storage::FileSystem::REPLACE_FILE_FLAGS;
    use windows::Win32::Storage::FileSystem::ReplaceFileW;
    use windows::core::PCWSTR;

    let temporary_path = temporary
        .into_temp_path()
        .keep()
        .map_err(|error| error.error)?;
    let temporary_wide = temporary_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target_wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = if target_existed {
        unsafe {
            ReplaceFileW(
                PCWSTR(target_wide.as_ptr()),
                PCWSTR(temporary_wide.as_ptr()),
                PCWSTR::null(),
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
        }
    } else {
        unsafe {
            MoveFileExW(
                PCWSTR(temporary_wide.as_ptr()),
                PCWSTR(target_wide.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
        return Err(SwitcherError::AtomicReplace(
            "Windows could not preserve the target security metadata".to_string(),
        ));
    }
    Ok(())
}

fn verify_expected_state(path: &Path, expected: &ExpectedFileState) -> Result<()> {
    let actual = read_optional(path)?;
    let matches = match expected {
        ExpectedFileState::Missing => actual.is_none(),
        ExpectedFileState::Sha256(expected_hash) => actual
            .as_deref()
            .is_some_and(|contents| sha256_hex(contents) == *expected_hash),
    };
    if !matches {
        return Err(SwitcherError::Conflict(format!(
            "{} changed immediately before replacement",
            path.display()
        )));
    }
    Ok(())
}

fn remove_file_expected(path: &Path, expected: &ExpectedFileState) -> Result<()> {
    verify_expected_state(path, expected)?;
    fs::remove_file(path)?;
    sync_parent(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_private_transaction_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::PermissionsExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    sync_parent(path)?;
    Ok(())
}

#[cfg(windows)]
fn create_private_transaction_dir(path: &Path) -> Result<()> {
    windows_private_directory::create(path)
}

#[cfg(not(any(unix, windows)))]
fn create_private_transaction_dir(_path: &Path) -> Result<()> {
    Err(SwitcherError::Validation(
        "private transaction directories are unsupported on this platform".to_string(),
    ))
}

#[cfg(windows)]
mod windows_private_directory {
    use std::ffi::c_void;
    use std::mem;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::Foundation::GENERIC_ALL;
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Foundation::HLOCAL;
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Foundation::WIN32_ERROR;
    use windows::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
    use windows::Win32::Security::Authorization::NO_MULTIPLE_TRUSTEE;
    use windows::Win32::Security::Authorization::SET_ACCESS;
    use windows::Win32::Security::Authorization::SetEntriesInAclW;
    use windows::Win32::Security::Authorization::TRUSTEE_IS_SID;
    use windows::Win32::Security::Authorization::TRUSTEE_IS_USER;
    use windows::Win32::Security::Authorization::TRUSTEE_W;
    use windows::Win32::Security::GetTokenInformation;
    use windows::Win32::Security::InitializeSecurityDescriptor;
    use windows::Win32::Security::PSECURITY_DESCRIPTOR;
    use windows::Win32::Security::SE_DACL_PROTECTED;
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::Security::SECURITY_DESCRIPTOR;
    use windows::Win32::Security::SUB_CONTAINERS_AND_OBJECTS_INHERIT;
    use windows::Win32::Security::SetSecurityDescriptorControl;
    use windows::Win32::Security::SetSecurityDescriptorDacl;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::Security::TOKEN_USER;
    use windows::Win32::Security::TokenUser;
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
    use windows::Win32::System::Threading::GetCurrentProcess;
    use windows::Win32::System::Threading::OpenProcessToken;
    use windows::core::PWSTR;

    use crate::error::Result;
    use crate::error::SwitcherError;

    pub fn create(path: &Path) -> Result<()> {
        // The DACL is supplied to CreateDirectoryW, so there is no interval in
        // which the transaction directory inherits a broader parent ACL.
        unsafe { create_with_current_user_dacl(path) }
    }

    unsafe fn create_with_current_user_dacl(path: &Path) -> Result<()> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .map_err(windows_error)?;

        let result = unsafe { create_with_token(path, token) };
        let close_result = unsafe { CloseHandle(token) }.map_err(windows_error);
        result?;
        close_result
    }

    unsafe fn create_with_token(path: &Path, token: HANDLE) -> Result<()> {
        let mut required = 0;
        let first = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut required) };
        if first.is_ok() || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(SwitcherError::Validation(
                "failed to size the current Windows user token".to_string(),
            ));
        }

        let word_size = mem::size_of::<usize>();
        let mut token_buffer = vec![0usize; (required as usize).div_ceil(word_size)];
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(token_buffer.as_mut_ptr().cast::<c_void>()),
                required,
                &mut required,
            )
        }
        .map_err(windows_error)?;
        let token_user = unsafe { &*token_buffer.as_ptr().cast::<TOKEN_USER>() };

        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL.0,
            grfAccessMode: SET_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: PWSTR(token_user.User.Sid.0.cast()),
            },
        };
        let mut acl = ptr::null_mut();
        let acl_status = unsafe { SetEntriesInAclW(Some(&[access]), None, &mut acl) };
        if acl_status != ERROR_SUCCESS {
            return Err(win32_status(
                "failed to build a private Windows DACL",
                acl_status,
            ));
        }
        let result = unsafe { create_with_acl(path, acl) };
        unsafe {
            LocalFree(Some(HLOCAL(acl.cast())));
        }
        result
    }

    unsafe fn create_with_acl(path: &Path, acl: *mut windows::Win32::Security::ACL) -> Result<()> {
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let descriptor_pointer =
            PSECURITY_DESCRIPTOR((&mut descriptor as *mut SECURITY_DESCRIPTOR).cast());
        unsafe { InitializeSecurityDescriptor(descriptor_pointer, SECURITY_DESCRIPTOR_REVISION) }
            .map_err(windows_error)?;
        unsafe { SetSecurityDescriptorDacl(descriptor_pointer, true, Some(acl), false) }
            .map_err(windows_error)?;
        unsafe {
            SetSecurityDescriptorControl(descriptor_pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
        }
        .map_err(windows_error)?;

        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast(),
            bInheritHandle: false.into(),
        };
        let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe { CreateDirectoryW(windows::core::PCWSTR(wide_path.as_ptr()), Some(&attributes)) }
            .map_err(windows_error)
    }

    fn windows_error(error: windows::core::Error) -> SwitcherError {
        SwitcherError::Validation(format!(
            "failed to create a private Windows transaction directory: {error}"
        ))
    }

    fn win32_status(context: &str, status: WIN32_ERROR) -> SwitcherError {
        SwitcherError::Validation(format!("{context}: Win32 error {}", status.0))
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    File::open(parent(path)?)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    ensure_regular_or_missing(path)?;
    if !path.exists() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn ensure_regular_or_missing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(SwitcherError::SymbolicLink(path.to_path_buf()))
        }
        Ok(metadata) if !metadata.is_file() => {
            Err(SwitcherError::NotRegularFile(path.to_path_buf()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn ensure_regular_file(path: &Path) -> Result<()> {
    ensure_regular_or_missing(path)?;
    if !path.is_file() {
        return Err(SwitcherError::NotRegularFile(path.to_path_buf()));
    }
    Ok(())
}

fn lock_path_for(config_path: &Path) -> PathBuf {
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    config_path.with_file_name(format!("{file_name}.provider-switcher.lock"))
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| SwitcherError::Validation(format!("path has no parent: {}", path.display())))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::domain::ModelSpec;
    use crate::domain::ProviderProfile;
    use crate::domain::ReasoningEffort;
    use crate::plan_config;
    use crate::plan_proxy_config;

    use super::*;

    fn test_profile() -> ProviderProfile {
        ProviderProfile {
            id: "acme".into(),
            display_name: "Acme".into(),
            base_url: "https://api.acme.test/v1".into(),
            supports_websockets: false,
            credential_required: true,
            models: vec![ModelSpec {
                id: "acme-code".into(),
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

    #[test]
    fn apply_and_restore_are_exact() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("codex/config.toml");
        let catalog_path = root.path().join("codex/switcher/models.json");
        let backup_root = root.path().join("backups");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let original = b"# existing\r\napproval_policy = \"on-request\"\r\n";
        fs::write(&config_path, original).unwrap();

        let plan = plan_config(
            std::str::from_utf8(original).unwrap(),
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &backup_root, &plan).unwrap();
        assert!(
            fs::read_to_string(&config_path)
                .unwrap()
                .contains("model_provider")
        );
        assert!(catalog_path.exists());

        restore_backup(&applied.manifest_path, &config_path, &catalog_path).unwrap();
        assert_eq!(fs::read(&config_path).unwrap(), original);
        assert!(!catalog_path.exists());
    }

    #[test]
    fn apply_uses_the_preallocated_transaction_id() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("codex/config.toml");
        let catalog_path = root.path().join("codex/switcher/models.json");
        let backup_root = root.path().join("backups");
        let transaction_id = Uuid::new_v4();
        let plan = plan_config(
            "",
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();

        let applied = apply_config_plan_with_transaction_id(
            &config_path,
            &backup_root,
            &plan,
            transaction_id,
        )
        .unwrap();

        assert_eq!(applied.transaction_id, transaction_id);
        assert_eq!(
            applied.manifest_path,
            backup_root
                .join(transaction_id.to_string())
                .join("manifest.json")
        );
        assert!(
            apply_config_plan_with_transaction_id(&config_path, &backup_root, &plan, Uuid::nil(),)
                .is_err()
        );
    }

    #[test]
    fn apply_detects_racing_config_edit() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        fs::write(&config_path, "model = \"before\"\n").unwrap();
        let plan = plan_config(
            "model = \"before\"\n",
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        fs::write(&config_path, "model = \"user-edit\"\n").unwrap();

        assert!(apply_config_plan(&config_path, &root.path().join("backups"), &plan).is_err());
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "model = \"user-edit\"\n"
        );
    }

    #[test]
    fn replacement_rechecks_after_temporary_file_is_ready() {
        let root = tempdir().unwrap();
        let path = root.path().join("config.toml");
        let before = b"model = \"before\"\n";
        let racer = b"model = \"racer\"\n";
        fs::write(&path, before).unwrap();

        let result = atomic_write_expected_with_hook(
            &path,
            &ExpectedFileState::Sha256(sha256_hex(before)),
            b"model = \"switcher\"\n",
            || fs::write(&path, racer).unwrap(),
        );

        assert!(matches!(result, Err(SwitcherError::Conflict(_))));
        assert_eq!(fs::read(&path).unwrap(), racer);
    }

    #[test]
    fn restore_detects_post_apply_edit() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        fs::write(&config_path, "").unwrap();
        let plan = plan_config(
            "",
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        fs::write(&config_path, "model = \"user-edit\"\n").unwrap();

        assert!(restore_backup(&applied.manifest_path, &config_path, &catalog_path).is_err());
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "model = \"user-edit\"\n"
        );
    }

    #[test]
    fn proxy_detach_restores_owned_fields_and_preserves_unrelated_edits() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        let original = r#"# user config
model_provider = "openai"
model = "official"
approval_policy = "on-request"

[model_providers.keep]
name = "Keep"
base_url = "https://keep.example/v1"
wire_api = "responses"
"#;
        fs::write(&config_path, original).unwrap();
        let plan = plan_proxy_config(
            original,
            &test_profile(),
            "acme-code",
            &catalog_path,
            &root.path().join("switcher"),
            "http://127.0.0.1:15722/v1",
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let edited = fs::read_to_string(&config_path).unwrap().replace(
            "approval_policy = \"on-request\"",
            "approval_policy = \"never\"\nnew_user_setting = true",
        );
        fs::write(&config_path, edited).unwrap();

        assert!(restore_backup(&applied.manifest_path, &config_path, &catalog_path).is_err());
        let detached = restore_proxy_config_preserving_unrelated_changes(
            &applied.manifest_path,
            &config_path,
            &catalog_path,
            "http://127.0.0.1:15722/v1",
        )
        .unwrap();

        assert!(detached.manifest_finalized);
        let restored = fs::read_to_string(&config_path).unwrap();
        assert!(restored.contains("model_provider = \"openai\""));
        assert!(restored.contains("model = \"official\""));
        assert!(restored.contains("approval_policy = \"never\""));
        assert!(restored.contains("new_user_setting = true"));
        assert!(restored.contains("[model_providers.keep]"));
        assert!(!restored.contains("model_providers.cps-local"));
        assert!(catalog_path.exists());
        let manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.status, BackupStatus::Restored);
    }

    #[test]
    fn proxy_detach_refuses_a_change_to_switcher_owned_fields() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        let original = "model_provider = \"openai\"\nmodel = \"official\"\n";
        fs::write(&config_path, original).unwrap();
        let plan = plan_proxy_config(
            original,
            &test_profile(),
            "acme-code",
            &catalog_path,
            &root.path().join("switcher"),
            "http://127.0.0.1:15722/v1",
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let edited = fs::read_to_string(&config_path)
            .unwrap()
            .replace("model = \"acme-code\"", "model = \"user-edit\"");
        fs::write(&config_path, &edited).unwrap();

        assert!(
            restore_proxy_config_preserving_unrelated_changes(
                &applied.manifest_path,
                &config_path,
                &catalog_path,
                "http://127.0.0.1:15722/v1",
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&config_path).unwrap(), edited);
        let manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.status, BackupStatus::Applied);
    }

    #[test]
    fn proxy_detach_reentry_refuses_an_edit_after_the_journal_was_written() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        let original = "model_provider = \"openai\"\nmodel = \"official\"\n";
        fs::write(&config_path, original).unwrap();
        let plan = plan_proxy_config(
            original,
            &test_profile(),
            "acme-code",
            &catalog_path,
            &root.path().join("switcher"),
            "http://127.0.0.1:15722/v1",
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let before = fs::read(&config_path).unwrap();
        let mut manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        let original_backup = read_verified_backup(&manifest.config).unwrap();
        let after = merge_proxy_owned_config(&before, original_backup.as_deref().unwrap()).unwrap();
        manifest.status = BackupStatus::Detaching;
        manifest.proxy_detach = Some(ProxyDetachJournal {
            before_sha256: sha256_hex(&before),
            after_existed: true,
            after_sha256: Some(sha256_hex(after.as_bytes())),
        });
        write_manifest(&applied.manifest_path, &manifest).unwrap();

        let user_edit = String::from_utf8(before)
            .unwrap()
            .replace("model = \"acme-code\"", "model = \"user\"");
        fs::write(&config_path, &user_edit).unwrap();

        assert!(
            verify_proxy_detach_recoverable(&applied.manifest_path, &config_path, &catalog_path,)
                .is_err()
        );
        assert!(
            restore_proxy_config_preserving_unrelated_changes(
                &applied.manifest_path,
                &config_path,
                &catalog_path,
                "http://127.0.0.1:15722/v1",
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&config_path).unwrap(), user_edit);
    }

    #[test]
    fn backup_integrity_check_detects_a_missing_original_before_restore() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        let original = "model_provider = \"openai\"\nmodel = \"official\"\n";
        fs::write(&config_path, original).unwrap();
        fs::write(&catalog_path, "{\"models\":[]}").unwrap();
        let plan = plan_proxy_config(
            original,
            &test_profile(),
            "acme-code",
            &catalog_path,
            &root.path().join("switcher"),
            "http://127.0.0.1:15722/v1",
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        fs::remove_file(manifest.config.backup_file.unwrap()).unwrap();

        assert!(
            verify_backup_integrity(&applied.manifest_path, &config_path, &catalog_path).is_err()
        );
    }

    #[test]
    fn restore_rejects_a_tampered_backup_before_changing_targets() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        let original = b"model = \"official\"\n";
        fs::write(&config_path, original).unwrap();
        let plan = plan_config(
            std::str::from_utf8(original).unwrap(),
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let applied_config = fs::read(&config_path).unwrap();
        let applied_catalog = fs::read(&catalog_path).unwrap();
        let manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        fs::write(
            manifest.config.backup_file.as_ref().unwrap(),
            b"tampered backup",
        )
        .unwrap();

        assert!(restore_backup(&applied.manifest_path, &config_path, &catalog_path).is_err());
        assert_eq!(fs::read(&config_path).unwrap(), applied_config);
        assert_eq!(fs::read(&catalog_path).unwrap(), applied_catalog);
        let manifest_after: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest_after.status, BackupStatus::Applied);
    }

    #[cfg(unix)]
    #[test]
    fn transaction_directory_and_backup_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        fs::write(&config_path, "model = \"official\"\n").unwrap();
        let plan = plan_config(
            "model = \"official\"\n",
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        let transaction_dir = applied.manifest_path.parent().unwrap();

        assert_eq!(
            fs::metadata(transaction_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&applied.manifest_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(manifest.config.backup_file.unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_target() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let real = root.path().join("real.toml");
        let linked = root.path().join("config.toml");
        fs::write(&real, "").unwrap();
        symlink(&real, &linked).unwrap();
        let plan = plan_config(
            "",
            &test_profile(),
            "acme-code",
            &root.path().join("models.json"),
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        assert!(apply_config_plan(&linked, &root.path().join("backups"), &plan).is_err());
    }

    #[test]
    fn recovers_a_catalog_only_interrupted_apply() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        let original = b"model = \"official\"\n";
        fs::write(&config_path, original).unwrap();
        let plan = plan_config(
            std::str::from_utf8(original).unwrap(),
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let mut manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        fs::write(&config_path, original).unwrap();
        manifest.status = BackupStatus::Prepared;
        write_manifest(&applied.manifest_path, &manifest).unwrap();

        assert_eq!(
            recover_prepared_backup(&applied.manifest_path, &config_path, &catalog_path).unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(fs::read(&config_path).unwrap(), original);
        assert!(!catalog_path.exists());
    }

    #[test]
    fn restoring_journal_recovers_a_kill_between_target_writes() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        let original = b"model = \"official\"\n";
        fs::write(&config_path, original).unwrap();
        let plan = plan_config(
            std::str::from_utf8(original).unwrap(),
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let mut manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        let config_original = read_verified_backup(&manifest.config).unwrap();
        manifest.status = BackupStatus::Restoring;
        write_manifest(&applied.manifest_path, &manifest).unwrap();
        restore_snapshot_expected(
            &manifest.config,
            KnownSnapshotState::Applied,
            config_original.as_deref(),
        )
        .unwrap();
        assert_eq!(fs::read(&config_path).unwrap(), original);
        assert!(catalog_path.exists());

        assert_eq!(
            recover_prepared_backup(&applied.manifest_path, &config_path, &catalog_path).unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(fs::read(&config_path).unwrap(), original);
        assert!(!catalog_path.exists());
        let manifest_after: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest_after.status, BackupStatus::Restored);
    }

    #[test]
    fn restoring_accepts_an_original_catalog_identical_to_applied() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        let original_config = b"model = \"official\"\n";
        fs::write(&config_path, original_config).unwrap();
        let plan = plan_config(
            std::str::from_utf8(original_config).unwrap(),
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        fs::write(&catalog_path, &plan.rendered_catalog).unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let mut manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest.catalog.original_sha256.as_deref(),
            Some(manifest.catalog.applied_sha256.as_str())
        );
        assert_eq!(
            known_snapshot_state(&manifest.catalog).unwrap(),
            KnownSnapshotState::OriginalAndApplied
        );
        manifest.status = BackupStatus::Restoring;
        write_manifest(&applied.manifest_path, &manifest).unwrap();

        assert_eq!(
            recover_prepared_backup(&applied.manifest_path, &config_path, &catalog_path).unwrap(),
            RecoveryOutcome::RolledBack
        );
        assert_eq!(fs::read(&config_path).unwrap(), original_config);
        assert_eq!(
            fs::read_to_string(&catalog_path).unwrap(),
            plan.rendered_catalog
        );
        let manifest_after: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest_after.status, BackupStatus::Restored);
    }

    #[test]
    fn prepared_transaction_with_both_applied_files_is_finalized() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        fs::write(&config_path, "").unwrap();
        let plan = plan_config(
            "",
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let mut manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        manifest.status = BackupStatus::Prepared;
        write_manifest(&applied.manifest_path, &manifest).unwrap();

        assert_eq!(
            recover_prepared_backup(&applied.manifest_path, &config_path, &catalog_path).unwrap(),
            RecoveryOutcome::FinalizedApplied
        );
        let manifest_after: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest_after.status, BackupStatus::Applied);
    }

    #[test]
    fn scoped_restore_rejects_a_manifest_target_change() {
        let root = tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let catalog_path = root.path().join("models.json");
        fs::write(&config_path, "").unwrap();
        let plan = plan_config(
            "",
            &test_profile(),
            "acme-code",
            &catalog_path,
            Some(&root.path().join("switcher")),
        )
        .unwrap();
        let applied = apply_config_plan(&config_path, &root.path().join("backups"), &plan).unwrap();
        let mut manifest: BackupManifest =
            serde_json::from_slice(&fs::read(&applied.manifest_path).unwrap()).unwrap();
        manifest.config.path = root.path().join("unrelated.toml");
        write_manifest(&applied.manifest_path, &manifest).unwrap();

        assert!(restore_backup(&applied.manifest_path, &config_path, &catalog_path).is_err());
    }
}
