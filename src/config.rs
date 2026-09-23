use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_CONFIG_FILE: &str = "config.yaml";

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
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_stream_idle_timeout")]
    pub stream_idle_timeout_seconds: u64,
    #[serde(default = "default_non_stream_timeout")]
    pub non_stream_timeout_seconds: u64,
    #[serde(default = "default_error_threshold")]
    pub error_threshold: u32,
    #[serde(default = "default_error_window")]
    pub error_window_seconds: u64,
    #[serde(default = "default_cooldown")]
    pub cooldown_seconds: u64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            connect_timeout_seconds: default_connect_timeout(),
            stream_idle_timeout_seconds: default_stream_idle_timeout(),
            non_stream_timeout_seconds: default_non_stream_timeout(),
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
    #[serde(default)]
    pub rpm: Option<u32>,
    #[serde(default)]
    pub stream_usage: bool,
}

impl ProviderConfig {
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
    #[serde(default)]
    pub fallback_model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Deployment {
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
    #[serde(default)]
    pub order: i64,
}

impl Deployment {
    pub fn health_key(&self) -> String {
        format!("{}|{}", self.provider, self.model)
    }
}

impl Config {
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
            if provider.rpm == Some(0) {
                return Err(ConfigError::Invalid(format!(
                    "provider '{name}': 'rpm' nie może wynosić 0 (usuń pole, żeby wyłączyć limit)"
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

        for entry in &self.model_list {
            let Some(fallback) = entry.fallback_model.as_deref() else {
                continue;
            };
            if fallback.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "model '{}': puste 'fallback_model'",
                    entry.model_name
                )));
            }
            if fallback == entry.model_name {
                return Err(ConfigError::Invalid(format!(
                    "model '{}': 'fallback_model' wskazuje sam na siebie",
                    entry.model_name
                )));
            }
            if !seen_aliases.contains(fallback) {
                return Err(ConfigError::Invalid(format!(
                    "model '{}': nieznany 'fallback_model' '{fallback}' (brak takiego 'model_name')",
                    entry.model_name
                )));
            }
        }

        for entry in &self.model_list {
            let mut chain: HashSet<&str> = HashSet::new();
            chain.insert(entry.model_name.as_str());
            let mut current = entry.model_name.as_str();
            while let Some(next) = self
                .model_entry(current)
                .and_then(|e| e.fallback_model.as_deref())
            {
                if !chain.insert(next) {
                    return Err(ConfigError::Invalid(format!(
                        "model_list: cykl w 'fallback_model' zaczynający się od '{}'",
                        entry.model_name
                    )));
                }
                current = next;
            }
        }

        Ok(())
    }

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

    pub fn model_entry(&self, alias: &str) -> Option<&ModelEntry> {
        self.model_list.iter().find(|m| m.model_name == alias)
    }

    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    pub fn connect_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.routing.connect_timeout_seconds.max(1))
    }

    pub fn stream_idle_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.routing.stream_idle_timeout_seconds.max(1))
    }

    pub fn non_stream_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.routing.non_stream_timeout_seconds.max(1))
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    4444
}

fn default_connect_timeout() -> u64 {
    20
}

fn default_stream_idle_timeout() -> u64 {
    90
}

fn default_non_stream_timeout() -> u64 {
    300
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

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> ProviderConfig {
        ProviderConfig {
            base_url: "https://example.test".to_string(),
            chat_path: "/chat/completions".to_string(),
            rpm: None,
            stream_usage: false,
        }
    }

    fn deployment() -> Deployment {
        Deployment {
            provider: "p".to_string(),
            model: "upstream-model".to_string(),
            api_key_env: "SOME_KEY".to_string(),
            order: 0,
        }
    }

    fn config_with(model_list: Vec<ModelEntry>) -> Config {
        let mut providers = HashMap::new();
        providers.insert("p".to_string(), provider());
        Config {
            server: ServerConfig::default(),
            routing: RoutingConfig::default(),
            providers,
            model_list,
        }
    }

    fn entry(name: &str, fallback: Option<&str>) -> ModelEntry {
        ModelEntry {
            model_name: name.to_string(),
            deployments: vec![deployment()],
            fallback_model: fallback.map(str::to_string),
        }
    }

    #[test]
    fn fallback_model_pointing_at_known_alias_is_valid() {
        let config = config_with(vec![entry("main", Some("backup")), entry("backup", None)]);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn fallback_model_pointing_at_unknown_alias_is_rejected() {
        let config = config_with(vec![entry("main", Some("ghost"))]);
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("nieznany 'fallback_model'"), "{err}");
    }

    #[test]
    fn fallback_model_cannot_point_at_itself() {
        let config = config_with(vec![entry("main", Some("main"))]);
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("wskazuje sam na siebie"), "{err}");
    }

    #[test]
    fn fallback_model_cycle_is_rejected() {
        let config = config_with(vec![entry("a", Some("b")), entry("b", Some("a"))]);
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("cykl w 'fallback_model'"), "{err}");
    }

    #[test]
    fn fallback_model_chain_of_three_is_valid() {
        let config = config_with(vec![
            entry("a", Some("b")),
            entry("b", Some("c")),
            entry("c", None),
        ]);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn provider_rpm_zero_is_rejected() {
        let mut config = config_with(vec![entry("main", None)]);
        config.providers.get_mut("p").unwrap().rpm = Some(0);
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("'rpm' nie może wynosić 0"), "{err}");
    }

    #[test]
    fn provider_rpm_missing_or_positive_is_valid() {
        let mut config = config_with(vec![entry("main", None)]);
        assert!(config.validate().is_ok());
        config.providers.get_mut("p").unwrap().rpm = Some(20);
        assert!(config.validate().is_ok());
    }
}
