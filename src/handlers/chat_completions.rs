//! `POST /v1/chat/completions` — czysty passthrough w formacie OpenAI.
//!
//! Gateway czyta z ciała żądania wyłącznie `model` (wybór aliasu) i `stream`;
//! resztę przekazuje providerowi 1:1. Odpowiedź non-stream wraca jako te same bajty,
//! a stream jest bajtowym proxy SSE — bez parsowania i bez typowanych structów,
//! ale z timeoutem bezczynności (`stream_idle_timeout_seconds`).

use std::convert::Infallible;
use std::fmt::Display;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::{Stream, StreamExt};
use serde_json::{json, Value};

use crate::error::AppError;
use crate::handlers::new_request_id;
use crate::state::AppState;

pub async fn handle(State(state): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    let request_id = new_request_id();

    let payload: Value = serde_json::from_slice(&body).map_err(|err| {
        AppError::bad_request(format!("ciało żądania nie jest poprawnym JSON: {err}"))
    })?;

    if !payload.is_object() {
        return Err(AppError::bad_request(
            "ciało żądania musi być obiektem JSON",
        ));
    }

    let alias = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|alias| !alias.is_empty())
        .ok_or_else(|| AppError::bad_request("pole 'model' jest wymagane"))?;

    let stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    tracing::debug!(request_id = %request_id, alias = %alias, stream, "/v1/chat/completions");

    let dispatched = state
        .router
        .dispatch(&alias, &payload, stream, false, &request_id)
        .await?;

    let upstream = dispatched.response;

    if stream {
        let content_type = upstream
            .headers()
            .get(header::CONTENT_TYPE)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("text/event-stream"));

        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            ],
            Body::from_stream(idle_guarded(
                upstream.bytes_stream(),
                state.config.stream_idle_timeout(),
                request_id,
            )),
        )
            .into_response());
    }

    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/json"));

    let bytes = upstream.bytes().await.map_err(|err| {
        AppError::UpstreamTransport(format!(
            "nie udało się odczytać odpowiedzi providera: {err}"
        ))
    })?;

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        bytes,
    )
        .into_response())
}

/// Przepuszcza bajty streamu providera bez zmian, ale gdy provider zamilknie
/// na dłużej niż `idle_timeout` albo połączenie się zerwie, kończy strumień
/// zdarzeniem `data: {"error": ...}` (BEZ `[DONE]`) — SDK OpenAI zgłasza wtedy
/// błąd, zamiast uznać uciętą odpowiedź za kompletną.
pub(crate) fn idle_guarded<S, E>(
    upstream: S,
    idle_timeout: Duration,
    request_id: String,
) -> impl Stream<Item = Result<Bytes, Infallible>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Display + Send + 'static,
{
    async_stream::stream! {
        let mut upstream = Box::pin(upstream);
        // Czy ostatni przekazany bajt kończył zdarzenie SSE — jeśli nie,
        // domykamy je przed doklejeniem zdarzenia błędu.
        let mut at_boundary = true;

        let failure = loop {
            match tokio::time::timeout(idle_timeout, upstream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    at_boundary = chunk.ends_with(b"\n\n") || chunk.ends_with(b"\r\n\r\n");
                    yield Ok(chunk);
                }
                Ok(None) => break None,
                Ok(Some(Err(err))) => {
                    tracing::warn!(
                        request_id = %request_id,
                        error = %err,
                        "błąd odczytu strumienia providera — przerywam strumień"
                    );
                    break Some(format!("błąd odczytu strumienia providera: {err}"));
                }
                Err(_) => {
                    tracing::warn!(request_id = %request_id, "provider zamilkł — przerywam strumień");
                    break Some("provider nie odpowiedział w oczekiwanym czasie".to_string());
                }
            }
        };

        if let Some(message) = failure {
            yield Ok(sse_error_event(&message, at_boundary));
        }
    }
}

fn sse_error_event(message: &str, at_boundary: bool) -> Bytes {
    let payload = json!({
        "error": { "message": message, "type": "upstream_error", "param": null, "code": null }
    });
    let prefix = if at_boundary { "" } else { "\n\n" };
    Bytes::from(format!("{prefix}data: {payload}\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    const IDLE: Duration = Duration::from_millis(100);

    async fn collect<S>(upstream: S) -> String
    where
        S: Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    {
        let chunks: Vec<_> = tokio::time::timeout(
            Duration::from_secs(5),
            idle_guarded(upstream, IDLE, "test".to_string()).collect::<Vec<_>>(),
        )
        .await
        .expect("strumień musi się zakończyć, a nie wisieć");
        chunks
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.unwrap().to_vec()).unwrap())
            .collect()
    }

    fn ok(text: &'static str) -> Result<Bytes, std::io::Error> {
        Ok(Bytes::from_static(text.as_bytes()))
    }

    #[tokio::test]
    async fn complete_stream_is_passed_through_unchanged() {
        let out = collect(stream::iter(vec![
            ok("data: {\"a\":1}\n\n"),
            ok("data: [DONE]\n\n"),
        ]))
        .await;
        assert_eq!(out, "data: {\"a\":1}\n\ndata: [DONE]\n\n");
    }

    #[tokio::test]
    async fn silence_mid_event_closes_it_and_appends_error_without_done() {
        let upstream = stream::iter(vec![ok("data: {\"par")]).chain(stream::pending());
        let out = collect(upstream).await;
        assert!(out.starts_with("data: {\"par\n\ndata: {\"error\""), "{out}");
        assert!(out.contains("upstream_error"));
        assert!(!out.contains("[DONE]"));
    }

    #[tokio::test]
    async fn silence_at_event_boundary_adds_no_extra_separator() {
        let upstream = stream::iter(vec![ok("data: {\"a\":1}\n\n")]).chain(stream::pending());
        let out = collect(upstream).await;
        assert!(
            out.starts_with("data: {\"a\":1}\n\ndata: {\"error\""),
            "{out}"
        );
    }

    #[tokio::test]
    async fn upstream_read_error_ends_with_error_event() {
        let upstream = stream::iter(vec![
            ok("data: {\"a\":1}\n\n"),
            Err(std::io::Error::other("connection reset")),
        ]);
        let out = collect(upstream).await;
        assert!(out.contains("connection reset"), "{out}");
        assert!(out.ends_with("\n\n"));
    }
}
