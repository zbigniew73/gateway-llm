//! Budowa żądania HTTP do dowolnego z czterech backendów.
//!
//! Wszystkie cztery (OpenRouter, Novita.ai, Infron.ai, NVIDIA NIM) są OpenAI-wire-compatible
//! i autoryzują się nagłówkiem `Authorization: Bearer <klucz>`, więc różnią się wyłącznie
//! `base_url` + `chat_path` (z `config.yaml`) oraz nazwą modelu.

use std::time::Duration;

use reqwest::RequestBuilder;
use serde_json::Value;

use crate::config::{Deployment, ProviderConfig};

/// Zwraca klucz API deploymentu ze środowiska (pusty string traktujemy jak brak).
pub fn api_key_for(deployment: &Deployment) -> Option<String> {
    std::env::var(&deployment.api_key_env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Składa `reqwest::RequestBuilder` gotowy do wysłania.
///
/// `timeout` ustawiamy WYŁĄCZNIE dla żądań non-stream — timeout request-level w reqwest
/// obejmuje całe body, więc dla streamu ucinałby połączenie w trakcie odpowiedzi.
/// Dla streamu timeout bezczynności jest pilnowany osobno, per-event.
pub fn build_request(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    api_key: &str,
    body: &Value,
    timeout: Option<Duration>,
) -> RequestBuilder {
    let mut builder = client
        .post(provider.chat_url())
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
        .json(body);

    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }

    builder
}

#[cfg(test)]
mod tests {
    use crate::config::ProviderConfig;

    #[test]
    fn chat_url_joins_without_double_slash() {
        let provider = ProviderConfig {
            base_url: "https://openrouter.ai/api/v1/".to_string(),
            chat_path: "/chat/completions".to_string(),
            rpm: None,
        };
        assert_eq!(
            provider.chat_url(),
            "https://openrouter.ai/api/v1/chat/completions"
        );

        let provider = ProviderConfig {
            base_url: "https://llm.onerouter.pro".to_string(),
            chat_path: "v1/chat/completions".to_string(),
            rpm: None,
        };
        assert_eq!(
            provider.chat_url(),
            "https://llm.onerouter.pro/v1/chat/completions"
        );
    }
}
