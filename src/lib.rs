//! gateway-llm — lekki, lokalny gateway LLM.
//!
//! Wystawia dwa kompatybilne wire-protokoły na jednym porcie:
//!
//! * `POST /v1/chat/completions` — OpenAI-compatible, czysty JSON/bajtowy passthrough
//!   do jednego z backendów (OpenRouter / Novita.ai / Infron.ai / NVIDIA NIM).
//! * `POST /v1/messages` — Anthropic Messages API-compatible (Claude Code), z pełną
//!   translacją request / response / streaming do i z formatu OpenAI.
//!
//! Kod jest wydzielony do biblioteki (a binarka to cienki `main.rs`), żeby testy
//! integracyjne w `tests/` mogły korzystać z [`protocol::translate::stream::StreamState`].

pub mod auth;
pub mod config;
pub mod error;
pub mod handlers;
pub mod protocol;
pub mod providers;
pub mod router;
pub mod state;

pub use config::Config;
pub use error::{AnthropicError, AppError, ErrorStyle};
pub use state::AppState;

/// Maksymalny rozmiar ciała żądania. Domyślne 2 MB z axum jest za małe dla
/// długich sesji Claude Code i obrazów w base64; 32 MB to limit samego
/// Anthropic Messages API.
pub const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Buduje kompletny `axum::Router` gatewaya.
///
/// `/healthz` jest celowo POZA middleware auth; oba endpointy `/v1/*` są nim objęte.
pub fn build_app(state: AppState) -> axum::Router {
    use axum::extract::DefaultBodyLimit;
    use axum::routing::{get, post};

    let protected = axum::Router::new()
        .route("/v1/chat/completions", post(handlers::chat_completions::handle))
        .route("/v1/messages", post(handlers::messages::handle))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));

    axum::Router::new()
        .route("/healthz", get(handlers::healthz))
        .merge(protected)
        .with_state(state)
}
