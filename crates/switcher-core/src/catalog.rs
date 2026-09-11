use serde_json::Value;
use serde_json::json;

use crate::domain::ProviderProfile;
use crate::error::Result;
use crate::validation::validate_profile;

pub fn render_model_catalog(profile: &ProviderProfile) -> Result<String> {
    validate_profile(profile)?;

    let models = profile
        .models
        .iter()
        .enumerate()
        .map(|(index, model)| {
            let reasoning_levels: Vec<Value> = model
                .reasoning_levels
                .iter()
                .map(|effort| {
                    json!({
                        "effort": effort,
                        "description": effort.description(),
                    })
                })
                .collect();
            let input_modalities = if model.supports_images {
                json!(["text", "image"])
            } else {
                json!(["text"])
            };

            json!({
                "slug": model.id,
                "display_name": model.display_name,
                "description": model.description,
                "default_reasoning_level": model.default_reasoning,
                "supported_reasoning_levels": reasoning_levels,
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": index as i32 + 1,
                "availability_nux": null,
                "upgrade": null,
                "base_instructions": "You are a coding agent working with the user in the current repository. Follow developer and user instructions, inspect relevant context before editing, keep changes scoped, use the available tools carefully, and verify completed work.",
                "include_skills_usage_instructions": true,
                "supports_reasoning_summary_parameter": false,
                "support_verbosity": false,
                "default_verbosity": null,
                "apply_patch_tool_type": "freeform",
                "web_search_tool_type": "text",
                "truncation_policy": {
                    "mode": "tokens",
                    "limit": 10_000
                },
                "supports_parallel_tool_calls": model.supports_parallel_tool_calls,
                "supports_image_detail_original": model.supports_images,
                "context_window": model.context_window,
                "max_context_window": model.context_window,
                "auto_compact_token_limit": null,
                "experimental_supported_tools": [],
                "input_modalities": input_modalities,
                "supports_search_tool": false,
                "use_responses_lite": false,
                "auto_review_model_override": null
            })
        })
        .collect::<Vec<_>>();

    let mut rendered = serde_json::to_string_pretty(&json!({ "models": models }))?;
    rendered.push('\n');
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use crate::domain::ModelSpec;
    use crate::domain::ReasoningEffort;

    use super::*;

    #[test]
    fn emits_codex_models_wrapper_without_credentials() {
        let profile = ProviderProfile {
            id: "acme".into(),
            display_name: "Acme".into(),
            base_url: "https://api.acme.test/v1".into(),
            supports_websockets: false,
            credential_required: true,
            models: vec![ModelSpec {
                id: "acme-code".into(),
                display_name: "Acme Code".into(),
                description: "Coding model".into(),
                context_window: 64_000,
                default_reasoning: ReasoningEffort::Low,
                reasoning_levels: vec![ReasoningEffort::Low],
                supports_parallel_tool_calls: true,
                supports_images: false,
            }],
        };

        let rendered = render_model_catalog(&profile).unwrap();
        let parsed: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["models"][0]["slug"], "acme-code");
        assert_eq!(parsed["models"][0]["input_modalities"], json!(["text"]));
        assert_eq!(parsed["models"][0]["supports_image_detail_original"], false);
        assert!(!rendered.to_ascii_lowercase().contains("api_key"));
    }

    #[test]
    fn image_capable_models_advertise_image_modalities() {
        let profile = ProviderProfile {
            id: "vision".into(),
            display_name: "Vision".into(),
            base_url: "https://api.vision.test/v1".into(),
            supports_websockets: false,
            credential_required: true,
            models: vec![ModelSpec {
                id: "vision-code".into(),
                display_name: "Vision Code".into(),
                description: "Vision model".into(),
                context_window: 64_000,
                default_reasoning: ReasoningEffort::Low,
                reasoning_levels: vec![ReasoningEffort::Low],
                supports_parallel_tool_calls: true,
                supports_images: true,
            }],
        };

        let parsed: Value = serde_json::from_str(&render_model_catalog(&profile).unwrap()).unwrap();
        assert_eq!(parsed["models"][0]["input_modalities"], json!(["text", "image"]));
        assert_eq!(parsed["models"][0]["supports_image_detail_original"], true);
    }
}
