//! Timeouty routera na prawdziwym (lokalnym, celowo wolnym) serwerze HTTP.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gateway_llm::config::{
    Config, Deployment, ModelEntry, ProviderConfig, RoutingConfig, ServerConfig,
};
use gateway_llm::router::Router;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serwer, który odsyła poprawną odpowiedź chat completions dopiero po `delay`
/// — tak jak provider non-stream, który odpowiada po wygenerowaniu całości.
async fn slow_server(delay: Duration) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        let _ = socket.read(&mut buf).await;
        tokio::time::sleep(delay).await;
        let body = r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
    });
    port
}

fn config(port: u16, api_key_env: &str, connect: u64, non_stream: u64) -> Config {
    let mut providers = HashMap::new();
    providers.insert(
        "local".to_string(),
        ProviderConfig {
            base_url: format!("http://127.0.0.1:{port}"),
            chat_path: "/v1/chat/completions".to_string(),
            rpm: None,
            stream_usage: false,
        },
    );
    Config {
        server: ServerConfig::default(),
        routing: RoutingConfig {
            connect_timeout_seconds: connect,
            non_stream_timeout_seconds: non_stream,
            ..RoutingConfig::default()
        },
        providers,
        model_list: vec![ModelEntry {
            model_name: "m".to_string(),
            deployments: vec![Deployment {
                provider: "local".to_string(),
                model: "upstream".to_string(),
                api_key_env: api_key_env.to_string(),
                order: 0,
            }],
            fallback_model: None,
        }],
    }
}

fn body() -> serde_json::Value {
    json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] })
}

#[tokio::test]
async fn slow_non_stream_response_is_not_cut_by_connect_timeout() {
    // Odpowiedź po 2 s przy connect_timeout = 1 s: przed poprawką connect_timeout
    // był pełnym timeoutem non-stream i to żądanie zostałoby ucięte.
    let env = "GW_TEST_KEY_SLOW_OK";
    std::env::set_var(env, "k");
    let port = slow_server(Duration::from_secs(2)).await;
    let router = Router::new(Arc::new(config(port, env, 1, 10))).unwrap();

    let dispatched = router
        .dispatch("m", &body(), false, false, "test")
        .await
        .expect("wolna, ale poprawna odpowiedź non-stream musi przejść");
    assert!(dispatched.response.status().is_success());
}

#[tokio::test]
async fn non_stream_timeout_still_bounds_the_request() {
    let env = "GW_TEST_KEY_SLOW_TIMEOUT";
    std::env::set_var(env, "k");
    let port = slow_server(Duration::from_secs(3)).await;
    let router = Router::new(Arc::new(config(port, env, 1, 1))).unwrap();

    let result = router.dispatch("m", &body(), false, false, "test").await;
    assert!(result.is_err(), "odpowiedź po 3 s musi przekroczyć non_stream_timeout = 1 s");
}
