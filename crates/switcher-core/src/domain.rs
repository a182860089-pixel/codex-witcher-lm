use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

impl ReasoningEffort {
    pub fn description(&self) -> &'static str {
        match self {
            Self::None => "No additional reasoning",
            Self::Minimal => "Minimal reasoning for fast responses",
            Self::Low => "Fast responses with lighter reasoning",
            Self::Medium => "Balanced speed and reasoning depth",
            Self::High => "Greater reasoning depth for complex work",
            Self::Xhigh => "Extra-high reasoning depth",
            Self::Max => "Maximum reasoning depth",
            Self::Ultra => "Maximum reasoning with agent delegation",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub context_window: u64,
    pub default_reasoning: ReasoningEffort,
    pub reasoning_levels: Vec<ReasoningEffort>,
    #[serde(default = "default_true")]
    pub supports_parallel_tool_calls: bool,
    #[serde(default)]
    pub supports_images: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderProfile {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub models: Vec<ModelSpec>,
    #[serde(default)]
    pub supports_websockets: bool,
    #[serde(default = "default_true")]
    pub credential_required: bool,
}

pub const OFFICIAL_PROFILE_SCHEMA_VERSION: u32 = 2;
pub const OFFICIAL_PROFILE_DISPLAY_NAME: &str = "OpenAI 官方账号";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialProfile {
    pub schema_version: u32,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

impl OfficialProfile {
    pub fn new(display_name: String, model_id: Option<String>) -> Self {
        Self {
            schema_version: OFFICIAL_PROFILE_SCHEMA_VERSION,
            display_name,
            model_id,
        }
    }

    pub fn default_named(model_id: Option<String>) -> Self {
        Self::new(OFFICIAL_PROFILE_DISPLAY_NAME.to_string(), model_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selection {
    pub provider_id: String,
    pub model_id: String,
}

fn default_true() -> bool {
    true
}
