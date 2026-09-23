//! Translacja Anthropic ↔ OpenAI.

pub mod request;
pub mod response;
pub mod stream;

use serde_json::Value;

/// Który z `stop_sequences` klienta zatrzymał generację. Standard OpenAI tego
/// nie podaje (samo `finish_reason: "stop"`); vLLM/NVIDIA NIM i część
/// providerów zwracają trafioną sekwencję w niestandardowym polu `stop_reason`.
/// Liczbę (id tokenu EOS) i sekwencje spoza listy klienta ignorujemy.
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

/// Mapowanie `finish_reason` (OpenAI) → `stop_reason` (Anthropic).
pub fn map_stop_reason(finish_reason: Option<&str>) -> String {
    match finish_reason {
        Some("length") => "max_tokens".to_string(),
        Some("tool_calls") | Some("function_call") => "tool_use".to_string(),
        Some("content_filter") => "end_turn".to_string(),
        _ => "end_turn".to_string(),
    }
}
