mod app_server;
mod catalog;
mod cdp;
mod config;
mod discovery;
mod domain;
mod error;
mod inspection;
mod profiles;
mod supervisor;
mod transaction;
mod validation;

pub use app_server::{TransformOutcome, transform_app_server_request};
pub use catalog::render_model_catalog;
pub use cdp::{
    CdpEndpointKind, CdpTarget, validate_browser_websocket_url, validate_page_target,
    validate_page_target_for_cleanup, validate_page_websocket_url,
};
pub use config::{
    ConfigPlan, LOCAL_PROXY_PROVIDER_ID, LOCAL_PROXY_PROVIDER_NAME, credential_account_for,
    plan_config, plan_official_config, plan_proxy_config, proxy_credential_account_for,
    verify_credential_binding, verify_proxy_config_binding,
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
    ProfileStore, parse_profile_store, remove_profile, render_profile_store, upsert_profile,
};
pub use supervisor::{
    CdpSupervisor, InjectedScript, ListenerIdentityVerifier, ProcessIdentity,
    allocate_loopback_port,
};
pub use transaction::{
    ApplyResult, BackupManifest, BackupStatus, ProxyDetachJournal, RecoveryOutcome, RestoreResult,
    apply_config_plan, apply_config_plan_with_transaction_id, backup_matches_applied,
    create_private_directory, recover_prepared_backup, refresh_proxy_credential_helper_file,
    restore_backup, restore_proxy_config_preserving_unrelated_changes, verify_backup_integrity,
    verify_proxy_detach_recoverable, write_private_file,
};
pub use validation::{validate_official_profile, validate_profile};
