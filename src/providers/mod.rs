use std::time::Duration;

use reqwest::RequestBuilder;
use serde_json::Value;

use crate::config::{Deployment, ProviderConfig};

pub fn api_key_for(deployment: &Deployment) -> Option<String> {
    std::env::var(&deployment.api_key_env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

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

    for (name, value) in &provider.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }

    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }

    builder
}

#[cfg(test)]
mod tests {
    use crate::config::ProviderConfig;

    #[test]
    fn provider_headers_are_sent() {
        let mut provider = ProviderConfig {
            base_url: "https://openrouter.ai/api/v1".to_string(),
            chat_path: "/chat/completions".to_string(),
            rpm: None,
            headers: Default::default(),
        };
        provider.headers.insert(
            "HTTP-Referer".to_string(),
            "https://github.com/zbigniew73/gateway-llm".to_string(),
        );
        provider
            .headers
            .insert("X-OpenRouter-Title".to_string(), "Gateway LLM".to_string());

        let client = reqwest::Client::new();
        let request = super::build_request(&client, &provider, "key", &serde_json::json!({}), None)
            .build()
            .unwrap();
        let headers = request.headers();
        assert_eq!(
            headers["http-referer"],
            "https://github.com/zbigniew73/gateway-llm"
        );
        assert_eq!(headers["x-openrouter-title"], "Gateway LLM");
        assert_eq!(headers["authorization"], "Bearer key");
    }

    #[test]
    fn chat_url_joins_without_double_slash() {
        let provider = ProviderConfig {
            base_url: "https://openrouter.ai/api/v1/".to_string(),
            chat_path: "/chat/completions".to_string(),
            rpm: None,
            headers: Default::default(),
        };
        assert_eq!(
            provider.chat_url(),
            "https://openrouter.ai/api/v1/chat/completions"
        );

        let provider = ProviderConfig {
            base_url: "https://llm.onerouter.pro".to_string(),
            chat_path: "v1/chat/completions".to_string(),
            rpm: None,
            headers: Default::default(),
        };
        assert_eq!(
            provider.chat_url(),
            "https://llm.onerouter.pro/v1/chat/completions"
        );
    }
}
