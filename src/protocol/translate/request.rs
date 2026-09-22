//! `MessagesRequest` (Anthropic) → `ChatCompletionRequest` (OpenAI).
//!
//! Najważniejsza różnica strukturalna: Anthropic pakuje wyniki narzędzi jako bloki
//! `tool_result` wewnątrz wiadomości `user`, a OpenAI wymaga osobnej wiadomości
//! `role: "tool"` dla każdego `tool_call_id`. Dlatego z jednej wiadomości `user`
//! może powstać kilka wiadomości OpenAI: najpierw wszystkie `tool`, potem
//! towarzyszący tekst jako `user`.

use serde_json::{json, Value};

use crate::error::AppError;
use crate::protocol::anthropic::{
    AnthropicContentBlock, AnthropicToolChoice, ImageSource, MessagesRequest,
};
use crate::protocol::openai::{
    ChatCompletionRequest, ChatMessage, OpenAiContent, OpenAiContentPart, OpenAiFunctionCall,
    OpenAiFunctionDef, OpenAiImageUrl, OpenAiTool, OpenAiToolCall,
};

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
            // Wszystko inne (w praktyce "user") traktujemy jak wiadomość użytkownika.
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

    // tool_choice ma sens tylko razem z listą narzędzi.
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
        stop: request
            .stop_sequences
            .clone()
            .filter(|sequences| !sequences.is_empty()),
        stream: request.stream,
        tools: tools.filter(|tools| !tools.is_empty()),
        tool_choice,
    })
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

/// Z bloków wiadomości `assistant` robi jedną wiadomość OpenAI
/// (tekst + ewentualne `tool_calls`).
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
    })
}

/// Z bloków wiadomości `user` robi listę wiadomości `role:"tool"` (po jednej na
/// `tool_result`) oraz co najwyżej jedną wiadomość `role:"user"` z resztą treści.
fn build_user_messages(
    blocks: &[AnthropicContentBlock],
) -> (Vec<ChatMessage>, Option<ChatMessage>) {
    let mut tool_messages: Vec<ChatMessage> = Vec::new();
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
                if is_error.unwrap_or(false) {
                    text = format!("ERROR: {text}");
                }
                tool_messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(OpenAiContent::Text(text)),
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some(tool_use_id.clone()),
                });
            }
            AnthropicContentBlock::ToolUse { .. } | AnthropicContentBlock::Unknown => {}
        }
    }

    let user_message = if parts.is_empty() {
        None
    } else if has_image {
        Some(ChatMessage {
            role: "user".to_string(),
            content: Some(OpenAiContent::Parts(parts)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
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

/// Anthropic `image.source` → OpenAI `image_url.url` (data-URI dla base64).
fn image_source_to_url(source: &ImageSource) -> Option<String> {
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
