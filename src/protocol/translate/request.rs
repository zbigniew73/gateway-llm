use serde_json::{json, Value};

use crate::error::AppError;
use crate::protocol::anthropic::{
    AnthropicContentBlock, AnthropicToolChoice, ImageSource, MessagesRequest,
};
use crate::protocol::openai::{
    ChatCompletionRequest, ChatMessage, OpenAiContent, OpenAiContentPart, OpenAiFunctionCall,
    OpenAiFunctionDef, OpenAiImageUrl, OpenAiTool, OpenAiToolCall,
};

const MAX_STOP_SEQUENCES: usize = 4;

pub fn anthropic_to_openai_request(
    request: &MessagesRequest,
) -> Result<ChatCompletionRequest, AppError> {
    if request.model.trim().is_empty() {
        return Err(AppError::bad_request("pole 'model' jest wymagane"));
    }
    if request.messages.is_empty() {
        return Err(AppError::bad_request(
            "pole 'messages' jest wymagane i nie może być puste",
        ));
    }

    let mut messages: Vec<ChatMessage> = Vec::new();

    if let Some(system) = &request.system {
        let system_text = system.as_text();
        if !system_text.trim().is_empty() {
            messages.push(ChatMessage::text("system", system_text));
        }
    }

    for message in &request.messages {
        let blocks = message.content.blocks();
        match message.role.as_str() {
            "assistant" => {
                if let Some(assistant) = build_assistant_message(&blocks) {
                    messages.push(assistant);
                }
            }
            _ => {
                let (tool_messages, user_message) = build_user_messages(&blocks);
                messages.extend(tool_messages);
                if let Some(user_message) = user_message {
                    messages.push(user_message);
                }
            }
        }
    }

    if messages.is_empty() {
        return Err(AppError::bad_request(
            "po translacji nie została żadna wiadomość do wysłania",
        ));
    }

    let tools = request.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|tool| OpenAiTool {
                kind: "function".to_string(),
                function: OpenAiFunctionDef {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool
                        .input_schema
                        .clone()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                },
            })
            .collect::<Vec<_>>()
    });

    let tool_choice = match (&tools, &request.tool_choice) {
        (Some(tools), Some(choice)) if !tools.is_empty() => map_tool_choice(choice),
        _ => None,
    };

    Ok(ChatCompletionRequest {
        model: request.model.clone(),
        messages,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
        top_p: request.top_p,
        stop: upstream_stop_sequences(request.stop_sequences()),
        stream: request.stream,
        tools: tools.filter(|tools| !tools.is_empty()),
        tool_choice,
    })
}

fn upstream_stop_sequences(sequences: &[String]) -> Option<Vec<String>> {
    if sequences.is_empty() {
        return None;
    }
    if sequences.len() > MAX_STOP_SEQUENCES {
        tracing::warn!(
            sent = MAX_STOP_SEQUENCES,
            dropped = sequences.len() - MAX_STOP_SEQUENCES,
            "za dużo 'stop_sequences' dla API OpenAI — wysyłam tylko pierwsze"
        );
    }
    Some(sequences.iter().take(MAX_STOP_SEQUENCES).cloned().collect())
}

fn map_tool_choice(choice: &AnthropicToolChoice) -> Option<Value> {
    match choice {
        AnthropicToolChoice::Auto => Some(Value::String("auto".to_string())),
        AnthropicToolChoice::Any => Some(Value::String("required".to_string())),
        AnthropicToolChoice::None => Some(Value::String("none".to_string())),
        AnthropicToolChoice::Tool { name } => Some(json!({
            "type": "function",
            "function": { "name": name }
        })),
        AnthropicToolChoice::Unknown => None,
    }
}

fn build_assistant_message(blocks: &[AnthropicContentBlock]) -> Option<ChatMessage> {
    let mut text = String::new();
    let mut tool_calls: Vec<OpenAiToolCall> = Vec::new();

    for block in blocks {
        match block {
            AnthropicContentBlock::Text { text: chunk } => text.push_str(chunk),
            AnthropicContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(OpenAiToolCall {
                    index: None,
                    id: Some(id.clone()),
                    kind: Some("function".to_string()),
                    function: Some(OpenAiFunctionCall {
                        name: Some(name.clone()),
                        arguments: Some(input.to_string()),
                    }),
                });
            }
            _ => {}
        }
    }

    if text.is_empty() && tool_calls.is_empty() {
        return None;
    }

    Some(ChatMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() {
            None
        } else {
            Some(OpenAiContent::Text(text))
        },
        name: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        ..Default::default()
    })
}

fn build_user_messages(
    blocks: &[AnthropicContentBlock],
) -> (Vec<ChatMessage>, Option<ChatMessage>) {
    let mut tool_messages: Vec<ChatMessage> = Vec::new();
    let mut tool_image_parts: Vec<OpenAiContentPart> = Vec::new();
    let mut parts: Vec<OpenAiContentPart> = Vec::new();
    let mut has_image = false;

    for block in blocks {
        match block {
            AnthropicContentBlock::Text { text } => {
                if !text.is_empty() {
                    parts.push(OpenAiContentPart::Text { text: text.clone() });
                }
            }
            AnthropicContentBlock::Image { source } => {
                if let Some(url) = image_source_to_url(source) {
                    has_image = true;
                    parts.push(OpenAiContentPart::ImageUrl {
                        image_url: OpenAiImageUrl { url },
                    });
                }
            }
            AnthropicContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut text = content
                    .as_ref()
                    .map(|content| content.as_text())
                    .unwrap_or_default();
                let images = content
                    .as_ref()
                    .map(|content| content.image_urls())
                    .unwrap_or_default();
                if !images.is_empty() {
                    has_image = true;
                    tool_image_parts.push(OpenAiContentPart::Text {
                        text: format!("[obraz z wyniku narzędzia {tool_use_id}]"),
                    });
                    tool_image_parts.extend(images.into_iter().map(|url| {
                        OpenAiContentPart::ImageUrl {
                            image_url: OpenAiImageUrl { url },
                        }
                    }));
                    if text.trim().is_empty() {
                        text =
                            "[wynik zawiera obraz — przekazany w następnej wiadomości]".to_string();
                    }
                }
                if is_error.unwrap_or(false) {
                    text = format!("ERROR: {text}");
                }
                tool_messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(OpenAiContent::Text(text)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some(tool_use_id.clone()),
                    ..Default::default()
                });
            }
            AnthropicContentBlock::ToolUse { .. }
            | AnthropicContentBlock::Thinking { .. }
            | AnthropicContentBlock::Unknown => {}
        }
    }

    tool_image_parts.append(&mut parts);
    let parts = tool_image_parts;

    let user_message = if parts.is_empty() {
        None
    } else if has_image {
        Some(ChatMessage {
            role: "user".to_string(),
            content: Some(OpenAiContent::Parts(parts)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            ..Default::default()
        })
    } else {
        let text = parts
            .iter()
            .filter_map(|part| match part {
                OpenAiContentPart::Text { text } => Some(text.as_str()),
                OpenAiContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(ChatMessage::text("user", text))
    };

    (tool_messages, user_message)
}

pub(crate) fn image_source_to_url(source: &ImageSource) -> Option<String> {
    match source.kind.as_str() {
        "base64" => {
            let media_type = source.media_type.as_deref().unwrap_or("image/png");
            let data = source.data.as_deref()?;
            Some(format!("data:{media_type};base64,{data}"))
        }
        "url" => source.url.clone(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::anthropic::MessagesRequest;
    use serde_json::json;

    fn translate(value: Value) -> ChatCompletionRequest {
        let request: MessagesRequest = serde_json::from_value(value).unwrap();
        anthropic_to_openai_request(&request).unwrap()
    }

    #[test]
    fn stop_sequences_are_capped_at_four() {
        let request = translate(json!({
            "model": "m",
            "stop_sequences": ["a", "b", "c", "d", "e", "f"],
            "messages": [{ "role": "user", "content": "hi" }]
        }));
        assert_eq!(request.stop.unwrap(), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn short_or_missing_stop_sequences_are_unchanged() {
        let two = translate(json!({
            "model": "m",
            "stop_sequences": ["a", "b"],
            "messages": [{ "role": "user", "content": "hi" }]
        }));
        assert_eq!(two.stop.unwrap(), vec!["a", "b"]);

        let none = translate(json!({
            "model": "m",
            "stop_sequences": [],
            "messages": [{ "role": "user", "content": "hi" }]
        }));
        assert!(none.stop.is_none());
    }

    #[test]
    fn tool_result_image_is_forwarded_as_user_image_after_tool_message() {
        let request = translate(json!({
            "model": "m",
            "messages": [
                { "role": "user", "content": "co jest na screenshot.png?" },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "toolu_1", "name": "Read", "input": { "file_path": "screenshot.png" } }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": [
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "iVBOR" } }
                    ]}
                ]}
            ]
        }));

        let roles: Vec<&str> = request.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "user"]);

        let tool = serde_json::to_value(&request.messages[2]).unwrap();
        assert_eq!(tool["tool_call_id"], "toolu_1");
        assert!(tool["content"].as_str().unwrap().contains("obraz"));

        let user = serde_json::to_value(&request.messages[3]).unwrap();
        let parts = user["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert!(parts[0]["text"].as_str().unwrap().contains("toolu_1"));
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,iVBOR");
    }

    #[test]
    fn tool_result_text_is_kept_next_to_image() {
        let request = translate(json!({
            "model": "m",
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "toolu_2", "name": "Read", "input": {} }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_2", "content": [
                        { "type": "text", "text": "podpis" },
                        { "type": "image", "source": { "type": "url", "url": "https://example.test/a.png" } }
                    ]},
                    { "type": "text", "text": "opisz to" }
                ]}
            ]
        }));

        let tool = serde_json::to_value(&request.messages[1]).unwrap();
        assert_eq!(tool["content"], "podpis");

        let user = serde_json::to_value(&request.messages[2]).unwrap();
        let parts = user["content"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1]["image_url"]["url"], "https://example.test/a.png");
        assert_eq!(parts[2]["text"], "opisz to");
    }

    #[test]
    fn text_only_tool_result_adds_no_user_message() {
        let request = translate(json!({
            "model": "m",
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "toolu_3", "name": "Bash", "input": {} }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_3", "content": "ok" }
                ]}
            ]
        }));
        let roles: Vec<&str> = request.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["assistant", "tool"]);
    }
}
