use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::config::{Config, ConfigError, Deployment, ProviderConfig};
use crate::error::truncate;
use crate::protocol::openai::{reasoning_str, ChatCompletionChunk};
use crate::providers;

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
const LOCAL_TIMEOUT: Duration = Duration::from_secs(5);
const SERVICE_NAME: &str = "gateway-llm.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

#[derive(Default)]
struct Report {
    ok: usize,
    warn: usize,
    fail: usize,
}

impl Report {
    fn line(&mut self, level: Level, message: impl AsRef<str>) {
        let tag = match level {
            Level::Ok => {
                self.ok += 1;
                "[ OK ]"
            }
            Level::Info => "[INFO]",
            Level::Warn => {
                self.warn += 1;
                "[WARN]"
            }
            Level::Fail => {
                self.fail += 1;
                "[FAIL]"
            }
        };
        println!("{tag} {}", message.as_ref());
    }
}

pub async fn run(env_file: Option<&Path>, check_providers: bool) -> bool {
    println!("gateway-llm doctor {}\n", env!("CARGO_PKG_VERSION"));
    let mut report = Report::default();

    check_env_file(&mut report, env_file);
    let config = check_config(&mut report);
    let gateway_key = check_gateway_key(&mut report);
    if let Some(config) = &config {
        check_provider_keys(&mut report, config);
    }
    check_service(&mut report);

    if let Some(config) = &config {
        check_running(&mut report, config, gateway_key.as_deref()).await;
        if check_providers {
            check_providers_live(&mut report, config).await;
        } else {
            report.line(
                Level::Info,
                "providerzy: test pominięty — uruchom z --providers, aby go wykonać",
            );
        }
    }

    println!(
        "\npodsumowanie: {} OK, {} WARN, {} FAIL",
        report.ok, report.warn, report.fail
    );
    report.fail == 0
}

fn check_env_file(report: &mut Report, env_file: Option<&Path>) {
    let Some(path) = env_file else {
        report.line(
            Level::Info,
            ".env: brak w bieżącym katalogu — zmienne muszą pochodzić z otoczenia",
        );
        return;
    };
    match file_mode(path) {
        Some(mode) if is_private_mode(mode) => report.line(
            Level::Ok,
            format!(".env: {} (uprawnienia {:o})", path.display(), mode & 0o777),
        ),
        Some(mode) => report.line(
            Level::Warn,
            format!(
                ".env: {} ma uprawnienia {:o} — ustaw: chmod 600 {}",
                path.display(),
                mode & 0o777,
                path.display()
            ),
        ),
        None => report.line(Level::Ok, format!(".env: {}", path.display())),
    }
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode())
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

fn is_private_mode(mode: u32) -> bool {
    mode & 0o077 == 0
}

fn check_config(report: &mut Report) -> Option<Config> {
    let path = Config::resolve_path();
    match Config::load(&path) {
        Ok(config) => {
            report.line(
                Level::Ok,
                format!(
                    "config: {} (aliasy: {}, providerzy: {})",
                    path.display(),
                    config.model_list.len(),
                    config.providers.len()
                ),
            );
            Some(config)
        }
        Err(err) => {
            report.line(
                Level::Fail,
                match err {
                    ConfigError::Read { .. } => format!(
                        "config: {err} — uruchom z katalogu gateway-llm albo ustaw GATEWAY_CONFIG"
                    ),
                    _ => format!("config: {err}"),
                },
            );
            None
        }
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn check_gateway_key(report: &mut Report) -> Option<String> {
    let key = env_value("GATEWAY_API_KEY");
    match &key {
        Some(_) => report.line(Level::Ok, "GATEWAY_API_KEY: ustawiony"),
        None => report.line(
            Level::Fail,
            "GATEWAY_API_KEY: brak — gateway nie wystartuje",
        ),
    }
    key
}

fn check_provider_keys(report: &mut Report, config: &Config) {
    let mut seen = HashSet::new();
    for deployment in config
        .model_list
        .iter()
        .flat_map(|entry| &entry.deployments)
    {
        if !seen.insert(deployment.api_key_env.as_str()) {
            continue;
        }
        if env_value(&deployment.api_key_env).is_some() {
            report.line(Level::Ok, format!("{}: ustawiony", deployment.api_key_env));
        } else {
            report.line(
                Level::Warn,
                format!(
                    "{}: brak — deploymenty z tym kluczem będą pomijane",
                    deployment.api_key_env
                ),
            );
        }
    }
}

fn check_service(report: &mut Report) {
    match Command::new("systemctl")
        .args(["--user", "is-active", SERVICE_NAME])
        .output()
    {
        Ok(output) => {
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if state == "active" {
                report.line(Level::Ok, "usługa systemd: active");
            } else {
                let state = if state.is_empty() {
                    "nieznany stan"
                } else {
                    state.as_str()
                };
                report.line(
                    Level::Warn,
                    format!(
                        "usługa systemd: {state} — sprawdź: systemctl --user status gateway-llm"
                    ),
                );
            }
        }
        Err(_) => {
            report.line(
                Level::Info,
                "usługa systemd: systemctl niedostępny — pomijam",
            );
            return;
        }
    }

    let Some(user) = env_value("USER") else {
        report.line(Level::Info, "linger: nieznany użytkownik — pomijam");
        return;
    };
    match Command::new("loginctl")
        .args(["show-user", &user, "-p", "Linger"])
        .output()
    {
        Ok(output) if String::from_utf8_lossy(&output.stdout).trim() == "Linger=yes" => {
            report.line(Level::Ok, "linger: włączony")
        }
        Ok(_) => report.line(
            Level::Warn,
            format!(
                "linger: wyłączony — usługa zatrzyma się po wylogowaniu (loginctl enable-linger {user})"
            ),
        ),
        Err(_) => report.line(Level::Info, "linger: loginctl niedostępny — pomijam"),
    }
}

fn gateway_base_url(host: &str, port: u16) -> String {
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    };
    if host.contains(':') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

async fn check_running(report: &mut Report, config: &Config, gateway_key: Option<&str>) {
    let base = gateway_base_url(&config.server.host, config.server.port);
    let client = match reqwest::Client::builder().timeout(LOCAL_TIMEOUT).build() {
        Ok(client) => client,
        Err(err) => {
            report.line(Level::Fail, format!("klient HTTP: {err}"));
            return;
        }
    };

    match client.get(format!("{base}/healthz")).send().await {
        Ok(response) if response.status().is_success() => {
            let version = response
                .json::<Value>()
                .await
                .ok()
                .and_then(|body| {
                    body.get("version")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "?".to_string());
            report.line(
                Level::Ok,
                format!("gateway odpowiada: {base} (wersja {version})"),
            );
        }
        Ok(response) => {
            report.line(
                Level::Fail,
                format!(
                    "gateway: {base}/healthz zwrócił HTTP {}",
                    response.status().as_u16()
                ),
            );
            return;
        }
        Err(err) => {
            report.line(
                Level::Fail,
                format!("gateway nie odpowiada na {base}: {err}"),
            );
            return;
        }
    }

    let Some(key) = gateway_key else {
        return;
    };
    let result = client
        .post(format!("{base}/v1/messages/count_tokens"))
        .header("x-api-key", key)
        .json(&json!({ "model": "doctor", "messages": [{ "role": "user", "content": "ping" }] }))
        .send()
        .await;
    match result {
        Ok(response) if response.status().is_success() => report.line(
            Level::Ok,
            "autoryzacja: działająca usługa akceptuje GATEWAY_API_KEY",
        ),
        Ok(response) if response.status().as_u16() == 401 => report.line(
            Level::Fail,
            "autoryzacja: działająca usługa odrzuca GATEWAY_API_KEY — po zmianie .env zrestartuj usługę",
        ),
        Ok(response) => report.line(
            Level::Warn,
            format!(
                "autoryzacja: nieoczekiwany HTTP {}",
                response.status().as_u16()
            ),
        ),
        Err(err) => report.line(Level::Warn, format!("autoryzacja: {err}")),
    }
}

async fn check_providers_live(report: &mut Report, config: &Config) {
    let client = match reqwest::Client::builder()
        .user_agent(concat!("gateway-llm-doctor/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(config.connect_timeout())
        .timeout(PROVIDER_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            report.line(Level::Fail, format!("klient HTTP: {err}"));
            return;
        }
    };

    let mut probed = HashSet::new();
    let mut working: Vec<&Deployment> = Vec::new();
    for deployment in config
        .model_list
        .iter()
        .flat_map(|entry| &entry.deployments)
    {
        if !probed.insert(deployment.health_key()) {
            continue;
        }
        let label = format!("{}/{}", deployment.provider, deployment.model);
        let Some(provider) = config.provider(&deployment.provider) else {
            continue;
        };
        let Some(api_key) = providers::api_key_for(deployment) else {
            report.line(
                Level::Warn,
                format!("{label}: pominięty — brak {}", deployment.api_key_env),
            );
            continue;
        };
        match probe_deployment(&client, provider, &deployment.model, &api_key).await {
            Ok(latency) => {
                report.line(
                    Level::Ok,
                    format!("{label}: odpowiada ({} ms)", latency.as_millis()),
                );
                working.push(deployment);
            }
            Err(message) => report.line(Level::Fail, format!("{label}: {message}")),
        }
    }

    for deployment in working {
        let (Some(provider), Some(api_key)) = (
            config.provider(&deployment.provider),
            providers::api_key_for(deployment),
        ) else {
            continue;
        };
        let probe = probe_stream_usage(&client, provider, &deployment.model, &api_key).await;
        let (level, message) = stream_usage_verdict(deployment.stream_usage, &probe);
        report.line(
            level,
            format!("{}/{}: {message}", deployment.provider, deployment.model),
        );

        let probe = probe_reasoning(&client, provider, &deployment.model, &api_key).await;
        let (level, message) = show_reasoning_verdict(deployment.show_reasoning, &probe);
        report.line(
            level,
            format!("{}/{}: {message}", deployment.provider, deployment.model),
        );
    }
}

async fn probe_deployment(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    model: &str,
    api_key: &str,
) -> Result<Duration, String> {
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": 5,
        "stream": false
    });
    let started = Instant::now();
    let response = providers::build_request(client, provider, api_key, &body, None)
        .send()
        .await
        .map_err(|err| format!("błąd połączenia: {err}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(started.elapsed());
    }
    let text = response.text().await.unwrap_or_default();
    Err(format!(
        "HTTP {}: {}",
        status.as_u16(),
        error_message(&text)
    ))
}

fn error_message(body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let message = parsed.as_ref().and_then(|value| {
        value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
    });
    truncate(message.unwrap_or(body).trim().to_string(), 200)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamUsageProbe {
    Supported,
    Ignored,
    Rejected(u16),
    Error(String),
}

async fn probe_stream_usage(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    model: &str,
    api_key: &str,
) -> StreamUsageProbe {
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": 5,
        "stream": true,
        "stream_options": { "include_usage": true }
    });
    let response = match providers::build_request(client, provider, api_key, &body, None)
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return StreamUsageProbe::Error(err.to_string()),
    };
    let status = response.status().as_u16();
    match status {
        200..=299 => {}
        400 | 422 => return StreamUsageProbe::Rejected(status),
        other => return StreamUsageProbe::Error(format!("HTTP {other}")),
    }
    match response.text().await {
        Ok(text) if sse_has_usage(&text) => StreamUsageProbe::Supported,
        Ok(_) => StreamUsageProbe::Ignored,
        Err(err) => StreamUsageProbe::Error(err.to_string()),
    }
}

fn sse_has_usage(text: &str) -> bool {
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| *data != "[DONE]")
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .any(|chunk| chunk.get("usage").is_some_and(Value::is_object))
}

fn stream_usage_verdict(enabled: bool, probe: &StreamUsageProbe) -> (Level, String) {
    match (probe, enabled) {
        (StreamUsageProbe::Supported, true) => {
            (Level::Ok, "stream_usage: obsługiwane i włączone".to_string())
        }
        (StreamUsageProbe::Supported, false) => (
            Level::Info,
            "stream_usage: provider obsługuje — możesz ustawić stream_usage: true".to_string(),
        ),
        (StreamUsageProbe::Ignored, true) => (
            Level::Warn,
            "stream_usage: provider ignoruje stream_options — stream_usage: true nic nie daje"
                .to_string(),
        ),
        (StreamUsageProbe::Ignored, false) => (
            Level::Ok,
            "stream_usage: provider nie zwraca usage, wyłączone — poprawnie".to_string(),
        ),
        (StreamUsageProbe::Rejected(status), true) => (
            Level::Fail,
            format!(
                "stream_usage: provider odrzuca stream_options (HTTP {status}) — ustaw stream_usage: false"
            ),
        ),
        (StreamUsageProbe::Rejected(status), false) => (
            Level::Ok,
            format!(
                "stream_usage: provider odrzuca stream_options (HTTP {status}), wyłączone — poprawnie"
            ),
        ),
        (StreamUsageProbe::Error(err), _) => (
            Level::Warn,
            format!("stream_usage: nie udało się sprawdzić ({err})"),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReasoningProbe {
    Separate,
    InlineTags,
    Absent,
    Error(String),
}

async fn probe_reasoning(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    model: &str,
    api_key: &str,
) -> ReasoningProbe {
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "Ile to 17 * 23? Podaj tylko wynik." }],
        "max_tokens": 300,
        "stream": true
    });
    let response = match providers::build_request(client, provider, api_key, &body, None)
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return ReasoningProbe::Error(err.to_string()),
    };
    let status = response.status().as_u16();
    if !(200..=299).contains(&status) {
        return ReasoningProbe::Error(format!("HTTP {status}"));
    }

    let mut events = Box::pin(response.bytes_stream().eventsource());
    while let Some(event) = events.next().await {
        let event = match event {
            Ok(event) => event,
            Err(err) => return ReasoningProbe::Error(err.to_string()),
        };
        let data = event.data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<ChatCompletionChunk>(data) else {
            continue;
        };
        for choice in &chunk.choices {
            if reasoning_str(&choice.delta.reasoning_content, &choice.delta.reasoning).is_some() {
                return ReasoningProbe::Separate;
            }
            if let Some(text) = choice
                .delta
                .content
                .as_deref()
                .map(str::trim_start)
                .filter(|text| !text.is_empty())
            {
                return if text.starts_with("<think>") {
                    ReasoningProbe::InlineTags
                } else {
                    ReasoningProbe::Absent
                };
            }
        }
    }
    ReasoningProbe::Absent
}

fn show_reasoning_verdict(enabled: bool, probe: &ReasoningProbe) -> (Level, String) {
    match (probe, enabled) {
        (ReasoningProbe::Separate, true) => (
            Level::Ok,
            "show_reasoning: model zwraca myślenie i jest włączone".to_string(),
        ),
        (ReasoningProbe::Separate, false) => (
            Level::Info,
            "show_reasoning: model zwraca myślenie — możesz ustawić show_reasoning: true"
                .to_string(),
        ),
        (ReasoningProbe::Absent, true) => (
            Level::Warn,
            "show_reasoning: model nie zwraca myślenia (reasoning_content/reasoning) — show_reasoning: true nic nie daje"
                .to_string(),
        ),
        (ReasoningProbe::Absent, false) => (
            Level::Ok,
            "show_reasoning: model nie zwraca myślenia, wyłączone — poprawnie".to_string(),
        ),
        (ReasoningProbe::InlineTags, _) => (
            Level::Warn,
            "show_reasoning: model wpisuje myślenie w treść (<think>) — trafi do Claude Code jako zwykły tekst"
                .to_string(),
        ),
        (ReasoningProbe::Error(err), _) => (
            Level::Warn,
            format!("show_reasoning: nie udało się sprawdzić ({err})"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn mock(status: u16, content_type: &'static str, body: &'static str) -> ProviderConfig {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        ProviderConfig {
            base_url: format!("http://127.0.0.1:{port}"),
            chat_path: "/v1/chat/completions".to_string(),
            rpm: None,
            headers: Default::default(),
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    const SSE_WITH_USAGE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n";
    const SSE_WITHOUT_USAGE: &str =
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";

    const SSE_REASONING: &str = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"reasoning_content\":\"17*23\"}}]}\n\ndata: [DONE]\n\n";
    const SSE_OPENROUTER_REASONING: &str =
        "data: {\"choices\":[{\"delta\":{\"reasoning\":\"17*23\"}}]}\n\ndata: [DONE]\n\n";
    const SSE_THINK_TAGS: &str =
        "data: {\"choices\":[{\"delta\":{\"content\":\"<think>17*23\"}}]}\n\ndata: [DONE]\n\n";

    #[tokio::test]
    async fn reasoning_probe_detects_separate_inline_and_absent() {
        let separate = mock(200, "text/event-stream", SSE_REASONING).await;
        let openrouter = mock(200, "text/event-stream", SSE_OPENROUTER_REASONING).await;
        let inline = mock(200, "text/event-stream", SSE_THINK_TAGS).await;
        let absent = mock(200, "text/event-stream", SSE_WITHOUT_USAGE).await;
        let failing = mock(500, "application/json", "{}").await;

        assert_eq!(
            probe_reasoning(&client(), &separate, "m", "k").await,
            ReasoningProbe::Separate
        );
        assert_eq!(
            probe_reasoning(&client(), &openrouter, "m", "k").await,
            ReasoningProbe::Separate
        );
        assert_eq!(
            probe_reasoning(&client(), &inline, "m", "k").await,
            ReasoningProbe::InlineTags
        );
        assert_eq!(
            probe_reasoning(&client(), &absent, "m", "k").await,
            ReasoningProbe::Absent
        );
        assert_eq!(
            probe_reasoning(&client(), &failing, "m", "k").await,
            ReasoningProbe::Error("HTTP 500".to_string())
        );
    }

    #[test]
    fn show_reasoning_verdicts_match_config() {
        assert_eq!(
            show_reasoning_verdict(true, &ReasoningProbe::Separate).0,
            Level::Ok
        );
        assert_eq!(
            show_reasoning_verdict(false, &ReasoningProbe::Separate).0,
            Level::Info
        );
        assert_eq!(
            show_reasoning_verdict(true, &ReasoningProbe::Absent).0,
            Level::Warn
        );
        assert_eq!(
            show_reasoning_verdict(false, &ReasoningProbe::Absent).0,
            Level::Ok
        );
        assert_eq!(
            show_reasoning_verdict(false, &ReasoningProbe::InlineTags).0,
            Level::Warn
        );
    }

    #[tokio::test]
    async fn working_deployment_is_reported_ok() {
        let provider = mock(200, "application/json", r#"{"choices":[]}"#).await;
        assert!(probe_deployment(&client(), &provider, "m", "k")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn failing_deployment_reports_status_and_provider_message() {
        let provider = mock(
            401,
            "application/json",
            r#"{"error":{"message":"invalid api key"}}"#,
        )
        .await;
        let err = probe_deployment(&client(), &provider, "m", "k")
            .await
            .unwrap_err();
        assert!(err.contains("HTTP 401"), "{err}");
        assert!(err.contains("invalid api key"), "{err}");
    }

    #[tokio::test]
    async fn stream_probe_detects_supported_ignored_and_rejected() {
        let supported = mock(200, "text/event-stream", SSE_WITH_USAGE).await;
        let ignored = mock(200, "text/event-stream", SSE_WITHOUT_USAGE).await;
        let rejected = mock(
            400,
            "application/json",
            r#"{"error":{"message":"unknown field"}}"#,
        )
        .await;
        let overloaded = mock(503, "application/json", "{}").await;

        assert_eq!(
            probe_stream_usage(&client(), &supported, "m", "k").await,
            StreamUsageProbe::Supported
        );
        assert_eq!(
            probe_stream_usage(&client(), &ignored, "m", "k").await,
            StreamUsageProbe::Ignored
        );
        assert_eq!(
            probe_stream_usage(&client(), &rejected, "m", "k").await,
            StreamUsageProbe::Rejected(400)
        );
        assert!(matches!(
            probe_stream_usage(&client(), &overloaded, "m", "k").await,
            StreamUsageProbe::Error(_)
        ));
    }

    #[test]
    fn usage_is_detected_only_as_an_object() {
        assert!(sse_has_usage(SSE_WITH_USAGE));
        assert!(!sse_has_usage(SSE_WITHOUT_USAGE));
        assert!(!sse_has_usage("data: {\"choices\":[],\"usage\":null}\n\n"));
        assert!(!sse_has_usage("data: nie-json\n\n"));
    }

    #[test]
    fn stream_usage_verdicts_match_config() {
        assert_eq!(
            stream_usage_verdict(true, &StreamUsageProbe::Supported).0,
            Level::Ok
        );
        assert_eq!(
            stream_usage_verdict(false, &StreamUsageProbe::Supported).0,
            Level::Info
        );
        assert_eq!(
            stream_usage_verdict(true, &StreamUsageProbe::Ignored).0,
            Level::Warn
        );
        assert_eq!(
            stream_usage_verdict(true, &StreamUsageProbe::Rejected(400)).0,
            Level::Fail
        );
        assert_eq!(
            stream_usage_verdict(false, &StreamUsageProbe::Rejected(400)).0,
            Level::Ok
        );
    }

    #[test]
    fn env_file_must_not_be_readable_by_others() {
        assert!(is_private_mode(0o100600));
        assert!(!is_private_mode(0o100644));
        assert!(!is_private_mode(0o100640));
    }

    #[test]
    fn gateway_url_maps_wildcard_hosts_to_loopback() {
        assert_eq!(gateway_base_url("127.0.0.1", 4444), "http://127.0.0.1:4444");
        assert_eq!(gateway_base_url("0.0.0.0", 4444), "http://127.0.0.1:4444");
        assert_eq!(gateway_base_url("::", 4444), "http://[::1]:4444");
    }

    #[test]
    fn provider_error_message_is_extracted_and_truncated() {
        assert_eq!(
            error_message(r#"{"error":{"message":"bad key"}}"#),
            "bad key"
        );
        assert_eq!(error_message(r#"{"message":"quota"}"#), "quota");
        assert_eq!(error_message("plain text"), "plain text");
        assert!(error_message(&"x".repeat(500)).chars().count() < 250);
    }
}
