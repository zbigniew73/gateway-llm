//! `POST /v1/messages` — Anthropic Messages API z pełną translacją do/z formatu OpenAI.
//!
//! Non-stream: request → OpenAI → provider → OpenAI response → Anthropic response.
//! Stream: SSE providera jest parsowane (`eventsource-stream`) i przepuszczane przez
//! [`StreamState`], która emituje zdarzenia w formacie Anthropic.

use std::convert::Infallible;

use axum::body::Bytes;
use axum::extract::State;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use eventsource_stream::Eventsource;
use futures::StreamExt;

use crate::error::{AnthropicError, AppError};
use crate::handlers::new_request_id;
use crate::protocol::anthropic::MessagesRequest;
use crate::protocol::openai::{ChatCompletionChunk, ChatCompletionResponse};
use crate::protocol::translate::request::anthropic_to_openai_request;
use crate::protocol::translate::response::openai_to_anthropic_response;
use crate::protocol::translate::stream::{SseEvent, StreamState};
use crate::state::AppState;

pub async fn handle(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, AnthropicError> {
    let request_id = new_request_id();

    let request: MessagesRequest = serde_json::from_slice(&body).map_err(|err| {
        AppError::bad_request(format!("nie udało się odczytać żądania Messages API: {err}"))
    })?;

    let alias = request.model.clone();
    let stream = request.stream.unwrap_or(false);

    let openai_request = anthropic_to_openai_request(&request)?;
    let payload = serde_json::to_value(&openai_request).map_err(|err| {
        AppError::internal(format!("nie udało się zserializować żądania do providera: {err}"))
    })?;

    tracing::debug!(request_id = %request_id, alias = %alias, stream, "/v1/messages");

    let dispatched = state
        .router
        .dispatch(&alias, &payload, stream, &request_id)
        .await?;

    let upstream = dispatched.response;

    if !stream {
        let bytes = upstream.bytes().await.map_err(|err| {
            AppError::UpstreamTransport(format!(
                "nie udało się odczytać odpowiedzi providera: {err}"
            ))
        })?;

        let parsed: ChatCompletionResponse = serde_json::from_slice(&bytes).map_err(|err| {
            AppError::internal(format!(
                "odpowiedź providera nie pasuje do formatu OpenAI ({err}): {}",
                crate::error::truncate(String::from_utf8_lossy(&bytes).to_string(), 500)
            ))
        })?;

        let translated = openai_to_anthropic_response(&parsed, &alias);
        return Ok(Json(translated).into_response());
    }

    // --- streaming -------------------------------------------------------
    // Decyzja o fallbacku zapadła już w `dispatch()`; od tego momentu tylko
    // tłumaczymy strumień. Timeout bezczynności pilnujemy per-event, bo
    // request-level timeout reqwest obejmowałby całą (długą) odpowiedź.
    let idle_timeout = state.config.request_timeout();
    let request_id_for_stream = request_id.clone();

    let events = async_stream::stream! {
        let mut event_source = Box::pin(upstream.bytes_stream().eventsource());
        let mut machine = StreamState::new(alias);

        loop {
            let next = tokio::time::timeout(idle_timeout, event_source.next()).await;

            let event = match next {
                Err(_) => {
                    tracing::warn!(
                        request_id = %request_id_for_stream,
                        "provider zamilkł — zamykam strumień"
                    );
                    break;
                }
                Ok(None) => break,
                Ok(Some(Err(err))) => {
                    tracing::warn!(
                        request_id = %request_id_for_stream,
                        error = %err,
                        "błąd odczytu strumienia providera — zamykam strumień"
                    );
                    break;
                }
                Ok(Some(Ok(event))) => event,
            };

            let data = event.data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                break;
            }

            match serde_json::from_str::<ChatCompletionChunk>(data) {
                Ok(chunk) => {
                    for translated in machine.handle_chunk(&chunk) {
                        if let Some(sse) = to_sse(&translated) {
                            yield Ok::<Event, Infallible>(sse);
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        request_id = %request_id_for_stream,
                        error = %err,
                        "pomijam nierozpoznany chunk SSE providera"
                    );
                }
            }
        }

        for translated in machine.finish() {
            if let Some(sse) = to_sse(&translated) {
                yield Ok::<Event, Infallible>(sse);
            }
        }
    };

    Ok(Sse::new(events).into_response())
}

/// Zamienia zdarzenie z `StreamState` na `axum` SSE (`event:` + `data:`).
fn to_sse(event: &SseEvent) -> Option<Event> {
    match Event::default().event(&event.event).json_data(&event.data) {
        Ok(sse) => Some(sse),
        Err(err) => {
            tracing::error!(
                event = %event.event,
                error = %err,
                "nie udało się zserializować zdarzenia SSE"
            );
            None
        }
    }
}
