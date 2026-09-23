use std::collections::HashMap;

use serde_json::{json, Value};

use crate::protocol::anthropic::new_message_id;
use crate::protocol::openai::{reasoning_str, ChatCompletionChunk, OpenAiToolCall};
use crate::protocol::translate::{map_stop_reason, matched_stop_sequence};

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}

impl SseEvent {
    fn new(event: &str, data: Value) -> Self {
        Self {
            event: event.to_string(),
            data,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(
            "error",
            json!({
                "type": "error",
                "error": { "type": "api_error", "message": message.into() }
            }),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurrentBlock {
    Thinking { index: u32 },
    Text { index: u32 },
    ToolUse { index: u32, openai_tool_index: u32 },
}

impl CurrentBlock {
    fn index(&self) -> u32 {
        match self {
            CurrentBlock::Thinking { index } => *index,
            CurrentBlock::Text { index } => *index,
            CurrentBlock::ToolUse { index, .. } => *index,
        }
    }
}

#[derive(Debug, Clone)]
struct ToolBuffer {
    anthropic_index: u32,
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    name: String,
    arguments: String,
    closed: bool,
}

#[derive(Debug)]
pub struct StreamState {
    model_alias: String,
    message_id: String,
    sent_message_start: bool,
    finished: bool,
    next_block_index: u32,
    current_block: Option<CurrentBlock>,
    tool_buffers: HashMap<u32, ToolBuffer>,
    tool_index_by_id: HashMap<String, u32>,
    next_synthetic_tool_index: u32,
    finish_reason: Option<String>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    thinking_enabled: bool,
    stop_sequences: Vec<String>,
    stop_sequence: Option<String>,
}

impl StreamState {
    pub fn new(model_alias: impl Into<String>) -> Self {
        Self {
            model_alias: model_alias.into(),
            message_id: new_message_id(),
            sent_message_start: false,
            finished: false,
            next_block_index: 0,
            current_block: None,
            tool_buffers: HashMap::new(),
            tool_index_by_id: HashMap::new(),
            next_synthetic_tool_index: 0,
            finish_reason: None,
            input_tokens: None,
            output_tokens: None,
            thinking_enabled: false,
            stop_sequences: Vec::new(),
            stop_sequence: None,
        }
    }

    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.thinking_enabled = enabled;
        self
    }

    pub fn with_stop_sequences(mut self, stop_sequences: Vec<String>) -> Self {
        self.stop_sequences = stop_sequences;
        self
    }

    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    pub fn tool_arguments(&self, openai_tool_index: u32) -> Option<&str> {
        self.tool_buffers
            .get(&openai_tool_index)
            .map(|buffer| buffer.arguments.as_str())
    }

    pub fn anthropic_index_for_tool(&self, openai_tool_index: u32) -> Option<u32> {
        self.tool_buffers
            .get(&openai_tool_index)
            .map(|buffer| buffer.anthropic_index)
    }

    pub fn handle_chunk(&mut self, chunk: &ChatCompletionChunk) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.finished {
            return events;
        }

        self.ensure_message_start(&mut events);

        if let Some(usage) = &chunk.usage {
            if let Some(prompt_tokens) = usage.prompt_tokens {
                self.input_tokens = Some(prompt_tokens);
            }
            if let Some(completion_tokens) = usage.completion_tokens {
                self.output_tokens = Some(completion_tokens);
            }
        }

        for choice in &chunk.choices {
            if self.thinking_enabled {
                if let Some(reasoning) =
                    reasoning_str(&choice.delta.reasoning_content, &choice.delta.reasoning)
                {
                    self.open_thinking_block(&mut events);
                    let block_index = self.current_block.map_or(0, |block| block.index());
                    events.push(SseEvent::new(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": block_index,
                            "delta": { "type": "thinking_delta", "thinking": reasoning }
                        }),
                    ));
                }
            }

            if let Some(text) = choice.delta.content.as_ref().filter(|t| !t.is_empty()) {
                self.open_text_block(&mut events);
                let block_index = match self.current_block {
                    Some(block) => block.index(),
                    None => 0,
                };
                events.push(SseEvent::new(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": block_index,
                        "delta": { "type": "text_delta", "text": text }
                    }),
                ));
            }

            if let Some(tool_calls) = &choice.delta.tool_calls {
                for tool_call in tool_calls {
                    self.handle_tool_call(tool_call, &mut events);
                }
            }

            if let Some(reason) = choice.finish_reason.as_ref().filter(|r| !r.is_empty()) {
                self.finish_reason = Some(reason.clone());
                if let Some(matched) = matched_stop_sequence(
                    Some(reason),
                    choice.stop_reason.as_ref(),
                    &self.stop_sequences,
                ) {
                    self.stop_sequence = Some(matched);
                }
            }
        }

        events
    }

    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.finished {
            return events;
        }

        self.ensure_message_start(&mut events);
        self.close_current_block(&mut events);
        self.finished = true;

        let has_tool_use = self.tool_buffers.values().any(|buffer| buffer.closed);
        let (stop_reason, stop_sequence) = if has_tool_use {
            ("tool_use".to_string(), None)
        } else if let Some(sequence) = self.stop_sequence.clone() {
            ("stop_sequence".to_string(), Some(sequence))
        } else {
            (map_stop_reason(self.finish_reason.as_deref()), None)
        };

        let mut usage = json!({ "output_tokens": self.output_tokens.unwrap_or(0) });
        if let Some(input_tokens) = self.input_tokens {
            usage["input_tokens"] = json!(input_tokens);
        }

        events.push(SseEvent::new(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": stop_sequence },
                "usage": usage
            }),
        ));
        events.push(SseEvent::new(
            "message_stop",
            json!({ "type": "message_stop" }),
        ));

        events
    }

    pub fn abort(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.finished {
            return events;
        }

        self.ensure_message_start(&mut events);
        self.close_current_block(&mut events);
        self.finished = true;

        events
    }

    fn ensure_message_start(&mut self, events: &mut Vec<SseEvent>) {
        if self.sent_message_start {
            return;
        }
        self.sent_message_start = true;
        events.push(SseEvent::new(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model_alias,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": {
                        "input_tokens": self.input_tokens.unwrap_or(0),
                        "output_tokens": 0
                    }
                }
            }),
        ));
    }

    fn open_thinking_block(&mut self, events: &mut Vec<SseEvent>) {
        if matches!(self.current_block, Some(CurrentBlock::Thinking { .. })) {
            return;
        }
        self.close_current_block(events);

        let index = self.next_block_index;
        self.next_block_index += 1;
        self.current_block = Some(CurrentBlock::Thinking { index });

        events.push(SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "thinking", "thinking": "", "signature": "" }
            }),
        ));
    }

    fn open_text_block(&mut self, events: &mut Vec<SseEvent>) {
        if matches!(self.current_block, Some(CurrentBlock::Text { .. })) {
            return;
        }
        self.close_current_block(events);

        let index = self.next_block_index;
        self.next_block_index += 1;
        self.current_block = Some(CurrentBlock::Text { index });

        events.push(SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "text", "text": "" }
            }),
        ));
    }

    fn close_current_block(&mut self, events: &mut Vec<SseEvent>) {
        let Some(block) = self.current_block.take() else {
            return;
        };
        if let CurrentBlock::ToolUse {
            openai_tool_index, ..
        } = block
        {
            if let Some(buffer) = self.tool_buffers.get_mut(&openai_tool_index) {
                buffer.closed = true;
            }
        }
        events.push(SseEvent::new(
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": block.index() }),
        ));
    }

    fn handle_tool_call(&mut self, tool_call: &OpenAiToolCall, events: &mut Vec<SseEvent>) {
        let openai_tool_index = self.resolve_tool_index(tool_call);

        if !self.tool_buffers.contains_key(&openai_tool_index) {
            self.close_current_block(events);

            let anthropic_index = self.next_block_index;
            self.next_block_index += 1;

            let id = tool_call
                .id
                .clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::new_v4().simple()));
            let name = tool_call
                .function
                .as_ref()
                .and_then(|function| function.name.clone())
                .unwrap_or_default();

            self.tool_buffers.insert(
                openai_tool_index,
                ToolBuffer {
                    anthropic_index,
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                    closed: false,
                },
            );
            self.current_block = Some(CurrentBlock::ToolUse {
                index: anthropic_index,
                openai_tool_index,
            });

            events.push(SseEvent::new(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": anthropic_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {}
                    }
                }),
            ));
        }

        if let Some(name) = tool_call
            .function
            .as_ref()
            .and_then(|function| function.name.as_ref())
            .filter(|name| !name.is_empty())
        {
            if let Some(buffer) = self.tool_buffers.get_mut(&openai_tool_index) {
                if buffer.name.is_empty() {
                    buffer.name = name.clone();
                }
            }
        }

        let Some(fragment) = tool_call
            .function
            .as_ref()
            .and_then(|function| function.arguments.as_ref())
            .filter(|arguments| !arguments.is_empty())
        else {
            return;
        };

        let Some(buffer) = self.tool_buffers.get_mut(&openai_tool_index) else {
            return;
        };
        buffer.arguments.push_str(fragment);
        let anthropic_index = buffer.anthropic_index;
        let closed = buffer.closed;

        if closed {
            tracing::warn!(
                openai_tool_index,
                "fragment 'arguments' dla już zamkniętego bloku tool_use — pomijam w SSE"
            );
            return;
        }

        events.push(SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": anthropic_index,
                "delta": { "type": "input_json_delta", "partial_json": fragment }
            }),
        ));
    }

    fn resolve_tool_index(&mut self, tool_call: &OpenAiToolCall) -> u32 {
        if let Some(index) = tool_call.index {
            return index;
        }
        if let Some(id) = tool_call.id.as_ref().filter(|id| !id.is_empty()) {
            if let Some(index) = self.tool_index_by_id.get(id) {
                return *index;
            }
            let index = self.next_synthetic_tool_index;
            self.next_synthetic_tool_index += 1;
            self.tool_index_by_id.insert(id.clone(), index);
            return index;
        }
        match self.current_block {
            Some(CurrentBlock::ToolUse {
                openai_tool_index, ..
            }) => openai_tool_index,
            _ => {
                let index = self.next_synthetic_tool_index;
                self.next_synthetic_tool_index += 1;
                index
            }
        }
    }
}
