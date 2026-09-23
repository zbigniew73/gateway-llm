//! Typy OpenAI Chat Completions — format pośredni translacji dla `/v1/messages`.
//!
//! Wszystkie pola odpowiedzi są maksymalnie tolerancyjne (`Option` + `#[serde(default)]`),
//! bo cztery różne backendy potrafią pomijać albo dokładać pola.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAiTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAiContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Rozszerzenie providerów (vLLM/NIM, DeepSeek, Qwen, GLM): tekst rozumowania.
    /// `Value`, bo część providerów wysyła tu nie-string — nie może to wywrócić
    /// parsowania całej odpowiedzi.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<Value>,
    /// Rozszerzenie OpenRouter: tekst rozumowania.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
}

/// Tekst rozumowania z `reasoning_content` albo `reasoning` (pierwszy niepusty string).
pub fn reasoning_str<'a>(
    reasoning_content: &'a Option<Value>,
    reasoning: &'a Option<Value>,
) -> Option<&'a str> {
    [reasoning_content, reasoning]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|text| !text.is_empty())
}

impl ChatMessage {
    pub fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: Some(OpenAiContent::Text(text.into())),
            ..Default::default()
        }
    }

    /// Zwraca treść tekstową wiadomości (łączy części typu `text`).
    pub fn text_content(&self) -> String {
        match &self.content {
            Some(OpenAiContent::Text(text)) => text.clone(),
            Some(OpenAiContent::Parts(parts)) => parts
                .iter()
                .filter_map(|part| match part {
                    OpenAiContentPart::Text { text } => Some(text.as_str()),
                    OpenAiContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            None => String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenAiImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiFunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiFunctionDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
}

// ---------------------------------------------------------------------------
// Tool calls (wspólne dla odpowiedzi i chunków streamingu)
// ---------------------------------------------------------------------------

/// W streamingu wszystkie pola poza `index` przychodzą fragmentarycznie,
/// dlatego każde jest opcjonalne.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAiToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<OpenAiFunctionCall>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAiFunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Argumenty jako STRING z JSON-em (w streamie doklejane fragmentami).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// Response (non-stream)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatCompletionResponse {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<ChatChoice>,
    #[serde(default)]
    pub usage: Option<OpenAiUsage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatChoice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub message: ChatMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
    /// Rozszerzenie vLLM/NIM: trafiona sekwencja stopu (string) albo id tokenu.
    #[serde(default)]
    pub stop_reason: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct OpenAiUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
    #[serde(default)]
    pub total_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Response (streaming)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatCompletionChunk {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub usage: Option<OpenAiUsage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChunkChoice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub delta: ChunkDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
    /// Rozszerzenie vLLM/NIM: trafiona sekwencja stopu (string) albo id tokenu.
    #[serde(default)]
    pub stop_reason: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChunkDelta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(default)]
    pub reasoning_content: Option<Value>,
    #[serde(default)]
    pub reasoning: Option<Value>,
}
