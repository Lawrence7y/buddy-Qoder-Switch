//! Isolated real-HTTP regressions for both Qoder realms; no vendor calls.
use axum::{
    body::{to_bytes, Bytes},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use buddy_switch_core::modules::{
    config,
    qoder::{
        account::{self, RawAccount},
        region::QoderRegion,
    },
};
use buddy_switch_gateway::qoder::{
    routes::{chat_for_region, Protocol},
    QoderGatewayConfig, QoderGatewayState,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

async fn upstream(method: Method, hits: Arc<AtomicUsize>) -> Response {
    if method == Method::GET {
        return Json(json!({"chat":[{"key":"dfmodel","max_input_tokens":128000}]})).into_response();
    }
    let nth = hits.fetch_add(1, Ordering::SeqCst);
    if nth == 100 {
        struct Cancelled(Arc<AtomicUsize>);
        impl Drop for Cancelled {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let guard = Cancelled(hits);
        let first = format!(
            "data: {}\n\n",
            json!({"body":json!({"choices":[{"delta":{"content":"WAITING"}}]}).to_string(),"statusCodeValue":200})
        );
        let body = stream::unfold(
            (Some(Bytes::from(first)), guard),
            |(first, guard)| async move {
                if let Some(first) = first {
                    Some((Ok::<_, std::io::Error>(first), (None, guard)))
                } else {
                    std::future::pending().await
                }
            },
        );
        return Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(body))
            .unwrap();
    }
    let body = if nth < 2 {
        // Two consecutive short queues must be absorbed, not handed to the client.
        json!({"code":"10605","message":json!({"isQueued":true,"retryAfterSeconds":1,"waitTime":1,"subCode":"CALL_COUNT_LIMIT"}).to_string()})
    } else {
        json!({"id":"stable","choices":[{"delta":{"content":"STABLE_完整"},"finish_reason":"stop"}]})
    };
    let envelope = json!({"body":body.to_string(),"statusCodeValue":if nth < 2 {403} else {200}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {envelope}\n\ndata: {{\"body\":\"[DONE]\"}}\n\n"),
    )
        .into_response()
}

#[tokio::test]
async fn repeated_short_queue_is_absorbed_and_zero_budget_stays_retryable() {
    // One test owns all process-wide environment mutations in this executable.
    let root = std::env::temp_dir().join(format!("buddy-stability-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var(config::BUDDY_SWITCH_HOME_ENV, &root);
    std::env::remove_var("QD_QUEUE_WAIT_SECS");
    for region in QoderRegion::all() {
        account::upsert_account_for(
            region,
            RawAccount {
                uid: format!("stable-{}", region.as_str()),
                domain: if region == QoderRegion::Cn {
                    "qoder.com.cn"
                } else {
                    "qoder.com"
                }
                .into(),
                access_token: Some("dt-fixture".into()),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mock_hits = hits.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(any(move |method: Method| {
                upstream(method, mock_hits.clone())
            })),
        )
        .await
        .unwrap();
    });
    for region in QoderRegion::all() {
        for protocol in [Protocol::OpenAi, Protocol::Anthropic] {
            for stream in [false, true] {
                hits.store(0, Ordering::SeqCst);
                let state = QoderGatewayState::new(QoderGatewayConfig::default())
                    .with_upstream(format!("http://{address}"));
                let response = chat_for_region(state,region,Bytes::from(json!({"model":"dfmodel","stream":stream,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]}).to_string()),protocol).await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{region:?}/{protocol:?}/{stream}"
                );
                let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
                let text = String::from_utf8(body.to_vec()).unwrap();
                assert!(text.contains("STABLE_完整"), "{text}");
                assert!(!text.contains("rate_limit_error"), "{text}");
                assert_eq!(
                    hits.load(Ordering::SeqCst),
                    3,
                    "must wait twice before succeeding"
                );
            }
        }
    }
    // Zero disables internal waiting, not account rotation or Retry-After.
    std::env::set_var("QD_QUEUE_WAIT_SECS", "0");
    hits.store(0, Ordering::SeqCst);
    let state = QoderGatewayState::new(QoderGatewayConfig::default())
        .with_upstream(format!("http://{address}"));
    let response = chat_for_region(state,QoderRegion::Cn,Bytes::from(json!({"model":"dfmodel","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]}).to_string()),Protocol::Anthropic).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "1");
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    std::env::remove_var("QD_QUEUE_WAIT_SECS");
    // A cancelled client must release even an upstream that never yields again.
    hits.store(100, Ordering::SeqCst);
    let state = QoderGatewayState::new(QoderGatewayConfig::default())
        .with_upstream(format!("http://{address}"));
    let response = chat_for_region(state.clone(),QoderRegion::Cn,Bytes::from(json!({"model":"dfmodel","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]}).to_string()),Protocol::OpenAi).await;
    let mut body = response.into_body().into_data_stream();
    assert!(body.next().await.unwrap().unwrap().starts_with(b"data:"));
    drop(body);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while hits.load(Ordering::SeqCst) < 102
            || !state.log.list().iter().any(|row| row["status"] == 499)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client cancellation must drop pending upstream and record 499");
    server.abort();
}
