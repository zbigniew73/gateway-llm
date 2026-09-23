pub mod auth;
pub mod config;
pub mod doctor;
pub mod error;
pub mod handlers;
pub mod protocol;
pub mod providers;
pub mod router;
pub mod state;

pub use config::Config;
pub use error::{AnthropicError, AppError, ErrorStyle};
pub use state::AppState;

pub const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;

pub fn build_app(state: AppState) -> axum::Router {
    use axum::extract::DefaultBodyLimit;
    use axum::routing::{get, post};

    let protected = axum::Router::new()
        .route(
            "/v1/chat/completions",
            post(handlers::chat_completions::handle),
        )
        .route("/v1/messages", post(handlers::messages::handle))
        .route(
            "/v1/messages/count_tokens",
            post(handlers::messages::count_tokens),
        )
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
