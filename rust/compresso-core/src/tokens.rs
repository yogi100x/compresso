use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenProvider {
    #[default]
    #[serde(rename = "openai_o200k")]
    OpenAiO200k,
    #[serde(rename = "openai_cl100k")]
    OpenAiCl100k,
    AnthropicCalibrated,
    Heuristic,
}

pub trait TokenCounter: Send + Sync {
    fn provider(&self) -> TokenProvider;
    fn count(&self, text: &str) -> usize;
}
