use std::collections::HashMap;
use std::sync::Arc;

use gateway_llm::config::{
    Config, Deployment, ModelEntry, ProviderConfig, RoutingConfig, ServerConfig,
};
use gateway_llm::router::Router;
use gateway_llm::state::AppState;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GATEWAY_KEY: &str = "test-gateway-key";

const SSE_MID_STREAM_ERROR: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Zaczynam odpowied\"}}]}\n\ndata: {\"id\":\"gen-abc123\",\"object\":\"chat.completion.chunk\",\"error\":{\"code\":429,\"message\":\"Rate limit exceeded\"},\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n";

const JSON_200_WITH_ERROR: &str =
    r#"{"error":{"code":503,"message":"Provider overloaded"},"choices":[]}"#;

async fn mock(content_type: &'static str, body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 64 * 1024];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    port
}

async fn gateway(provider_port: u16, api_key_env: &str) -> String {
    std::env::set_var(api_key_env, "k");
    let mut providers = HashMap::new();
    providers.insert(
        "p".to_string(),
        ProviderConfig {
            base_url: format!("http://127.0.0.1:{provider_port}"),
            chat_path: "/v1/chat/completions".to_string(),
            rpm: None,
            headers: Default::default(),
        },
    );
    let config = Arc::new(Config {
        server: ServerConfig::default(),
        routing: RoutingConfig::default(),
        providers,
        model_list: vec![ModelEntry {
            model_name: "m".to_string(),
            deployments: vec![Deployment {
                provider: "p".to_string(),
                model: "upstream".to_string(),
                api_key_env: api_key_env.to_string(),
                order: 0,
                stream_usage: false,
                show_reasoning: false,
            }],
            fallback_model: None,
        }],
    });
    config.validate().expect("config testowy musi być poprawny");
    let router = Arc::new(Router::new(config.clone()).unwrap());
    let app = gateway_llm::build_app(AppState::new(config, router, GATEWAY_KEY.to_string()));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://127.0.0.1:{port}")
}

async fn post_messages(base: &str, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", GATEWAY_KEY)
        .json(&json!({
            "model": "m",
            "max_tokens": 100,
            "stream": stream,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn mid_stream_provider_error_becomes_anthropic_error_event() {
    let provider_port = mock("text/event-stream", SSE_MID_STREAM_ERROR).await;
    let base = gateway(provider_port, "GW_TEST_KEY_MID_STREAM").await;

    let body = post_messages(&base, true).await.text().await.unwrap();

    assert!(body.contains("Zaczynam odpowied"), "{body}");
    assert!(body.contains("event: error"), "{body}");
    assert!(body.contains("Rate limit exceeded (kod 429)"), "{body}");
    assert!(body.contains("provider 'p'"), "{body}");
    assert!(!body.contains("message_stop"), "{body}");
    assert!(!body.contains("end_turn"), "{body}");
}

#[tokio::test]
async fn non_stream_error_body_with_http_200_becomes_error_response() {
    let provider_port = mock("application/json", JSON_200_WITH_ERROR).await;
    let base = gateway(provider_port, "GW_TEST_KEY_NON_STREAM").await;

    let response = post_messages(&base, false).await;
    assert_eq!(response.status().as_u16(), 503);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Provider overloaded"),
        "{body}"
    );
}
