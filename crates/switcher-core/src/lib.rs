mod app_server;
mod catalog;
mod cdp;
mod codex_account;
mod config;
mod discovery;
mod domain;
mod error;
mod inspection;
mod profiles;
mod proxy_bypass;
mod supervisor;
mod transaction;
mod validation;

pub use app_server::{TransformOutcome, transform_app_server_request};
pub use catalog::render_model_catalog;
pub use cdp::{
    CdpEndpointKind, CdpTarget, validate_browser_websocket_url, validate_page_target,
    validate_page_target_for_cleanup, validate_page_websocket_url,
};
pub use codex_account::{
    CodexAccountStatus, CodexAuthMode, CodexJsonLineError, parse_codex_account_result,
    parse_codex_account_updated, read_codex_json_line,
};
pub use config::{
    ConfigPlan, LOCAL_PROXY_PROVIDER_ID, LOCAL_PROXY_PROVIDER_NAME, credential_account_for,
    plan_config, plan_official_config, plan_proxy_config, proxy_credential_account_for,
    retarget_local_proxy_base_url, verify_credential_binding, verify_proxy_config_binding,
};
pub use discovery::{
    FetchedModel, ModelDiscovery, fetch_models, model_endpoint_candidates, normalize_api_base_url,
};
pub use domain::{
    ModelSpec, OFFICIAL_PROFILE_DISPLAY_NAME, OFFICIAL_PROFILE_SCHEMA_VERSION, OfficialProfile,
    ProviderProfile, ReasoningEffort, Selection,
};
pub use error::{Result, SwitcherError};
pub use inspection::{AuthKind, CurrentCodexConfig, inspect_config};
pub use profiles::{
    ProfileStore, parse_profile_store, parse_profile_store_with_migration, remove_profile,
    render_profile_store, upsert_profile,
};
pub use proxy_bypass::{
    LOOPBACK_NO_PROXY_HOSTS, NO_PROXY_MAX_CHARS, SanitizedNoProxy, UserNoProxyPersistPlan,
    loopback_no_proxy_value, merge_no_proxy, no_proxy_covers_loopback, plan_user_no_proxy_persist,
    sanitize_no_proxy, select_no_proxy_source, should_delete_duplicate_no_proxy,
};
pub use supervisor::{
    CdpSupervisor, InjectedScript, ListenerIdentityVerifier, ProcessIdentity,
    allocate_loopback_port,
};
pub use transaction::{
    ApplyResult, BackupManifest, BackupStatus, ProxyDetachJournal, RecoveryOutcome, RestoreResult,
    apply_config_plan, apply_config_plan_with_transaction_id, backup_matches_applied,
    create_private_directory, leftover_backup_requires_manual_review, recover_prepared_backup,
    refresh_proxy_credential_helper_file, refresh_proxy_selected_model_file, restore_backup,
    restore_proxy_config_preserving_unrelated_changes, retarget_local_proxy_base_url_file,
    verify_backup_integrity, verify_proxy_detach_recoverable, write_private_file,
};
pub use validation::{validate_official_profile, validate_profile};
