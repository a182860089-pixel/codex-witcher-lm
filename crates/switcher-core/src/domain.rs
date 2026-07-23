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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selection {
    pub provider_id: String,
    pub model_id: String,
}

fn default_true() -> bool {
    true
}
