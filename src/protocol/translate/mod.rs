//! Translacja Anthropic ↔ OpenAI.

pub mod request;
pub mod response;
pub mod stream;

/// Mapowanie `finish_reason` (OpenAI) → `stop_reason` (Anthropic).
pub fn map_stop_reason(finish_reason: Option<&str>) -> String {
    match finish_reason {
        Some("length") => "max_tokens".to_string(),
        Some("tool_calls") | Some("function_call") => "tool_use".to_string(),
        Some("content_filter") => "end_turn".to_string(),
        _ => "end_turn".to_string(),
    }
}
