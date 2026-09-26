use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::config::{Config, ConfigError, Deployment, ModelEntry, ProviderConfig};
use crate::error::truncate;
use crate::protocol::openai::{reasoning_str, ChatCompletionChunk, ChunkChoice};
use crate::providers;

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
const LATENCY_TIMEOUT: Duration = Duration::from_secs(300);
const LATENCY_PROMPT_LINES: usize = 900;
const LATENCY_MAX_TOKENS: u32 = 200;
const LATENCY_WARN_RATIO: f64 = 0.7;
const LATENCY_SIGNIFICANT_RATIO: f64 = 0.8;
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

pub async fn run(env_file: Option<&Path>, check_providers: bool, check_latency: bool) -> bool {
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
        if check_providers || check_latency {
            let working = check_providers_live(&mut report, config).await;
            if check_latency {
                check_latency_live(&mut report, config, &working).await;
            } else {
                report.line(
                    Level::Info,
                    "szybkość: test pominięty — uruchom z --providers --latency, aby go wykonać",
                );
            }
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

async fn check_providers_live(report: &mut Report, config: &Config) -> HashSet<String> {
    let client = match reqwest::Client::builder()
        .user_agent(concat!("gateway-llm-doctor/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(config.connect_timeout())
        .timeout(PROVIDER_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            report.line(Level::Fail, format!("klient HTTP: {err}"));
            return HashSet::new();
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

    for deployment in &working {
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

    working
        .iter()
        .map(|deployment| deployment.health_key())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LatencyProbe {
    headers: Duration,
    first_token: Option<Duration>,
    total: Duration,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

async fn check_latency_live(report: &mut Report, config: &Config, working: &HashSet<String>) {
    let client = match reqwest::Client::builder()
        .user_agent(concat!("gateway-llm-doctor/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(config.connect_timeout())
        .timeout(LATENCY_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            report.line(Level::Fail, format!("klient HTTP: {err}"));
            return;
        }
    };

    let prompt = latency_prompt(LATENCY_PROMPT_LINES);
    report.line(
        Level::Info,
        format!(
            "szybkość: wysyłam duży prompt (~{} tys. znaków, jak zapytanie z Claude Code) do każdego działającego deploymentu — zużywa to limity providerów",
            prompt.len() / 1000
        ),
    );

    let header_limit = config.connect_timeout();
    let mut measured = HashSet::new();
    let mut results: HashMap<String, LatencyProbe> = HashMap::new();
    for deployment in config
        .model_list
        .iter()
        .flat_map(|entry| &entry.deployments)
    {
        let key = deployment.health_key();
        if !working.contains(&key) || !measured.insert(key.clone()) {
            continue;
        }
        let label = format!("{}/{}", deployment.provider, deployment.model);
        let (Some(provider), Some(api_key)) = (
            config.provider(&deployment.provider),
            providers::api_key_for(deployment),
        ) else {
            continue;
        };
        match probe_latency(
            &client,
            provider,
            &deployment.model,
            &api_key,
            deployment.stream_usage,
            &prompt,
        )
        .await
        {
            Ok(probe) => {
                let (level, message) = latency_verdict(&probe, header_limit);
                report.line(level, format!("{label}: {message}"));
                results.insert(key, probe);
            }
            Err(message) => report.line(Level::Fail, format!("{label}: szybkość: {message}")),
        }
    }

    for entry in &config.model_list {
        if let Some((level, message)) = order_advice(entry, &results) {
            report.line(level, message);
        }
    }
}

fn latency_prompt(lines: usize) -> String {
    let mut prompt = String::from(
        "Poniżej jest fragment kodu. Przeczytaj go, a potem wykonaj polecenie z końca wiadomości.\n\n",
    );
    for i in 0..lines {
        prompt.push_str(&format!(
            "fn handler_{i:04}(request: &Request, state: &State) -> Response {{ route(request, state, {i}) }}\n"
        ));
    }
    prompt.push_str(
        "\nPolecenie: odpowiedz jednym krótkim zdaniem, ile funkcji zdefiniowano powyżej.",
    );
    prompt
}

async fn probe_latency(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    model: &str,
    api_key: &str,
    stream_usage: bool,
    prompt: &str,
) -> Result<LatencyProbe, String> {
    let mut body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": format!("pomiar {}\n\n{prompt}", uuid::Uuid::new_v4())
        }],
        "max_tokens": LATENCY_MAX_TOKENS,
        "stream": true
    });
    if stream_usage {
        body["stream_options"] = json!({ "include_usage": true });
    }

    let started = Instant::now();
    let response = providers::build_request(client, provider, api_key, &body, None)
        .send()
        .await
        .map_err(|err| format!("błąd połączenia: {err}"))?;
    let headers = started.elapsed();
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {}: {}",
            status.as_u16(),
            error_message(&text)
        ));
    }

    let mut first_token = None;
    let mut usage = None;
    let mut events = Box::pin(response.bytes_stream().eventsource());
    while let Some(event) = events.next().await {
        let event = event.map_err(|err| format!("błąd odczytu strumienia: {err}"))?;
        let data = event.data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<ChatCompletionChunk>(data) else {
            continue;
        };
        if let Some(error) = &chunk.error {
            return Err(format!(
                "błąd w strumieniu: {}",
                truncate(error.to_string(), 200)
            ));
        }
        if first_token.is_none() && chunk.choices.iter().any(choice_has_output) {
            first_token = Some(started.elapsed());
        }
        if chunk.usage.is_some() {
            usage = chunk.usage;
        }
    }

    Ok(LatencyProbe {
        headers,
        first_token,
        total: started.elapsed(),
        prompt_tokens: usage.and_then(|usage| usage.prompt_tokens),
        completion_tokens: usage.and_then(|usage| usage.completion_tokens),
    })
}

fn choice_has_output(choice: &ChunkChoice) -> bool {
    let delta = &choice.delta;
    delta
        .content
        .as_deref()
        .is_some_and(|text| !text.is_empty())
        || delta.tool_calls.is_some()
        || reasoning_str(&delta.reasoning_content, &delta.reasoning).is_some()
}

fn latency_summary(probe: &LatencyProbe) -> String {
    let mut parts = vec![format!("nagłówki {:.1} s", probe.headers.as_secs_f64())];
    if let Some(first_token) = probe.first_token {
        parts.push(format!("pierwszy token {:.1} s", first_token.as_secs_f64()));
    }
    parts.push(format!("całość {:.1} s", probe.total.as_secs_f64()));
    if let Some(tokens) = probe.prompt_tokens {
        parts.push(format!("prompt {tokens} tok"));
    }
    if let (Some(first_token), Some(tokens)) = (probe.first_token, probe.completion_tokens) {
        let generating = probe.total.saturating_sub(first_token).as_secs_f64();
        if generating > 0.0 && tokens > 1 {
            parts.push(format!("{:.0} tok/s", f64::from(tokens) / generating));
        }
    }
    parts.join(", ")
}

fn latency_verdict(probe: &LatencyProbe, header_limit: Duration) -> (Level, String) {
    let summary = latency_summary(probe);
    let limit = header_limit.as_secs();
    let headers = probe.headers.as_secs_f64();
    if headers > header_limit.as_secs_f64() {
        let suggested = (headers * 1.5).ceil() as u64;
        return (
            Level::Fail,
            format!(
                "szybkość: {summary} — nagłówki przyszły później niż connect_timeout_seconds ({limit} s); w Claude Code ten deployment będzie odpadał, ustaw connect_timeout_seconds na co najmniej {suggested}"
            ),
        );
    }
    if headers > header_limit.as_secs_f64() * LATENCY_WARN_RATIO {
        return (
            Level::Warn,
            format!(
                "szybkość: {summary} — blisko limitu connect_timeout_seconds ({limit} s), przy większym prompcie deployment może odpadać"
            ),
        );
    }
    if probe.first_token.is_none() {
        return (
            Level::Warn,
            format!("szybkość: {summary} — model nie zwrócił żadnej treści"),
        );
    }
    (Level::Ok, format!("szybkość: {summary}"))
}

fn order_advice(
    entry: &ModelEntry,
    results: &HashMap<String, LatencyProbe>,
) -> Option<(Level, String)> {
    let measured: Vec<(&Deployment, Duration)> = entry
        .deployments
        .iter()
        .filter_map(|deployment| {
            results
                .get(&deployment.health_key())
                .and_then(|probe| probe.first_token)
                .map(|first_token| (deployment, first_token))
        })
        .collect();
    if measured.len() < 2 {
        return None;
    }

    let (first, first_time) = measured[0];
    let (fastest, fastest_time) = measured
        .iter()
        .copied()
        .min_by_key(|(_, first_token)| *first_token)?;
    let describe = |deployment: &Deployment, time: Duration| {
        format!(
            "{}/{} (order {}, {:.1} s)",
            deployment.provider,
            deployment.model,
            deployment.order,
            time.as_secs_f64()
        )
    };

    if std::ptr::eq(first, fastest) {
        return Some((
            Level::Ok,
            format!(
                "{}: kolejność zgodna z pomiarem — najszybszy jest pierwszy {}",
                entry.model_name,
                describe(first, first_time)
            ),
        ));
    }
    if fastest_time.as_secs_f64() >= first_time.as_secs_f64() * LATENCY_SIGNIFICANT_RATIO {
        return Some((
            Level::Ok,
            format!(
                "{}: {} jest niewiele szybszy od pierwszego {} — różnica w granicach błędu pomiaru, kolejność może zostać",
                entry.model_name,
                describe(fastest, fastest_time),
                describe(first, first_time)
            ),
        ));
    }
    Some((
        Level::Info,
        format!(
            "{}: najszybszy jest {}, a pierwszy w kolejności {} — rozważ zamianę order",
            entry.model_name,
            describe(fastest, fastest_time),
            describe(first, first_time)
        ),
    ))
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

    const SSE_LATENCY: &str = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"reasoning\":\"licze\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"900\"}}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":20000,\"completion_tokens\":40}}\n\ndata: [DONE]\n\n";
    const SSE_EMPTY: &str =
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\ndata: [DONE]\n\n";
    const SSE_STREAM_ERROR: &str =
        "data: {\"error\":{\"message\":\"overloaded\"}}\n\ndata: [DONE]\n\n";

    #[tokio::test]
    async fn latency_probe_measures_first_token_and_usage() {
        let provider = mock(200, "text/event-stream", SSE_LATENCY).await;
        let probe = probe_latency(&client(), &provider, "m", "k", true, "prompt")
            .await
            .unwrap();
        assert!(probe.first_token.is_some());
        assert!(probe.headers <= probe.total);
        assert_eq!(probe.prompt_tokens, Some(20000));
        assert_eq!(probe.completion_tokens, Some(40));
    }

    #[tokio::test]
    async fn latency_probe_reports_empty_stream_and_errors() {
        let empty = mock(200, "text/event-stream", SSE_EMPTY).await;
        let stream_error = mock(200, "text/event-stream", SSE_STREAM_ERROR).await;
        let rejected = mock(
            429,
            "application/json",
            r#"{"error":{"message":"rate limited"}}"#,
        )
        .await;

        let probe = probe_latency(&client(), &empty, "m", "k", false, "prompt")
            .await
            .unwrap();
        assert_eq!(probe.first_token, None);
        assert_eq!(probe.prompt_tokens, None);

        let err = probe_latency(&client(), &stream_error, "m", "k", false, "prompt")
            .await
            .unwrap_err();
        assert!(err.contains("overloaded"), "{err}");

        let err = probe_latency(&client(), &rejected, "m", "k", false, "prompt")
            .await
            .unwrap_err();
        assert!(err.contains("HTTP 429"), "{err}");
        assert!(err.contains("rate limited"), "{err}");
    }

    #[test]
    fn latency_prompt_has_requested_size_and_instruction() {
        let prompt = latency_prompt(900);
        assert_eq!(prompt.matches("fn handler_").count(), 900);
        assert!(prompt.len() > 60_000);
        assert!(prompt.ends_with("ile funkcji zdefiniowano powyżej."));
    }

    fn latency(headers: f64, first_token: Option<f64>) -> LatencyProbe {
        LatencyProbe {
            headers: Duration::from_secs_f64(headers),
            first_token: first_token.map(Duration::from_secs_f64),
            total: Duration::from_secs_f64(first_token.unwrap_or(headers) + 2.0),
            prompt_tokens: None,
            completion_tokens: None,
        }
    }

    #[test]
    fn latency_verdicts_compare_headers_with_connect_timeout() {
        let limit = Duration::from_secs(20);
        assert_eq!(
            latency_verdict(&latency(2.0, Some(3.0)), limit).0,
            Level::Ok
        );
        assert_eq!(
            latency_verdict(&latency(16.0, Some(17.0)), limit).0,
            Level::Warn
        );
        let (level, message) = latency_verdict(&latency(30.0, Some(31.0)), limit);
        assert_eq!(level, Level::Fail);
        assert!(message.contains("co najmniej 45"), "{message}");
        assert_eq!(latency_verdict(&latency(2.0, None), limit).0, Level::Warn);
    }

    #[test]
    fn latency_summary_reports_generation_speed() {
        let probe = LatencyProbe {
            headers: Duration::from_millis(500),
            first_token: Some(Duration::from_secs(3)),
            total: Duration::from_secs(5),
            prompt_tokens: Some(20000),
            completion_tokens: Some(100),
        };
        assert_eq!(
            latency_summary(&probe),
            "nagłówki 0.5 s, pierwszy token 3.0 s, całość 5.0 s, prompt 20000 tok, 50 tok/s"
        );
    }

    fn deployment(model: &str, order: i64) -> Deployment {
        Deployment {
            provider: "infron".to_string(),
            model: model.to_string(),
            api_key_env: "K".to_string(),
            order,
            stream_usage: false,
            show_reasoning: false,
        }
    }

    fn entry(deployments: Vec<Deployment>) -> ModelEntry {
        ModelEntry {
            model_name: "cc-main".to_string(),
            deployments,
            fallback_model: None,
        }
    }

    fn results(times: &[(&str, f64)]) -> HashMap<String, LatencyProbe> {
        times
            .iter()
            .map(|(model, time)| (deployment(model, 0).health_key(), latency(0.5, Some(*time))))
            .collect()
    }

    #[test]
    fn order_advice_suggests_swap_only_for_a_clearly_faster_deployment() {
        let alias = entry(vec![deployment("slow", 1), deployment("fast", 2)]);

        let (level, message) =
            order_advice(&alias, &results(&[("slow", 8.0), ("fast", 3.0)])).unwrap();
        assert_eq!(level, Level::Info);
        assert!(
            message.contains("najszybszy jest infron/fast (order 2"),
            "{message}"
        );

        let (level, message) =
            order_advice(&alias, &results(&[("slow", 3.0), ("fast", 8.0)])).unwrap();
        assert_eq!(level, Level::Ok);
        assert!(message.contains("kolejność zgodna"), "{message}");

        let (level, message) =
            order_advice(&alias, &results(&[("slow", 3.3), ("fast", 3.0)])).unwrap();
        assert_eq!(level, Level::Ok);
        assert!(message.contains("błędu pomiaru"), "{message}");

        assert!(order_advice(&alias, &results(&[("slow", 3.0)])).is_none());
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
