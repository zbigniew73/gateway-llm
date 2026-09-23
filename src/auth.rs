use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use crate::error::{AppError, ErrorStyle};
use crate::state::AppState;

const X_API_KEY: &str = "x-api-key";

pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let style = error_style_for_path(request.uri().path());

    let presented = extract_key(&request);

    match presented {
        Some(key) if constant_time_eq(key.as_bytes(), state.gateway_api_key.as_bytes()) => {
            next.run(request).await
        }
        Some(_) => {
            tracing::warn!(
                path = request.uri().path(),
                "odrzucono: niepoprawny klucz API"
            );
            AppError::Unauthorized.into_response_with(style)
        }
        None => {
            tracing::warn!(
                path = request.uri().path(),
                "odrzucono: brak nagłówka 'x-api-key' oraz 'Authorization: Bearer'"
            );
            AppError::Unauthorized.into_response_with(style)
        }
    }
}

fn error_style_for_path(path: &str) -> ErrorStyle {
    if path.starts_with("/v1/messages") {
        ErrorStyle::Anthropic
    } else {
        ErrorStyle::OpenAi
    }
}

fn extract_key(request: &Request) -> Option<String> {
    let headers = request.headers();

    if let Some(value) = headers.get(X_API_KEY).and_then(|v| v.to_str().ok()) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let authorization = headers.get(AUTHORIZATION)?.to_str().ok()?.trim();
    let token = strip_bearer(authorization)?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn strip_bearer(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(rest.trim())
    } else {
        None
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_is_parsed_case_insensitively() {
        assert_eq!(strip_bearer("Bearer abc"), Some("abc"));
        assert_eq!(strip_bearer("bearer abc"), Some("abc"));
        assert_eq!(strip_bearer("Basic abc"), None);
        assert_eq!(strip_bearer("abc"), None);
    }

    #[test]
    fn constant_time_eq_matches_semantics_of_eq() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrex"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
    }

    #[test]
    fn error_style_depends_on_path() {
        assert_eq!(error_style_for_path("/v1/messages"), ErrorStyle::Anthropic);
        assert_eq!(
            error_style_for_path("/v1/chat/completions"),
            ErrorStyle::OpenAi
        );
    }
}
