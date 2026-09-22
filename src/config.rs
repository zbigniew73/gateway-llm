//! Wczytanie i walidacja `config.yaml`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Domyślna nazwa pliku konfiguracyjnego (względem katalogu roboczego).
pub const DEFAULT_CONFIG_FILE: &str = "config.yaml";

/// Zmienna środowiskowa nadpisująca ścieżkę do konfiguracji.
pub const CONFIG_PATH_ENV: &str = "GATEWAY_CONFIG";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("nie można odczytać pliku konfiguracyjnego '{path}': {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("błąd parsowania YAML w '{path}': {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("niepoprawna konfiguracja: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub model_list: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutingConfig {
    /// Timeout pojedynczego żądania non-stream (sekundy).
    #[serde(default = "default_request_timeout")]
    pub request_timeout_seconds: u64,
    /// Liczba błędów w oknie, po której deployment trafia do cooldownu.
    #[serde(default = "default_error_threshold")]
    pub error_threshold: u32,
    /// Długość okna zliczania błędów (sekundy).
    #[serde(default = "default_error_window")]
    pub error_window_seconds: u64,
    /// Jak długo deployment jest pomijany po przekroczeniu progu (sekundy).
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: u64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            request_timeout_seconds: default_request_timeout(),
            error_threshold: default_error_threshold(),
            error_window_seconds: default_error_window(),
            cooldown_seconds: default_cooldown(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub chat_path: String,
}

impl ProviderConfig {
    /// Pełny URL endpointu chat completions danego providera.
    pub fn chat_url(&self) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            self.chat_path.trim_start_matches('/')
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    pub model_name: String,
    #[serde(default)]
    pub deployments: Vec<Deployment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Deployment {
    /// Klucz w mapie `providers`.
    pub provider: String,
    /// Nazwa modelu tak, jak oczekuje jej provider.
    pub model: String,
    /// Nazwa zmiennej środowiskowej z kluczem API providera.
    pub api_key_env: String,
    /// Kolejność prób (rosnąco). Brak = 0.
    #[serde(default)]
    pub order: i64,
}

impl Deployment {
    /// Stabilny klucz deploymentu używany przez tracker zdrowia.
    pub fn health_key(&self) -> String {
        format!("{}|{}", self.provider, self.model)
    }
}

impl Config {
    /// Ustala ścieżkę configu: `$GATEWAY_CONFIG`, potem `./config.yaml`,
    /// a na końcu `config.yaml` obok binarki (przydatne przy uruchomieniu spoza repo).
    pub fn resolve_path() -> PathBuf {
        if let Ok(explicit) = std::env::var(CONFIG_PATH_ENV) {
            if !explicit.trim().is_empty() {
                return PathBuf::from(explicit);
            }
        }

        let cwd_candidate = PathBuf::from(DEFAULT_CONFIG_FILE);
        if cwd_candidate.is_file() {
            return cwd_candidate;
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let beside_exe = dir.join(DEFAULT_CONFIG_FILE);
                if beside_exe.is_file() {
                    return beside_exe;
                }
            }
        }

        cwd_candidate
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;

        let mut config: Config =
            serde_yaml_ng::from_str(&raw).map_err(|source| ConfigError::Parse {
                path: path.display().to_string(),
                source,
            })?;

        for entry in &mut config.model_list {
            entry.deployments.sort_by_key(|d| d.order);
        }

        config.validate()?;
        Ok(config)
    }

    /// Waliduje spójność configu. Brakujące klucze API to tylko ostrzeżenie —
    /// provider może być celowo nieużywany na tej maszynie.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.providers.is_empty() {
            return Err(ConfigError::Invalid(
                "sekcja 'providers' jest pusta".to_string(),
            ));
        }
        if self.model_list.is_empty() {
            return Err(ConfigError::Invalid(
                "sekcja 'model_list' jest pusta".to_string(),
            ));
        }

        for (name, provider) in &self.providers {
            if provider.base_url.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "provider '{name}': puste 'base_url'"
                )));
            }
            if !provider.base_url.starts_with("http://")
                && !provider.base_url.starts_with("https://")
            {
                return Err(ConfigError::Invalid(format!(
                    "provider '{name}': 'base_url' musi zaczynać się od http:// lub https://"
                )));
            }
            if provider.chat_path.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "provider '{name}': puste 'chat_path'"
                )));
            }
        }

        let mut seen_aliases: HashSet<&str> = HashSet::new();
        for entry in &self.model_list {
            if entry.model_name.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "model_list: puste 'model_name'".to_string(),
                ));
            }
            if !seen_aliases.insert(entry.model_name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "model_list: zduplikowany 'model_name': '{}'",
                    entry.model_name
                )));
            }
            if entry.deployments.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "model '{}': lista 'deployments' jest pusta",
                    entry.model_name
                )));
            }
            for deployment in &entry.deployments {
                if !self.providers.contains_key(&deployment.provider) {
                    return Err(ConfigError::Invalid(format!(
                        "model '{}': nieznany provider '{}' (brak w sekcji 'providers')",
                        entry.model_name, deployment.provider
                    )));
                }
                if deployment.model.trim().is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "model '{}': puste pole 'model' w deploymencie providera '{}'",
                        entry.model_name, deployment.provider
                    )));
                }
                if deployment.api_key_env.trim().is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "model '{}': puste 'api_key_env' w deploymencie providera '{}'",
                        entry.model_name, deployment.provider
                    )));
                }
            }
        }

        Ok(())
    }

    /// Loguje ostrzeżenia o brakujących w środowisku kluczach API (nie jest to błąd).
    pub fn warn_about_missing_api_keys(&self) {
        let mut missing: Vec<&str> = Vec::new();
        for entry in &self.model_list {
            for deployment in &entry.deployments {
                let present = std::env::var(&deployment.api_key_env)
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false);
                if !present && !missing.contains(&deployment.api_key_env.as_str()) {
                    missing.push(deployment.api_key_env.as_str());
                }
            }
        }
        for key in missing {
            tracing::warn!(
                api_key_env = key,
                "brak klucza API w środowisku — deploymenty go używające będą pomijane"
            );
        }
    }

    /// Zwraca wpis `model_list` dla danego aliasu.
    pub fn model_entry(&self, alias: &str) -> Option<&ModelEntry> {
        self.model_list.iter().find(|m| m.model_name == alias)
    }

    /// Zwraca konfigurację providera po nazwie.
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    pub fn request_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.routing.request_timeout_seconds.max(1))
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    4444
}

fn default_request_timeout() -> u64 {
    120
}

fn default_error_threshold() -> u32 {
    3
}

fn default_error_window() -> u64 {
    120
}

fn default_cooldown() -> u64 {
    60
}
