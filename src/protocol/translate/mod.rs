pub mod request;
pub mod response;
pub mod stream;

use serde_json::Value;

pub fn matched_stop_sequence(
    finish_reason: Option<&str>,
    provider_stop_reason: Option<&Value>,
    stop_sequences: &[String],
) -> Option<String> {
    if finish_reason != Some("stop") {
        return None;
    }
    let hit = provider_stop_reason?.as_str()?;
    stop_sequences
        .iter()
        .find(|sequence| sequence.as_str() == hit)
        .cloned()
}

pub fn map_stop_reason(finish_reason: Option<&str>) -> String {
    match finish_reason {
        Some("length") => "max_tokens".to_string(),
        Some("tool_calls") | Some("function_call") => "tool_use".to_string(),
        Some("content_filter") => "end_turn".to_string(),
        _ => "end_turn".to_string(),
    }
}
