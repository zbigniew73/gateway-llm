//! `ChatCompletionResponse` (OpenAI) → `MessagesResponse` (Anthropic), non-stream.

use serde_json::Value;

use crate::protocol::anthropic::{
    new_message_id, AnthropicContentBlock, AnthropicUsage, MessagesResponse,
};
use crate::protocol::openai::ChatCompletionResponse;
use crate::protocol::translate::map_stop_reason;

pub fn openai_to_anthropic_response(
    response: &ChatCompletionResponse,
    model_alias: &str,
) -> MessagesResponse {
    let choice = response.choices.first();

    let mut content: Vec<AnthropicContentBlock> = Vec::new();
    let mut finish_reason: Option<&str> = None;

    if let Some(choice) = choice {
        finish_reason = choice.finish_reason.as_deref();

        let text = choice.message.text_content();
        if !text.is_empty() {
            content.push(AnthropicContentBlock::Text { text });
        }

        if let Some(tool_calls) = &choice.message.tool_calls {
            for (position, tool_call) in tool_calls.iter().enumerate() {
                let function = match &tool_call.function {
                    Some(function) => function,
                    None => continue,
                };
                let name = function.name.clone().unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let id = tool_call
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("toolu_{position}_{}", new_message_id()));
                let input = parse_arguments(function.arguments.as_deref().unwrap_or(""));
                content.push(AnthropicContentBlock::ToolUse { id, name, input });
            }
        }
    }

    // Anthropic wymaga niepustej listy bloków.
    if content.is_empty() {
        content.push(AnthropicContentBlock::Text {
            text: String::new(),
        });
    }

    let has_tool_use = content
        .iter()
        .any(|block| matches!(block, AnthropicContentBlock::ToolUse { .. }));

    let stop_reason = if has_tool_use {
        "tool_use".to_string()
    } else {
        map_stop_reason(finish_reason)
    };

    let usage = response.usage.unwrap_or_default();

    MessagesResponse {
        id: response
            .id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(new_message_id),
        kind: "message",
        role: "assistant",
        model: model_alias.to_string(),
        content,
        stop_reason: Some(stop_reason),
        stop_sequence: None,
        usage: AnthropicUsage {
            input_tokens: usage.prompt_tokens.unwrap_or(0),
            output_tokens: usage.completion_tokens.unwrap_or(0),
        },
    }
}

/// `arguments` przychodzi jako string z JSON-em; gdy jest pusty albo uszkodzony,
/// nie wywracamy odpowiedzi — zwracamy pusty obiekt (albo opakowany surowy tekst).
pub fn parse_arguments(arguments: &str) -> Value {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value @ Value::Object(_)) => value,
        Ok(other) => serde_json::json!({ "value": other }),
        Err(_) => {
            tracing::warn!(
                arguments = %crate::error::truncate(trimmed.to_string(), 200),
                "nie udało się sparsować 'arguments' narzędzia — zwracam pusty obiekt"
            );
            Value::Object(serde_json::Map::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_arguments;
    use serde_json::json;

    #[test]
    fn empty_arguments_become_empty_object() {
        assert_eq!(parse_arguments(""), json!({}));
        assert_eq!(parse_arguments("   "), json!({}));
    }

    #[test]
    fn valid_object_is_preserved() {
        assert_eq!(
            parse_arguments(r#"{"location":"SF"}"#),
            json!({"location": "SF"})
        );
    }

    #[test]
    fn broken_arguments_do_not_panic() {
        assert_eq!(parse_arguments(r#"{"location":"#), json!({}));
    }
}
