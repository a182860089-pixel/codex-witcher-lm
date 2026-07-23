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
pub use config::{ConfigPlan, credential_account_for, plan_config, verify_credential_binding};
pub use discovery::{
    FetchedModel, ModelDiscovery, fetch_models, model_endpoint_candidates, normalize_api_base_url,
};
pub use domain::{ModelSpec, ProviderProfile, ReasoningEffort, Selection};
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
    ApplyResult, BackupManifest, BackupStatus, RecoveryOutcome, RestoreResult, apply_config_plan,
    backup_matches_applied, create_private_directory, recover_prepared_backup, restore_backup,
    write_private_file,
};
pub use validation::validate_profile;
