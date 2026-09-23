use serde_json::Value;

use crate::protocol::anthropic::{
    new_message_id, AnthropicContentBlock, AnthropicUsage, MessagesResponse,
};
use crate::protocol::openai::{reasoning_str, ChatCompletionResponse};
use crate::protocol::translate::{map_stop_reason, matched_stop_sequence};

pub fn openai_to_anthropic_response(
    response: &ChatCompletionResponse,
    model_alias: &str,
    thinking_enabled: bool,
    stop_sequences: &[String],
) -> MessagesResponse {
    let choice = response.choices.first();

    let mut content: Vec<AnthropicContentBlock> = Vec::new();
    let mut finish_reason: Option<&str> = None;
    let mut stop_sequence: Option<String> = None;

    if let Some(choice) = choice {
        finish_reason = choice.finish_reason.as_deref();
        stop_sequence =
            matched_stop_sequence(finish_reason, choice.stop_reason.as_ref(), stop_sequences);

        if thinking_enabled {
            if let Some(reasoning) =
                reasoning_str(&choice.message.reasoning_content, &choice.message.reasoning)
            {
                content.push(AnthropicContentBlock::Thinking {
                    thinking: reasoning.to_string(),
                    signature: String::new(),
                });
            }
        }

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

    let has_answer = content.iter().any(|block| {
        matches!(
            block,
            AnthropicContentBlock::Text { .. } | AnthropicContentBlock::ToolUse { .. }
        )
    });
    if !has_answer {
        content.push(AnthropicContentBlock::Text {
            text: String::new(),
        });
    }

    let has_tool_use = content
        .iter()
        .any(|block| matches!(block, AnthropicContentBlock::ToolUse { .. }));

    let stop_reason = if has_tool_use {
        stop_sequence = None;
        "tool_use".to_string()
    } else if stop_sequence.is_some() {
        "stop_sequence".to_string()
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
        stop_sequence,
        usage: AnthropicUsage {
            input_tokens: usage.prompt_tokens.unwrap_or(0),
            output_tokens: usage.completion_tokens.unwrap_or(0),
        },
    }
}

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
    use super::{openai_to_anthropic_response, parse_arguments};
    use crate::protocol::anthropic::AnthropicContentBlock;
    use crate::protocol::openai::ChatCompletionResponse;
    use serde_json::json;

    fn response(value: serde_json::Value) -> ChatCompletionResponse {
        serde_json::from_value(value).expect("poprawna odpowiedź testowa")
    }

    fn with_reasoning() -> ChatCompletionResponse {
        response(json!({
            "choices": [{ "index": 0, "finish_reason": "stop",
                "message": { "role": "assistant", "content": "Wynik: 4",
                             "reasoning_content": "2+2 to 4" } }]
        }))
    }

    #[test]
    fn reasoning_becomes_thinking_block_only_when_enabled() {
        let enabled = openai_to_anthropic_response(&with_reasoning(), "m", true, &[]);
        assert!(matches!(
            &enabled.content[0],
            AnthropicContentBlock::Thinking { thinking, .. } if thinking == "2+2 to 4"
        ));
        assert!(
            matches!(&enabled.content[1], AnthropicContentBlock::Text { text } if text == "Wynik: 4")
        );

        let disabled = openai_to_anthropic_response(&with_reasoning(), "m", false, &[]);
        assert_eq!(disabled.content.len(), 1);
        assert!(matches!(
            &disabled.content[0],
            AnthropicContentBlock::Text { .. }
        ));
    }

    #[test]
    fn openrouter_reasoning_field_is_supported() {
        let resp = response(json!({
            "choices": [{ "index": 0, "finish_reason": "stop",
                "message": { "role": "assistant", "content": "ok", "reasoning": "myślę" } }]
        }));
        let out = openai_to_anthropic_response(&resp, "m", true, &[]);
        assert!(
            matches!(&out.content[0], AnthropicContentBlock::Thinking { thinking, .. } if thinking == "myślę")
        );
    }

    #[test]
    fn non_string_reasoning_does_not_break_parsing() {
        let resp = response(json!({
            "choices": [{ "index": 0, "finish_reason": "stop",
                "message": { "role": "assistant", "content": "ok", "reasoning": { "effort": "high" } } }]
        }));
        let out = openai_to_anthropic_response(&resp, "m", true, &[]);
        assert_eq!(out.content.len(), 1);
    }

    #[test]
    fn stop_sequence_is_reported_when_provider_names_it() {
        let resp = response(json!({
            "choices": [{ "index": 0, "finish_reason": "stop", "stop_reason": "###",
                "message": { "role": "assistant", "content": "abc" } }]
        }));
        let out = openai_to_anthropic_response(&resp, "m", false, &["###".to_string()]);
        assert_eq!(out.stop_reason.as_deref(), Some("stop_sequence"));
        assert_eq!(out.stop_sequence.as_deref(), Some("###"));
    }

    #[test]
    fn eos_token_id_or_foreign_stop_is_plain_end_turn() {
        for stop_reason in [json!(151645), json!("<|im_end|>")] {
            let resp = response(json!({
                "choices": [{ "index": 0, "finish_reason": "stop", "stop_reason": stop_reason,
                    "message": { "role": "assistant", "content": "abc" } }]
            }));
            let out = openai_to_anthropic_response(&resp, "m", false, &["###".to_string()]);
            assert_eq!(out.stop_reason.as_deref(), Some("end_turn"));
            assert_eq!(out.stop_sequence, None);
        }
    }

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
