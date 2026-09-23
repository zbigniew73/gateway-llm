//! `POST /v1/chat/completions` — czysty passthrough w formacie OpenAI.
//!
//! Gateway czyta z ciała żądania wyłącznie `model` (wybór aliasu) i `stream`;
//! resztę przekazuje providerowi 1:1. Odpowiedź non-stream wraca jako te same bajty,
//! a stream jest bajtowym proxy SSE — bez parsowania i bez typowanych structów.

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::error::AppError;
use crate::handlers::new_request_id;
use crate::state::AppState;

pub async fn handle(State(state): State<AppState>, body: Bytes) -> Result<Response, AppError> {
    let request_id = new_request_id();

    let payload: Value = serde_json::from_slice(&body)
        .map_err(|err| AppError::bad_request(format!("ciało żądania nie jest poprawnym JSON: {err}")))?;

    if !payload.is_object() {
        return Err(AppError::bad_request("ciało żądania musi być obiektem JSON"));
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
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("no-cache"),
                ),
            ],
            Body::from_stream(upstream.bytes_stream()),
        )
            .into_response());
    }

    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/json"));

    let bytes = upstream.bytes().await.map_err(|err| {
        AppError::UpstreamTransport(format!("nie udało się odczytać odpowiedzi providera: {err}"))
    })?;

    Ok((StatusCode::OK, [(header::CONTENT_TYPE, content_type)], bytes).into_response())
}
