use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

const MAX_UPSTREAM_BODY: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorStyle {
    OpenAi,
    Anthropic,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("brak lub niepoprawny klucz API gatewaya")]
    Unauthorized,

    #[error("{0}")]
    BadRequest(String),

    #[error("nieznany model '{0}' — brak takiego 'model_name' w config.yaml")]
    UnknownModel(String),

    #[error("brak dostępnego deploymentu dla modelu '{0}'")]
    NoDeploymentAvailable(String),

    #[error("provider '{provider}' zwrócił HTTP {status}: {body}")]
    Upstream {
        provider: String,
        status: u16,
        body: String,
    },

    #[error("nie udało się połączyć z providerem: {0}")]
    UpstreamTransport(String),

    #[error("błąd wewnętrzny: {0}")]
    Internal(String),
}

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        AppError::BadRequest(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        AppError::Internal(msg.into())
    }

    pub fn upstream(provider: impl Into<String>, status: u16, body: impl Into<String>) -> Self {
        AppError::Upstream {
            provider: provider.into(),
            status,
            body: truncate(body.into(), MAX_UPSTREAM_BODY),
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::UnknownModel(_) => StatusCode::NOT_FOUND,
            AppError::NoDeploymentAvailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Upstream { status, .. } => StatusCode::from_u16(*status)
                .ok()
                .filter(|s| s.is_client_error() || s.is_server_error())
                .unwrap_or(StatusCode::BAD_GATEWAY),
            AppError::UpstreamTransport(_) => StatusCode::BAD_GATEWAY,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn openai_type(&self) -> &'static str {
        match self {
            AppError::Unauthorized => "invalid_request_error",
            AppError::BadRequest(_) => "invalid_request_error",
            AppError::UnknownModel(_) => "invalid_request_error",
            AppError::NoDeploymentAvailable(_) => "server_error",
            AppError::Upstream { .. } => "upstream_error",
            AppError::UpstreamTransport(_) => "upstream_error",
            AppError::Internal(_) => "server_error",
        }
    }

    pub fn anthropic_type(&self) -> &'static str {
        match self {
            AppError::Unauthorized => "authentication_error",
            AppError::BadRequest(_) => "invalid_request_error",
            AppError::UnknownModel(_) => "not_found_error",
            AppError::NoDeploymentAvailable(_) => "overloaded_error",
            AppError::Upstream { .. } => "api_error",
            AppError::UpstreamTransport(_) => "api_error",
            AppError::Internal(_) => "api_error",
        }
    }

    pub fn into_response_with(self, style: ErrorStyle) -> Response {
        let status = self.status();
        let message = self.to_string();
        let body = match style {
            ErrorStyle::OpenAi => json!({
                "error": {
                    "message": message,
                    "type": self.openai_type(),
                    "param": serde_json::Value::Null,
                    "code": serde_json::Value::Null,
                }
            }),
            ErrorStyle::Anthropic => json!({
                "type": "error",
                "error": {
                    "type": self.anthropic_type(),
                    "message": message,
                }
            }),
        };
        (status, axum::Json(body)).into_response()
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        self.into_response_with(ErrorStyle::OpenAi)
    }
}

#[derive(Debug)]
pub struct AnthropicError(pub AppError);

impl From<AppError> for AnthropicError {
    fn from(err: AppError) -> Self {
        AnthropicError(err)
    }
}

impl IntoResponse for AnthropicError {
    fn into_response(self) -> Response {
        self.0.into_response_with(ErrorStyle::Anthropic)
    }
}

pub fn truncate(mut value: String, max: usize) -> String {
    if value.chars().count() <= max {
        return value;
    }
    let cut = value
        .char_indices()
        .nth(max)
        .map(|(idx, _)| idx)
        .unwrap_or(value.len());
    value.truncate(cut);
    value.push_str("… (obcięte)");
    value
}
