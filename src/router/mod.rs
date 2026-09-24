pub mod health;
pub mod rate_limit;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use crate::config::{Config, Deployment};
use crate::error::AppError;
use crate::providers;
use crate::router::health::HealthTracker;
use crate::router::rate_limit::RateLimiter;

pub struct Dispatched {
    pub response: reqwest::Response,
    pub provider: String,
    pub upstream_model: String,
    pub attempts: usize,
}

pub struct Router {
    client: reqwest::Client,
    config: Arc<Config>,
    health: HealthTracker,
    rate_limiter: RateLimiter,
}

impl Router {
    pub fn new(config: Arc<Config>) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("gateway-llm/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(config.connect_timeout())
            .build()
            .map_err(|err| {
                AppError::internal(format!("nie udało się zbudować klienta HTTP: {err}"))
            })?;

        let health = HealthTracker::new(&config.routing);
        let rate_limiter = RateLimiter::new(&config.providers);
        Ok(Self {
            client,
            config,
            health,
            rate_limiter,
        })
    }

    pub fn health(&self) -> &HealthTracker {
        &self.health
    }

    pub async fn dispatch(
        &self,
        alias: &str,
        body: &Value,
        stream: bool,
        stream_usage: bool,
        request_id: &str,
    ) -> Result<Dispatched, AppError> {
        let mut current = alias.to_string();
        let mut visited: HashSet<String> = HashSet::new();

        loop {
            if !visited.insert(current.clone()) {
                tracing::error!(
                    request_id,
                    alias = %current,
                    "wykryto cykl w 'fallback_model' w trakcie routingu — przerywam"
                );
                return Err(AppError::NoDeploymentAvailable(alias.to_string()));
            }

            match self
                .dispatch_one(&current, body, stream, stream_usage, request_id)
                .await
            {
                Ok(dispatched) => return Ok(dispatched),
                Err(Failure::Terminal(err)) => return Err(err),
                Err(Failure::Exhausted(err)) => {
                    let fallback = self
                        .config
                        .model_entry(&current)
                        .and_then(|entry| entry.fallback_model.clone());

                    match fallback {
                        Some(next_alias) => {
                            tracing::warn!(
                                request_id,
                                from_alias = %current,
                                to_alias = %next_alias,
                                error = %err,
                                "alias wyczerpał deploymenty — przechodzę na 'fallback_model'"
                            );
                            current = next_alias;
                        }
                        None => return Err(err),
                    }
                }
            }
        }
    }

    async fn dispatch_one(
        &self,
        alias: &str,
        body: &Value,
        stream: bool,
        stream_usage: bool,
        request_id: &str,
    ) -> Result<Dispatched, Failure> {
        let entry = self
            .config
            .model_entry(alias)
            .ok_or_else(|| Failure::Terminal(AppError::UnknownModel(alias.to_string())))?;

        let (mut candidates, over_limit): (Vec<&Deployment>, Vec<&Deployment>) = entry
            .deployments
            .iter()
            .filter(|deployment| self.health.is_available(&deployment.health_key()))
            .partition(|deployment| self.rate_limiter.has_capacity(&deployment.provider));
        candidates.extend(over_limit);

        if candidates.is_empty() {
            tracing::warn!(
                request_id,
                alias,
                "wszystkie deploymenty są w cooldownie — próbuję mimo to"
            );
            candidates = entry.deployments.iter().collect();
        }

        let mut attempts = 0usize;
        let mut last_error: Option<AppError> = None;

        for deployment in candidates {
            let provider = match self.config.provider(&deployment.provider) {
                Some(provider) => provider,
                None => {
                    tracing::error!(
                        request_id,
                        provider = %deployment.provider,
                        "deployment wskazuje na nieznanego providera — pomijam"
                    );
                    continue;
                }
            };

            let Some(api_key) = providers::api_key_for(deployment) else {
                tracing::warn!(
                    request_id,
                    alias,
                    provider = %deployment.provider,
                    api_key_env = %deployment.api_key_env,
                    "brak klucza API w środowisku — pomijam deployment"
                );
                if last_error.is_none() {
                    last_error = Some(AppError::NoDeploymentAvailable(alias.to_string()));
                }
                continue;
            };

            let upstream_body = build_upstream_body(
                body,
                &deployment.model,
                stream,
                stream_usage && deployment.stream_usage,
            )
            .map_err(Failure::Terminal)?;

            let timeout = if stream {
                None
            } else {
                Some(self.config.non_stream_timeout())
            };

            attempts += 1;
            let health_key = deployment.health_key();
            let started = Instant::now();

            if !self.rate_limiter.try_acquire(&deployment.provider) {
                tracing::warn!(
                    request_id,
                    alias,
                    provider = %deployment.provider,
                    "wysyłam mimo braku wolnego tokena RPM — ryzykuję 429 u providera"
                );
            }

            let request =
                providers::build_request(&self.client, provider, &api_key, &upstream_body, timeout);

            let send = async {
                if stream {
                    match tokio::time::timeout(self.config.connect_timeout(), request.send()).await
                    {
                        Ok(result) => result.map_err(|err| err.to_string()),
                        Err(_) => {
                            Err("przekroczono czas oczekiwania na nagłówki odpowiedzi".to_string())
                        }
                    }
                } else {
                    request.send().await.map_err(|err| err.to_string())
                }
            };

            match send.await {
                Err(message) => {
                    let cooled = self.health.record_failure(&health_key);
                    tracing::warn!(
                        request_id,
                        alias,
                        provider = %deployment.provider,
                        model = %deployment.model,
                        attempt = attempts,
                        latency_ms = started.elapsed().as_millis() as u64,
                        cooldown_started = cooled,
                        error = %message,
                        "błąd transportu — próbuję kolejny deployment"
                    );
                    last_error = Some(AppError::UpstreamTransport(message));
                }
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        self.health.record_success(&health_key);
                        tracing::info!(
                            request_id,
                            alias,
                            provider = %deployment.provider,
                            model = %deployment.model,
                            attempt = attempts,
                            latency_ms = started.elapsed().as_millis() as u64,
                            status = status.as_u16(),
                            stream,
                            "routing zakończony powodzeniem"
                        );
                        return Ok(Dispatched {
                            response,
                            provider: deployment.provider.clone(),
                            upstream_model: deployment.model.clone(),
                            attempts,
                        });
                    }

                    let body_text = response.text().await.unwrap_or_default();
                    let context_exceeded = is_context_length_error(status.as_u16(), &body_text);
                    let error =
                        AppError::upstream(deployment.provider.clone(), status.as_u16(), body_text);

                    if context_exceeded {
                        tracing::warn!(
                            request_id,
                            alias,
                            provider = %deployment.provider,
                            model = %deployment.model,
                            attempt = attempts,
                            status = status.as_u16(),
                            "przekroczone okno kontekstu modelu — próbuję kolejny deployment"
                        );
                        last_error = Some(error);
                    } else if is_retryable(status.as_u16()) {
                        let cooled = self.health.record_failure(&health_key);
                        tracing::warn!(
                            request_id,
                            alias,
                            provider = %deployment.provider,
                            model = %deployment.model,
                            attempt = attempts,
                            latency_ms = started.elapsed().as_millis() as u64,
                            status = status.as_u16(),
                            cooldown_started = cooled,
                            "provider zwrócił błąd — próbuję kolejny deployment"
                        );
                        last_error = Some(error);
                    } else {
                        tracing::warn!(
                            request_id,
                            alias,
                            provider = %deployment.provider,
                            status = status.as_u16(),
                            "provider zwrócił błąd klienta — bez fallbacku"
                        );
                        return Err(Failure::Terminal(error));
                    }
                }
            }
        }

        Err(Failure::Exhausted(last_error.unwrap_or_else(|| {
            AppError::NoDeploymentAvailable(alias.to_string())
        })))
    }
}

enum Failure {
    Exhausted(AppError),
    Terminal(AppError),
}

fn is_context_length_error(status: u16, body: &str) -> bool {
    if !matches!(status, 400 | 422) {
        return false;
    }
    let body = body.to_ascii_lowercase();
    [
        "context_length_exceeded",
        "context length",
        "context window",
        "maximum context",
        "too many tokens",
        "prompt is too long",
    ]
    .iter()
    .any(|phrase| body.contains(phrase))
}

fn build_upstream_body(
    body: &Value,
    model: &str,
    stream: bool,
    include_usage: bool,
) -> Result<Value, AppError> {
    let mut upstream = body.clone();
    let object = upstream
        .as_object_mut()
        .ok_or_else(|| AppError::bad_request("ciało żądania musi być obiektem JSON"))?;
    object.insert("model".to_string(), Value::String(model.to_string()));
    object.insert("stream".to_string(), Value::Bool(stream));
    if stream && include_usage {
        object.insert(
            "stream_options".to_string(),
            serde_json::json!({ "include_usage": true }),
        );
    }
    Ok(upstream)
}

fn is_retryable(status: u16) -> bool {
    matches!(status, 401 | 402 | 403 | 404 | 408 | 409 | 413 | 429) || status >= 500
}

#[cfg(test)]
mod tests {
    use super::{build_upstream_body, is_context_length_error, is_retryable};
    use serde_json::json;

    #[test]
    fn context_length_errors_are_recognized_across_providers() {
        assert!(is_context_length_error(
            400,
            r#"{"error":{"code":"context_length_exceeded","message":"..."}}"#
        ));
        assert!(is_context_length_error(
            400,
            "This model's maximum context length is 32768 tokens. However, you requested 40000 tokens."
        ));
        assert!(is_context_length_error(
            400,
            "This endpoint's maximum context length is 131072 tokens."
        ));
        assert!(is_context_length_error(
            422,
            "Input exceeds the context window"
        ));
        assert!(is_context_length_error(400, "Prompt is too long"));
    }

    #[test]
    fn other_client_errors_are_not_context_errors() {
        assert!(!is_context_length_error(
            400,
            "invalid tool schema: missing 'type'"
        ));
        assert!(!is_context_length_error(
            400,
            "unsupported parameter: top_k"
        ));
        assert!(!is_context_length_error(
            500,
            "maximum context length exceeded"
        ));
    }

    #[test]
    fn stream_options_added_only_for_stream_with_usage_enabled() {
        let body = json!({"model": "alias", "messages": []});

        let with = build_upstream_body(&body, "up/model", true, true).unwrap();
        assert_eq!(with["model"], "up/model");
        assert_eq!(with["stream"], true);
        assert_eq!(with["stream_options"], json!({"include_usage": true}));

        let flag_off = build_upstream_body(&body, "up/model", true, false).unwrap();
        assert!(flag_off.get("stream_options").is_none());

        let non_stream = build_upstream_body(&body, "up/model", false, true).unwrap();
        assert!(non_stream.get("stream_options").is_none());
        assert_eq!(non_stream["stream"], false);
    }

    #[test]
    fn non_object_body_is_rejected() {
        assert!(build_upstream_body(&json!([1, 2]), "m", true, true).is_err());
    }

    #[test]
    fn client_errors_are_not_retried_except_auth_and_rate_limits() {
        assert!(!is_retryable(400));
        assert!(!is_retryable(422));
        assert!(is_retryable(401));
        assert!(is_retryable(429));
        assert!(is_retryable(500));
        assert!(is_retryable(502));
    }
}
