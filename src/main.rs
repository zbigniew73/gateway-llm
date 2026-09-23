use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use gateway_llm::config::Config;
use gateway_llm::router::Router;
use gateway_llm::state::AppState;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "użycie: gateway-llm [doctor [--providers]]

  (bez argumentów)     uruchamia gateway
  doctor               sprawdza instalację, konfigurację i działającą usługę
  doctor --providers   dodatkowo testuje każdy deployment jednym małym żądaniem";

#[tokio::main]
async fn main() -> ExitCode {
    let env_file = dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None => {}
        Some("doctor") => {
            let flags = &args[1..];
            if let Some(unknown) = flags.iter().find(|flag| *flag != "--providers") {
                eprintln!("nieznana opcja doctor: {unknown}\n\n{USAGE}");
                return ExitCode::from(2);
            }
            let check_providers = !flags.is_empty();
            return if gateway_llm::doctor::run(env_file.as_deref(), check_providers).await {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("nieznana komenda: {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    }

    init_tracing();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            tracing::error!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("gateway_llm=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

async fn run() -> Result<(), String> {
    let config_path = Config::resolve_path();
    let config = Config::load(&config_path).map_err(|err| err.to_string())?;
    tracing::info!(
        config = %config_path.display(),
        models = config.model_list.len(),
        providers = config.providers.len(),
        "konfiguracja wczytana"
    );
    config.warn_about_missing_api_keys();

    let gateway_api_key = std::env::var("GATEWAY_API_KEY")
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            "brak zmiennej środowiskowej GATEWAY_API_KEY (skopiuj .env.example do .env \
             i ustaw długi losowy klucz)"
                .to_string()
        })?;

    let config = Arc::new(config);
    let router = Router::new(config.clone()).map_err(|err| err.to_string())?;
    let state = AppState::new(config.clone(), Arc::new(router), gateway_api_key);

    let app = gateway_llm::build_app(state);

    let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port)
        .parse()
        .map_err(|err| {
            format!(
                "niepoprawny adres nasłuchu '{}:{}': {err}",
                config.server.host, config.server.port
            )
        })?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| format!("nie udało się zająć portu {addr}: {err}"))?;

    tracing::info!(%addr, "gateway-llm nasłuchuje");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|err| format!("serwer zakończył się błędem: {err}"))?;

    tracing::info!("gateway-llm zatrzymany");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "nie udało się podpiąć obsługi Ctrl+C");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "nie udało się podpiąć obsługi SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("otrzymano sygnał zatrzymania — kończę pracę");
}
