//! Wspólny router/fallback dla OBU endpointów.
//!
//! `Router::dispatch()` dostaje alias modelu + body już w kształcie OpenAI i zwraca
//! surową `reqwest::Response` pierwszego deploymentu, który odpowiedział poprawnie.
//! Handlery różnią się wyłącznie tym, co z tą odpowiedzią robią (passthrough vs translacja).

pub mod health;

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use crate::config::{Config, Deployment};
use crate::error::AppError;
use crate::providers;
use crate::router::health::HealthTracker;

/// Wynik udanego routingu.
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
}

impl Router {
    pub fn new(config: Arc<Config>) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("gateway-llm/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|err| AppError::internal(format!("nie udało się zbudować klienta HTTP: {err}")))?;

        let health = HealthTracker::new(&config.routing);
        Ok(Self {
            client,
            config,
            health,
        })
    }

    pub fn health(&self) -> &HealthTracker {
        &self.health
    }

    /// Przechodzi po deploymentach aliasu w kolejności `order` i zwraca pierwszą
    /// udaną odpowiedź providera. Deploymenty w cooldownie są pomijane w pierwszym
    /// podejściu; jeżeli wszystkie są w cooldownie, próbujemy ich mimo to (lepiej
    /// spróbować, niż odesłać klientowi błąd bez żadnej próby).
    pub async fn dispatch(
        &self,
        alias: &str,
        body: &Value,
        stream: bool,
        request_id: &str,
    ) -> Result<Dispatched, AppError> {
        let entry = self
            .config
            .model_entry(alias)
            .ok_or_else(|| AppError::UnknownModel(alias.to_string()))?;

        let mut candidates: Vec<&Deployment> = entry
            .deployments
            .iter()
            .filter(|deployment| self.health.is_available(&deployment.health_key()))
            .collect();

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
                    // Walidacja configu tego nie przepuści, ale nie panikujemy.
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

            let mut upstream_body = body.clone();
            if let Some(object) = upstream_body.as_object_mut() {
                object.insert("model".to_string(), Value::String(deployment.model.clone()));
                object.insert("stream".to_string(), Value::Bool(stream));
            } else {
                return Err(AppError::bad_request(
                    "ciało żądania musi być obiektem JSON",
                ));
            }

            let timeout = if stream {
                None
            } else {
                Some(self.config.request_timeout())
            };

            attempts += 1;
            let health_key = deployment.health_key();
            let started = Instant::now();

            let request =
                providers::build_request(&self.client, provider, &api_key, &upstream_body, timeout);

            // Dla streamu timeout request-level jest wyłączony, więc pilnujemy
            // przynajmniej fazy nawiązania połączenia i nagłówków.
            let send = async {
                if stream {
                    match tokio::time::timeout(self.config.request_timeout(), request.send()).await
                    {
                        Ok(result) => result.map_err(|err| err.to_string()),
                        Err(_) => Err("przekroczono czas oczekiwania na nagłówki odpowiedzi"
                            .to_string()),
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
                    let error = AppError::upstream(
                        deployment.provider.clone(),
                        status.as_u16(),
                        body_text,
                    );

                    if is_retryable(status.as_u16()) {
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
                        // 4xx po stronie klienta (np. 400 – zły request) nie naprawi
                        // się na innym providerze; zwracamy od razu.
                        tracing::warn!(
                            request_id,
                            alias,
                            provider = %deployment.provider,
                            status = status.as_u16(),
                            "provider zwrócił błąd klienta — bez fallbacku"
                        );
                        return Err(error);
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| AppError::NoDeploymentAvailable(alias.to_string())))
    }
}

/// Czy przy takim kodzie HTTP warto spróbować kolejnego deploymentu.
fn is_retryable(status: u16) -> bool {
    matches!(status, 401 | 402 | 403 | 404 | 408 | 409 | 413 | 429) || status >= 500
}

#[cfg(test)]
mod tests {
    use super::is_retryable;

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
