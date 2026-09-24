use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

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
    #[serde(default)]
    pub error: Option<Value>,
}

impl ChatCompletionResponse {
    pub fn error_message(&self) -> Option<String> {
        provider_error_message(
            self.error.as_ref(),
            self.choices
                .iter()
                .any(|choice| choice.finish_reason.as_deref() == Some("error")),
        )
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatChoice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub message: ChatMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
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
    #[serde(default)]
    pub error: Option<Value>,
}

impl ChatCompletionChunk {
    pub fn error_message(&self) -> Option<String> {
        provider_error_message(
            self.error.as_ref(),
            self.choices
                .iter()
                .any(|choice| choice.finish_reason.as_deref() == Some("error")),
        )
    }
}

pub fn provider_error_code(error: Option<&Value>) -> Option<u16> {
    let code = error?.get("code")?;
    code.as_u64()
        .and_then(|code| u16::try_from(code).ok())
        .or_else(|| code.as_str().and_then(|code| code.parse().ok()))
        .filter(|code| (400..=599).contains(code))
}

fn provider_error_message(error: Option<&Value>, finished_with_error: bool) -> Option<String> {
    if let Some(error) = error.filter(|error| !error.is_null()) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .filter(|message| !message.is_empty())
            .unwrap_or("nieznany błąd");
        let code = error.get("code").filter(|code| !code.is_null());
        return Some(match code {
            Some(Value::String(code)) => format!("{message} (kod {code})"),
            Some(code) => format!("{message} (kod {code})"),
            None => message.to_string(),
        });
    }
    finished_with_error
        .then(|| "provider zakończył odpowiedź błędem (finish_reason: error)".to_string())
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChunkChoice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub delta: ChunkDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(value: Value) -> ChatCompletionChunk {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn openrouter_mid_stream_error_is_detected() {
        let chunk = chunk(json!({
            "id": "gen-abc123",
            "object": "chat.completion.chunk",
            "error": { "code": 429, "message": "Rate limit exceeded" },
            "choices": [{ "index": 0, "delta": { "content": "" }, "finish_reason": "error" }]
        }));
        assert_eq!(
            chunk.error_message().as_deref(),
            Some("Rate limit exceeded (kod 429)")
        );
        assert_eq!(provider_error_code(chunk.error.as_ref()), Some(429));
    }

    #[test]
    fn finish_reason_error_without_details_is_detected() {
        let chunk = chunk(json!({
            "choices": [{ "delta": {}, "finish_reason": "error" }]
        }));
        assert!(chunk
            .error_message()
            .unwrap()
            .contains("finish_reason: error"));
    }

    #[test]
    fn normal_chunks_carry_no_error() {
        assert!(
            chunk(json!({ "choices": [{ "delta": { "content": "hi" } }] }))
                .error_message()
                .is_none()
        );
        assert!(
            chunk(json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] }))
                .error_message()
                .is_none()
        );
        assert!(chunk(json!({ "error": null, "choices": [] }))
            .error_message()
            .is_none());
    }

    #[test]
    fn non_stream_error_body_is_detected() {
        let response: ChatCompletionResponse = serde_json::from_value(json!({
            "error": { "code": "502", "message": "upstream down" },
            "choices": []
        }))
        .unwrap();
        assert_eq!(
            response.error_message().as_deref(),
            Some("upstream down (kod 502)")
        );
        assert_eq!(provider_error_code(response.error.as_ref()), Some(502));
    }

    #[test]
    fn error_code_outside_http_error_range_is_ignored() {
        assert_eq!(provider_error_code(Some(&json!({ "code": 200 }))), None);
        assert_eq!(
            provider_error_code(Some(&json!({ "code": "rate_limit" }))),
            None
        );
        assert_eq!(provider_error_code(None), None);
    }
}
