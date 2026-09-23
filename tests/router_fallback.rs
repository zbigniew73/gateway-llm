use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use gateway_llm::config::{
    Config, Deployment, ModelEntry, ProviderConfig, RoutingConfig, ServerConfig,
};
use gateway_llm::router::Router;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const OK_BODY: &str = r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
const CONTEXT_400: &str = r#"{"error":{"message":"This model's maximum context length is 32768 tokens. However, you requested 40000 tokens."}}"#;
const GENERIC_400: &str = r#"{"error":{"message":"invalid tool schema: missing 'type'"}}"#;

async fn mock(status: u16, body: &'static str) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0u8; 64 * 1024];
            let _ = socket.read(&mut buf).await;
            let reason = if status == 200 { "OK" } else { "Bad Request" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (port, hits)
}

fn provider(port: u16) -> ProviderConfig {
    ProviderConfig {
        base_url: format!("http://127.0.0.1:{port}"),
        chat_path: "/v1/chat/completions".to_string(),
        rpm: None,
        stream_usage: false,
    }
}

fn deployment(provider: &str, api_key_env: &str, order: i64) -> Deployment {
    Deployment {
        provider: provider.to_string(),
        model: format!("model-{provider}"),
        api_key_env: api_key_env.to_string(),
        order,
    }
}

fn router(providers: Vec<(&str, u16)>, model_list: Vec<ModelEntry>) -> Router {
    router_with(
        providers
            .into_iter()
            .map(|(name, port)| (name.to_string(), provider(port)))
            .collect(),
        model_list,
    )
}

fn router_with(providers: HashMap<String, ProviderConfig>, model_list: Vec<ModelEntry>) -> Router {
    let config = Config {
        server: ServerConfig::default(),
        routing: RoutingConfig::default(),
        providers,
        model_list,
    };
    config.validate().expect("config testowy musi być poprawny");
    Router::new(Arc::new(config)).unwrap()
}

fn body() -> serde_json::Value {
    json!({ "model": "a", "messages": [{ "role": "user", "content": "hi" }] })
}

#[tokio::test]
async fn context_error_moves_to_next_deployment_within_alias() {
    let env = "GW_TEST_KEY_CTX_DEPLOYMENT";
    std::env::set_var(env, "k");
    let (port1, hits1) = mock(400, CONTEXT_400).await;
    let (port2, hits2) = mock(200, OK_BODY).await;
    let router = router(
        vec![("p1", port1), ("p2", port2)],
        vec![ModelEntry {
            model_name: "a".to_string(),
            deployments: vec![deployment("p1", env, 1), deployment("p2", env, 2)],
            fallback_model: None,
        }],
    );

    let dispatched = router
        .dispatch("a", &body(), false, false, "test")
        .await
        .expect("drugi deployment może mieć większe okno kontekstu");
    assert_eq!(dispatched.provider, "p2");
    assert_eq!(hits1.load(Ordering::SeqCst), 1);
    assert_eq!(hits2.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn generic_400_stops_within_alias() {
    let env = "GW_TEST_KEY_GENERIC_DEPLOYMENT";
    std::env::set_var(env, "k");
    let (port1, _) = mock(400, GENERIC_400).await;
    let (port2, hits2) = mock(200, OK_BODY).await;
    let router = router(
        vec![("p1", port1), ("p2", port2)],
        vec![ModelEntry {
            model_name: "a".to_string(),
            deployments: vec![deployment("p1", env, 1), deployment("p2", env, 2)],
            fallback_model: None,
        }],
    );

    let result = router.dispatch("a", &body(), false, false, "test").await;
    assert!(result.is_err());
    assert_eq!(
        hits2.load(Ordering::SeqCst),
        0,
        "zły request nie może iść dalej"
    );
}

#[tokio::test]
async fn context_error_moves_to_fallback_model() {
    let env = "GW_TEST_KEY_CTX_FALLBACK";
    std::env::set_var(env, "k");
    let (port1, _) = mock(400, CONTEXT_400).await;
    let (port2, hits2) = mock(200, OK_BODY).await;
    let router = router(
        vec![("p1", port1), ("p2", port2)],
        vec![
            ModelEntry {
                model_name: "a".to_string(),
                deployments: vec![deployment("p1", env, 1)],
                fallback_model: Some("b".to_string()),
            },
            ModelEntry {
                model_name: "b".to_string(),
                deployments: vec![deployment("p2", env, 1)],
                fallback_model: None,
            },
        ],
    );

    let dispatched = router
        .dispatch("a", &body(), false, false, "test")
        .await
        .expect("fallback_model może mieć większe okno kontekstu");
    assert_eq!(dispatched.provider, "p2");
    assert_eq!(hits2.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn generic_400_does_not_trigger_fallback_model() {
    let env = "GW_TEST_KEY_GENERIC_FALLBACK";
    std::env::set_var(env, "k");
    let (port1, _) = mock(400, GENERIC_400).await;
    let (port2, hits2) = mock(200, OK_BODY).await;
    let router = router(
        vec![("p1", port1), ("p2", port2)],
        vec![
            ModelEntry {
                model_name: "a".to_string(),
                deployments: vec![deployment("p1", env, 1)],
                fallback_model: Some("b".to_string()),
            },
            ModelEntry {
                model_name: "b".to_string(),
                deployments: vec![deployment("p2", env, 1)],
                fallback_model: None,
            },
        ],
    );

    let result = router.dispatch("a", &body(), false, false, "test").await;
    assert!(result.is_err());
    assert_eq!(
        hits2.load(Ordering::SeqCst),
        0,
        "fallback_model nie naprawi złego requestu"
    );
}

#[tokio::test]
async fn deployment_over_rpm_limit_is_still_tried_as_last_resort() {
    let env = "GW_TEST_KEY_RPM_LAST_RESORT";
    std::env::set_var(env, "k");
    let (port1, hits1) = mock(200, OK_BODY).await;
    let (port2, hits2) = mock(500, r#"{"error":{"message":"upstream down"}}"#).await;

    let mut limited = provider(port1);
    limited.rpm = Some(1);
    let providers = HashMap::from([
        ("p1".to_string(), limited),
        ("p2".to_string(), provider(port2)),
    ]);
    let router = router_with(
        providers,
        vec![ModelEntry {
            model_name: "a".to_string(),
            deployments: vec![deployment("p1", env, 1), deployment("p2", env, 2)],
            fallback_model: None,
        }],
    );

    let first = router
        .dispatch("a", &body(), false, false, "t1")
        .await
        .unwrap();
    assert_eq!(first.provider, "p1");

    let second = router
        .dispatch("a", &body(), false, false, "t2")
        .await
        .expect("deployment bez wolnego limitu to ostatnia deska ratunku");
    assert_eq!(second.provider, "p1");
    assert_eq!(
        hits2.load(Ordering::SeqCst),
        1,
        "najpierw deployment z wolnym limitem"
    );
    assert_eq!(hits1.load(Ordering::SeqCst), 2);
}
