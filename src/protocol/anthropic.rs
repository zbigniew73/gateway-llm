//! Typy Anthropic Messages API (`/v1/messages`).
//!
//! Nieznane typy bloków treści (np. `thinking`, `redacted_thinking`, przyszłe rozszerzenia)
//! lądują w wariancie `Unknown` zamiast wywracać deserializację całego żądania.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub system: Option<SystemPrompt>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(default)]
    pub tool_choice: Option<AnthropicToolChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

impl SystemPrompt {
    pub fn as_text(&self) -> String {
        match self {
            SystemPrompt::Text(text) => text.clone(),
            SystemPrompt::Blocks(blocks) => join_text_blocks(blocks),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

impl AnthropicContent {
    /// Normalizuje treść do listy bloków.
    pub fn blocks(&self) -> Vec<AnthropicContentBlock> {
        match self {
            AnthropicContent::Text(text) => vec![AnthropicContentBlock::Text { text: text.clone() }],
            AnthropicContent::Blocks(blocks) => blocks.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Option<ToolResultContent>,
        #[serde(default)]
        is_error: Option<bool>,
    },
    /// Wszystko, czego nie rozpoznajemy (np. `thinking`) — ignorowane w translacji.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

impl ToolResultContent {
    pub fn as_text(&self) -> String {
        match self {
            ToolResultContent::Text(text) => text.clone(),
            ToolResultContent::Blocks(blocks) => join_text_blocks(blocks),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicToolChoice {
    Auto,
    Any,
    Tool {
        name: String,
    },
    None,
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Response (non-stream)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub role: &'static str,
    pub model: String,
    pub content: Vec<AnthropicContentBlock>,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct AnthropicUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Skleja tekst ze wszystkich bloków typu `text`.
pub fn join_text_blocks(blocks: &[AnthropicContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            AnthropicContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Generuje identyfikator wiadomości w konwencji Anthropic.
pub fn new_message_id() -> String {
    format!("msg_{}", uuid::Uuid::new_v4().simple())
}
