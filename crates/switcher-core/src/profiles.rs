use std::collections::HashSet;

use serde::Deserialize;
use serde::Serialize;

use crate::domain::ProviderProfile;
use crate::error::Result;
use crate::error::SwitcherError;
use crate::validation::validate_profile;

const PROFILE_STORE_SCHEMA_VERSION: u32 = 1;
const MAX_SAVED_PROFILES: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileStore {
    pub schema_version: u32,
    pub profiles: Vec<ProviderProfile>,
}

impl Default for ProfileStore {
    fn default() -> Self {
        Self {
            schema_version: PROFILE_STORE_SCHEMA_VERSION,
            profiles: Vec::new(),
        }
    }
}

pub fn parse_profile_store(contents: &str) -> Result<ProfileStore> {
    if contents.trim().is_empty() {
        return Ok(ProfileStore::default());
    }
    let store = serde_json::from_str::<ProfileStore>(contents)?;
    validate_store(&store)?;
    Ok(store)
}

pub fn render_profile_store(store: &ProfileStore) -> Result<Vec<u8>> {
    validate_store(store)?;
    let mut rendered = serde_json::to_vec_pretty(store)?;
    rendered.push(b'\n');
    Ok(rendered)
}

pub fn upsert_profile(store: &mut ProfileStore, profile: ProviderProfile) -> Result<()> {
    validate_profile(&profile)?;
    if let Some(existing) = store
        .profiles
        .iter_mut()
        .find(|existing| existing.id == profile.id)
    {
        *existing = profile;
        return validate_store(store);
    }
    if store.profiles.len() >= MAX_SAVED_PROFILES {
        return Err(SwitcherError::Validation(format!(
            "at most {MAX_SAVED_PROFILES} saved connections are supported"
        )));
    }
    store.profiles.push(profile);
    validate_store(store)
}

pub fn remove_profile(store: &mut ProfileStore, profile_id: &str) -> Result<bool> {
    let original_len = store.profiles.len();
    store.profiles.retain(|profile| profile.id != profile_id);
    validate_store(store)?;
    Ok(store.profiles.len() != original_len)
}

fn validate_store(store: &ProfileStore) -> Result<()> {
    if store.schema_version != PROFILE_STORE_SCHEMA_VERSION {
        return Err(SwitcherError::Validation(format!(
            "unsupported saved connection schema version {}",
            store.schema_version
        )));
    }
    if store.profiles.len() > MAX_SAVED_PROFILES {
        return Err(SwitcherError::Validation(format!(
            "at most {MAX_SAVED_PROFILES} saved connections are supported"
        )));
    }
    let mut ids = HashSet::new();
    for profile in &store.profiles {
        validate_profile(profile)?;
        if !ids.insert(profile.id.as_str()) {
            return Err(SwitcherError::Validation(format!(
                "duplicate saved connection id: {}",
                profile.id
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::domain::ModelSpec;
    use crate::domain::ReasoningEffort;

    use super::*;

    fn profile(id: &str) -> ProviderProfile {
        ProviderProfile {
            id: id.to_string(),
            display_name: format!("{id} API"),
            base_url: format!("https://{id}.example/v1"),
            models: vec![ModelSpec {
                id: format!("{id}/code"),
                display_name: format!("{id}/code"),
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
            }],
            supports_websockets: false,
            credential_required: true,
        }
    }

    #[test]
    fn upsert_preserves_order_and_replaces_matching_profile() {
        let mut store = ProfileStore::default();
        upsert_profile(&mut store, profile("first")).unwrap();
        upsert_profile(&mut store, profile("second")).unwrap();
        let mut replacement = profile("first");
        replacement.display_name = "Updated".to_string();
        upsert_profile(&mut store, replacement).unwrap();

        assert_eq!(store.profiles.len(), 2);
        assert_eq!(store.profiles[0].display_name, "Updated");
        assert_eq!(store.profiles[1].id, "second");
    }

    #[test]
    fn rendered_store_round_trips_without_credentials() {
        let mut store = ProfileStore::default();
        upsert_profile(&mut store, profile("vendor")).unwrap();
        let rendered = render_profile_store(&store).unwrap();
        let text = String::from_utf8(rendered).unwrap();

        assert!(!text.to_ascii_lowercase().contains("api_key"));
        assert_eq!(parse_profile_store(&text).unwrap(), store);
    }

    #[test]
    fn remove_only_changes_the_requested_shortcut() {
        let mut store = ProfileStore::default();
        upsert_profile(&mut store, profile("first")).unwrap();
        upsert_profile(&mut store, profile("second")).unwrap();

        assert!(remove_profile(&mut store, "first").unwrap());
        assert!(!remove_profile(&mut store, "missing").unwrap());
        assert_eq!(store.profiles[0].id, "second");
    }
}
