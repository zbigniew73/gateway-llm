//! Handlery HTTP.

pub mod chat_completions;
pub mod messages;

use axum::http::StatusCode;
use axum::Json;
use serde_json::json;

/// `GET /healthz` — celowo bez autoryzacji.
pub async fn healthz() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "service": "gateway-llm",
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

/// Identyfikator żądania używany w logach.
pub(crate) fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
