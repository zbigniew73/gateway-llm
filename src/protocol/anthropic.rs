//! Typy Anthropic Messages API (`/v1/messages`).
//!
//! Nieznane typy bloków treści (np. `redacted_thinking`, dokumenty, przyszłe rozszerzenia)
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
    /// Konfiguracja extended thinking klienta (`{"type": "enabled", ...}`).
    #[serde(default)]
    pub thinking: Option<Value>,
}

impl MessagesRequest {
    /// Czy klient chce bloków `thinking` w odpowiedzi. Tak jak w API Anthropic:
    /// bez włączonego thinking rozumowanie modelu nie jest pokazywane.
    pub fn thinking_enabled(&self) -> bool {
        self.thinking
            .as_ref()
            .and_then(|thinking| thinking.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "disabled")
    }

    pub fn stop_sequences(&self) -> &[String] {
        self.stop_sequences.as_deref().unwrap_or(&[])
    }
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
    /// Rozumowanie modelu. W odpowiedzi tworzone z `reasoning_content`/`reasoning`
    /// providera; w żądaniu (echo poprzednich tur) ignorowane w translacji.
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    /// Wszystko, czego nie rozpoznajemy (np. `redacted_thinking`) — ignorowane w translacji.
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

// ---------------------------------------------------------------------------
// Szacowanie tokenów (`/v1/messages/count_tokens`)
// ---------------------------------------------------------------------------

/// ~4 bajty UTF-8 na token. Bajty zamiast znaków celowo lekko zawyżają wynik
/// dla tekstu nie-ASCII (np. polskiego) — dla auto-kompaktowania w Claude Code
/// bezpieczniej przeszacować niż niedoszacować.
const BYTES_PER_TOKEN: usize = 4;
/// Obraz liczony ryczałtem — długość base64 nie ma związku z liczbą tokenów.
const IMAGE_TOKENS: usize = 1600;
/// Narzut formatu na każdą wiadomość (rola, separatory).
const MESSAGE_OVERHEAD_TOKENS: usize = 4;

impl MessagesRequest {
    /// PRZYBLIŻONA liczba tokenów wejścia. Backendy OpenAI-wire nie mają
    /// endpointu do liczenia tokenów, więc szacujemy lokalnie. Bloki, których
    /// nie rozpoznajemy (np. `thinking`, dokumenty), nie są liczone.
    pub fn estimate_input_tokens(&self) -> u32 {
        let mut bytes = 0usize;
        let mut tokens = 0usize;

        match &self.system {
            Some(SystemPrompt::Text(text)) => bytes += text.len(),
            Some(SystemPrompt::Blocks(blocks)) => count_blocks(blocks, &mut bytes, &mut tokens),
            None => {}
        }

        for message in &self.messages {
            tokens += MESSAGE_OVERHEAD_TOKENS;
            match &message.content {
                AnthropicContent::Text(text) => bytes += text.len(),
                AnthropicContent::Blocks(blocks) => count_blocks(blocks, &mut bytes, &mut tokens),
            }
        }

        for tool in self.tools.iter().flatten() {
            bytes += tool.name.len();
            bytes += tool.description.as_deref().map_or(0, str::len);
            bytes += tool.input_schema.as_ref().map_or(0, |schema| schema.to_string().len());
        }

        let total = tokens + bytes.div_ceil(BYTES_PER_TOKEN);
        u32::try_from(total).unwrap_or(u32::MAX)
    }
}

fn count_blocks(blocks: &[AnthropicContentBlock], bytes: &mut usize, tokens: &mut usize) {
    for block in blocks {
        match block {
            AnthropicContentBlock::Text { text } => *bytes += text.len(),
            AnthropicContentBlock::Image { .. } => *tokens += IMAGE_TOKENS,
            AnthropicContentBlock::ToolUse { name, input, .. } => {
                *bytes += name.len() + input.to_string().len();
            }
            AnthropicContentBlock::ToolResult { content, .. } => match content {
                Some(ToolResultContent::Text(text)) => *bytes += text.len(),
                Some(ToolResultContent::Blocks(nested)) => count_blocks(nested, bytes, tokens),
                None => {}
            },
            // Rozumowanie z poprzednich tur nie trafia do providera (translacja je
            // pomija), więc nie zajmuje kontekstu.
            AnthropicContentBlock::Thinking { .. } | AnthropicContentBlock::Unknown => {}
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(value: Value) -> MessagesRequest {
        serde_json::from_value(value).expect("poprawne żądanie testowe")
    }

    #[test]
    fn text_is_estimated_at_four_bytes_per_token() {
        // 400 bajtów tekstu = 100 tokenów + 4 narzutu na wiadomość.
        let req = request(json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "a".repeat(400) }]
        }));
        assert_eq!(req.estimate_input_tokens(), 104);
    }

    #[test]
    fn system_prompt_and_tools_are_counted() {
        let bare = request(json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }]
        }));
        let full = request(json!({
            "model": "m",
            "system": "x".repeat(4000),
            "tools": [{ "name": "read_file", "description": "d".repeat(400),
                        "input_schema": { "type": "object", "properties": {} } }],
            "messages": [{ "role": "user", "content": "hi" }]
        }));
        assert!(full.estimate_input_tokens() >= bare.estimate_input_tokens() + 1100);
    }

    #[test]
    fn image_counts_as_fixed_amount_not_base64_length() {
        let req = request(json!({
            "model": "m",
            "messages": [{ "role": "user", "content": [
                { "type": "image", "source": { "type": "base64", "media_type": "image/png",
                                               "data": "A".repeat(1_000_000) } }
            ]}]
        }));
        assert_eq!(req.estimate_input_tokens(), (IMAGE_TOKENS + MESSAGE_OVERHEAD_TOKENS) as u32);
    }

    #[test]
    fn tool_use_and_nested_tool_results_are_counted() {
        let req = request(json!({
            "model": "m",
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "t1", "name": "read", "input": { "path": "a".repeat(396) } }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": [
                        { "type": "text", "text": "b".repeat(800) }
                    ]}
                ]}
            ]
        }));
        // tool_use: nazwa + JSON inputu (> 400 bajtów), tool_result: 800 bajtów.
        assert!(req.estimate_input_tokens() >= 300);
    }
}
