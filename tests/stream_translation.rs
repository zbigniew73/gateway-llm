//! Testy `StreamState` na syntetycznych sekwencjach chunków OpenAI (bez sieci).
//!
//! Sprawdzamy trzy scenariusze wskazane jako najbardziej ryzykowne:
//! 1. zwykły strumień tekstowy,
//! 2. `tool_use` — mapowanie `openai_tool_index -> anthropic_block_index`
//!    oraz to, że skonkatenowany `partial_json` parsuje się do poprawnego JSON-a,
//! 3. wiele bloków (tekst + dwa `tool_use`) w jednej odpowiedzi.

use gateway_llm::protocol::openai::ChatCompletionChunk;
use gateway_llm::protocol::translate::stream::{SseEvent, StreamState};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Pomocnicze
// ---------------------------------------------------------------------------

fn chunk(value: Value) -> ChatCompletionChunk {
    serde_json::from_value(value).expect("syntetyczny chunk musi się deserializować")
}

fn text_chunk(text: &str) -> ChatCompletionChunk {
    chunk(json!({
        "id": "chatcmpl-test",
        "object": "chat.completion.chunk",
        "model": "upstream/model",
        "choices": [{ "index": 0, "delta": { "content": text }, "finish_reason": null }]
    }))
}

fn finish_chunk(reason: &str) -> ChatCompletionChunk {
    chunk(json!({
        "id": "chatcmpl-test",
        "object": "chat.completion.chunk",
        "model": "upstream/model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }]
    }))
}

fn tool_start_chunk(index: u32, id: &str, name: &str) -> ChatCompletionChunk {
    chunk(json!({
        "id": "chatcmpl-test",
        "choices": [{
            "index": 0,
            "delta": { "tool_calls": [{
                "index": index,
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": "" }
            }]},
            "finish_reason": null
        }]
    }))
}

fn tool_args_chunk(index: u32, fragment: &str) -> ChatCompletionChunk {
    chunk(json!({
        "id": "chatcmpl-test",
        "choices": [{
            "index": 0,
            "delta": { "tool_calls": [{
                "index": index,
                "function": { "arguments": fragment }
            }]},
            "finish_reason": null
        }]
    }))
}

fn run(chunks: Vec<ChatCompletionChunk>) -> (StreamState, Vec<SseEvent>) {
    let mut state = StreamState::new("claude-sonnet-4");
    let mut events = Vec::new();
    for chunk in &chunks {
        events.extend(state.handle_chunk(chunk));
    }
    events.extend(state.finish());
    (state, events)
}

fn names(events: &[SseEvent]) -> Vec<&str> {
    events.iter().map(|event| event.event.as_str()).collect()
}

fn field<'a>(event: &'a SseEvent, path: &[&str]) -> &'a Value {
    let mut current = &event.data;
    for key in path {
        current = current
            .get(*key)
            .unwrap_or_else(|| panic!("brak pola '{key}' w zdarzeniu {}", event.event));
    }
    current
}

/// Skleja wszystkie `partial_json` dla danego indeksu bloku Anthropic.
fn concatenated_partial_json(events: &[SseEvent], block_index: u64) -> String {
    events
        .iter()
        .filter(|event| event.event == "content_block_delta")
        .filter(|event| event.data.get("index").and_then(Value::as_u64) == Some(block_index))
        .filter_map(|event| {
            event
                .data
                .get("delta")
                .and_then(|delta| delta.get("partial_json"))
                .and_then(Value::as_str)
        })
        .collect::<String>()
}

// ---------------------------------------------------------------------------
// 1. Zwykły tekst
// ---------------------------------------------------------------------------

#[test]
fn plain_text_stream_produces_canonical_anthropic_sequence() {
    let (_state, events) = run(vec![
        text_chunk("Hel"),
        text_chunk("lo"),
        text_chunk(" world"),
        finish_chunk("stop"),
    ]);

    assert_eq!(
        names(&events),
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );

    // message_start zawiera id, model (alias!) i pustą listę bloków
    let start = &events[0];
    assert_eq!(field(start, &["type"]), "message_start");
    assert_eq!(field(start, &["message", "role"]), "assistant");
    assert_eq!(field(start, &["message", "model"]), "claude-sonnet-4");
    assert_eq!(field(start, &["message", "content"]), &json!([]));

    // pojedynczy blok tekstowy o indeksie 0
    assert_eq!(field(&events[1], &["index"]), 0);
    assert_eq!(field(&events[1], &["content_block", "type"]), "text");

    let text: String = events
        .iter()
        .filter(|event| event.event == "content_block_delta")
        .filter_map(|event| {
            event
                .data
                .get("delta")
                .and_then(|delta| delta.get("text"))
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(text, "Hello world");

    assert_eq!(field(&events[5], &["index"]), 0);

    let message_delta = &events[6];
    assert_eq!(field(message_delta, &["delta", "stop_reason"]), "end_turn");
    assert_eq!(field(message_delta, &["usage", "output_tokens"]), 0);
}

#[test]
fn length_finish_reason_maps_to_max_tokens() {
    let (_state, events) = run(vec![text_chunk("abc"), finish_chunk("length")]);
    let message_delta = events
        .iter()
        .find(|event| event.event == "message_delta")
        .expect("message_delta musi wystąpić");
    assert_eq!(field(message_delta, &["delta", "stop_reason"]), "max_tokens");
}

#[test]
fn usage_from_final_chunk_is_reported() {
    let usage_chunk = chunk(json!({
        "id": "chatcmpl-test",
        "choices": [],
        "usage": { "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 }
    }));

    let (_state, events) = run(vec![text_chunk("hi"), finish_chunk("stop"), usage_chunk]);
    let message_delta = events
        .iter()
        .find(|event| event.event == "message_delta")
        .expect("message_delta musi wystąpić");
    assert_eq!(field(message_delta, &["usage", "output_tokens"]), 7);
}

// ---------------------------------------------------------------------------
// 2. tool_use
// ---------------------------------------------------------------------------

#[test]
fn tool_use_stream_maps_indices_and_rebuilds_valid_json() {
    let (state, events) = run(vec![
        tool_start_chunk(0, "call_abc123", "get_weather"),
        tool_args_chunk(0, "{\"locat"),
        tool_args_chunk(0, "ion\":\"San Francisco\""),
        tool_args_chunk(0, ",\"unit\":\"celsius\"}"),
        finish_chunk("tool_calls"),
    ]);

    assert_eq!(
        names(&events),
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );

    // Jedyny blok (tool_use) dostaje indeks Anthropic 0.
    let block_start = &events[1];
    assert_eq!(field(block_start, &["index"]), 0);
    assert_eq!(field(block_start, &["content_block", "type"]), "tool_use");
    assert_eq!(field(block_start, &["content_block", "id"]), "call_abc123");
    assert_eq!(field(block_start, &["content_block", "name"]), "get_weather");
    assert_eq!(field(block_start, &["content_block", "input"]), &json!({}));
    assert_eq!(state.anthropic_index_for_tool(0), Some(0));

    // Wszystkie delty to input_json_delta pod tym samym indeksem.
    for event in events.iter().filter(|e| e.event == "content_block_delta") {
        assert_eq!(field(event, &["index"]), 0);
        assert_eq!(field(event, &["delta", "type"]), "input_json_delta");
    }

    // Sklejone fragmenty muszą tworzyć poprawny JSON.
    let joined = concatenated_partial_json(&events, 0);
    assert_eq!(joined, r#"{"location":"San Francisco","unit":"celsius"}"#);
    let parsed: Value = serde_json::from_str(&joined).expect("partial_json musi sklejać się do JSON");
    assert_eq!(parsed, json!({"location": "San Francisco", "unit": "celsius"}));

    // Bufor stanu widzi dokładnie to samo.
    assert_eq!(state.tool_arguments(0), Some(joined.as_str()));

    let message_delta = &events[6];
    assert_eq!(field(message_delta, &["delta", "stop_reason"]), "tool_use");
}

#[test]
fn tool_call_without_index_is_tracked_by_id() {
    let start = chunk(json!({
        "id": "chatcmpl-test",
        "choices": [{
            "index": 0,
            "delta": { "tool_calls": [{
                "id": "call_no_index",
                "type": "function",
                "function": { "name": "ping", "arguments": "{}" }
            }]},
            "finish_reason": null
        }]
    }));

    let (state, events) = run(vec![start, finish_chunk("tool_calls")]);

    assert_eq!(
        names(&events),
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(state.anthropic_index_for_tool(0), Some(0));
    assert_eq!(state.tool_arguments(0), Some("{}"));
    assert_eq!(concatenated_partial_json(&events, 0), "{}");
}

// ---------------------------------------------------------------------------
// 3. Wiele bloków: tekst + dwa tool_use
// ---------------------------------------------------------------------------

#[test]
fn text_then_two_tool_calls_get_separate_anthropic_indices() {
    let (state, events) = run(vec![
        text_chunk("Sprawdzam pogodę."),
        tool_start_chunk(0, "call_1", "get_weather"),
        tool_args_chunk(0, "{\"city\":"),
        tool_args_chunk(0, "\"Warsaw\"}"),
        tool_start_chunk(1, "call_2", "get_time"),
        tool_args_chunk(1, "{\"tz\":\"CET\"}"),
        finish_chunk("tool_calls"),
    ]);

    assert_eq!(
        names(&events),
        vec![
            "message_start",
            // blok 0 – tekst
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            // blok 1 – pierwsze narzędzie
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            // blok 2 – drugie narzędzie
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );

    // Kluczowa asercja: indeksy narzędzi OpenAI (0, 1) NIE są indeksami bloków
    // Anthropic (1, 2) — bo blok 0 zajął tekst.
    assert_eq!(state.anthropic_index_for_tool(0), Some(1));
    assert_eq!(state.anthropic_index_for_tool(1), Some(2));

    assert_eq!(field(&events[1], &["index"]), 0);
    assert_eq!(field(&events[1], &["content_block", "type"]), "text");
    assert_eq!(field(&events[3], &["index"]), 0);

    assert_eq!(field(&events[4], &["index"]), 1);
    assert_eq!(field(&events[4], &["content_block", "id"]), "call_1");
    assert_eq!(field(&events[4], &["content_block", "name"]), "get_weather");
    assert_eq!(field(&events[7], &["index"]), 1);

    assert_eq!(field(&events[8], &["index"]), 2);
    assert_eq!(field(&events[8], &["content_block", "id"]), "call_2");
    assert_eq!(field(&events[8], &["content_block", "name"]), "get_time");
    assert_eq!(field(&events[10], &["index"]), 2);

    // Oba zlepki argumentów są poprawnym JSON-em, każdy pod swoim indeksem bloku.
    let first = concatenated_partial_json(&events, 1);
    let second = concatenated_partial_json(&events, 2);
    assert_eq!(
        serde_json::from_str::<Value>(&first).expect("poprawny JSON"),
        json!({"city": "Warsaw"})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&second).expect("poprawny JSON"),
        json!({"tz": "CET"})
    );
    assert_eq!(state.tool_arguments(0), Some(first.as_str()));
    assert_eq!(state.tool_arguments(1), Some(second.as_str()));

    assert_eq!(
        field(&events[11], &["delta", "stop_reason"]),
        "tool_use"
    );
}

#[test]
fn text_after_tool_call_opens_a_new_text_block() {
    let (state, events) = run(vec![
        tool_start_chunk(0, "call_1", "noop"),
        tool_args_chunk(0, "{}"),
        text_chunk("gotowe"),
        finish_chunk("stop"),
    ]);

    assert_eq!(
        names(&events),
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );

    assert_eq!(state.anthropic_index_for_tool(0), Some(0));
    assert_eq!(field(&events[4], &["index"]), 1);
    assert_eq!(field(&events[4], &["content_block", "type"]), "text");
    // Obecność zamkniętego bloku tool_use wymusza stop_reason = tool_use.
    assert_eq!(field(&events[7], &["delta", "stop_reason"]), "tool_use");
}

// ---------------------------------------------------------------------------
// Przypadki brzegowe
// ---------------------------------------------------------------------------

#[test]
fn finish_without_any_chunk_still_emits_full_envelope() {
    let mut state = StreamState::new("claude-sonnet-4");
    let events = state.finish();
    assert_eq!(names(&events), vec!["message_start", "message_delta", "message_stop"]);
    assert!(!state.message_id().is_empty());
}

#[test]
fn finish_is_idempotent() {
    let mut state = StreamState::new("claude-sonnet-4");
    let _ = state.handle_chunk(&text_chunk("x"));
    let first = state.finish();
    let second = state.finish();
    assert!(!first.is_empty());
    assert!(second.is_empty());
}

#[test]
fn empty_content_deltas_do_not_open_blocks() {
    let (_state, events) = run(vec![
        chunk(json!({
            "id": "chatcmpl-test",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "" }, "finish_reason": null }]
        })),
        finish_chunk("stop"),
    ]);

    assert_eq!(names(&events), vec!["message_start", "message_delta", "message_stop"]);
}
