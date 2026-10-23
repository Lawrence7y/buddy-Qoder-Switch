//! Real localhost upstream HTTP + production Qoder handler, no vendor calls.
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
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

async fn queued_upstream(method: Method) -> Response {
    if method == Method::GET {
        return Json(json!({"chat":[{"key":"dfmodel","display_name":"DeepSeek Flash","max_input_tokens":128000}]})).into_response();
    }
    let queue = json!({"isQueued":true,"queueCount":2796,"queueType":"p3","retryAfterSeconds":30,"serviceAvailable":true,"subCode":"CALL_COUNT_LIMIT","waitTime":170});
    let inner = json!({"code":"10605","message":queue.to_string(),"subCode":"CALL_COUNT_LIMIT"});
    let error = json!({"code":"403","message":inner.to_string()});
    let envelope = json!({"body":error.to_string(),"statusCodeValue":403});
    (
        [("content-type", "text/event-stream")],
        format!("data: {envelope}\n\ndata: {{\"body\":\"[DONE]\"}}\n"),
    )
        .into_response()
}

#[tokio::test]
async fn http_200_queue_envelope_returns_retryable_429_in_both_protocols() {
    let root = std::env::temp_dir().join(format!("buddy-queue-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var(config::BUDDY_SWITCH_HOME_ENV, &root);
    account::upsert_account_for(
        QoderRegion::Cn,
        RawAccount {
            uid: "queue-http-fixture".into(),
            domain: "qoder.com.cn".into(),
            access_token: Some("dt-test".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(any(queued_upstream)))
            .await
            .unwrap()
    });
    for protocol in [Protocol::OpenAi, Protocol::Anthropic] {
        for stream in [true, false] {
            let state = QoderGatewayState::new(QoderGatewayConfig::default())
                .with_upstream(format!("http://{address}"));
            let response=chat_for_region(state.clone(),QoderRegion::Cn,Bytes::from(json!({
                "model":"dfmodel","stream":stream,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]
            }).to_string()),protocol).await;
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(response.headers()["retry-after"], "170");
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("CALL_COUNT_LIMIT"));
            if protocol == Protocol::Anthropic {
                assert_eq!(body["type"], "error");
                assert_eq!(body["error"]["type"], "rate_limit_error");
            }
            let pools = state.pools.lock().await;
            let pool = pools.get(&QoderRegion::Cn).unwrap();
            let entry = pool.entries().next().unwrap();
            assert!(!entry.auth_dead, "queue must not invalidate the credential");
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            assert!(entry.model_cooldowns.get("dfmodel").copied().unwrap_or(0) >= now + 160000);
        }
    }
    server.abort();
    // Retain the isolated fixture root for diagnostics; never target real home.
}

/// 短排队限流（retry_after ≤ 90s，队列人数≈0）⇒ **换号续试**：
/// 同一模型在第二个账号上直接成功，客户端看到的是 200 而不是 429。
/// （2026-10-21 真机：8h 内 103/130 全是 30s 级 CALL_COUNT_LIMIT。）
#[tokio::test]
async fn short_queue_rotates_to_the_next_account_instead_of_failing() {
    let root = std::env::temp_dir().join(format!("buddy-queue-rotate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var(config::BUDDY_SWITCH_HOME_ENV, &root);
    for uid in ["rotate-a", "rotate-b"] {
        account::upsert_account_for(
            QoderRegion::Cn,
            RawAccount {
                uid: uid.into(),
                domain: "qoder.com.cn".into(),
                access_token: Some(format!("dt-{uid}")),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_for_mock = hits.clone();
    async fn rotate_upstream(
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        method: Method,
    ) -> Response {
        if method == Method::GET {
            return Json(json!({"chat":[{"key":"dfmodel","display_name":"DeepSeek Flash","max_input_tokens":128000}]})).into_response();
        }
        let nth = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if nth == 0 {
            // 第一次：短排队限流（30s，≤ 90s 上限 ⇒ 触发换号）。
            let queue = json!({"isQueued":true,"queueCount":0,"queueType":"p3","retryAfterSeconds":30,"serviceAvailable":true,"subCode":"CALL_COUNT_LIMIT","waitTime":30});
            let inner =
                json!({"code":"10605","message":queue.to_string(),"subCode":"CALL_COUNT_LIMIT"});
            let error = json!({"code":"403","message":inner.to_string()});
            let envelope = json!({"body":error.to_string(),"statusCodeValue":403});
            return (
                [("content-type", "text/event-stream")],
                format!("data: {envelope}\n\ndata: {{\"body\":\"[DONE]\"}}\n"),
            )
                .into_response();
        }
        // 第二次（换号后）：正常补全。
        let chunk = json!({"choices":[{"index":0,"delta":{"content":"ROTATED_OK"},"finish_reason":null}],"id":"c1","object":"chat.completion.chunk","created":1,"model":"dfmodel"});
        (
            [("content-type", "text/event-stream")],
            format!(
                "data: {}\n\ndata: {{\"body\":\"[DONE]\"}}\n",
                json!({"body":chunk.to_string(),"statusCodeValue":200})
            ),
        )
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let hits = hits_for_mock;
        axum::serve(
            listener,
            Router::new().fallback(any(move |method: Method| {
                let hits = hits.clone();
                async move { rotate_upstream(hits, method).await }
            })),
        )
        .await
        .unwrap()
    });

    let state = QoderGatewayState::new(QoderGatewayConfig::default())
        .with_upstream(format!("http://{address}"));
    let response = chat_for_region(
        state,
        QoderRegion::Cn,
        Bytes::from(
            json!({
                "model":"dfmodel","stream":true,"max_tokens":16,
                "messages":[{"role":"user","content":"fixture"}]
            })
            .to_string(),
        ),
        Protocol::Anthropic,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "短排队必须换号吸收，而不是 429 抛给客户端"
    );
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("ROTATED_OK"),
        "换号后的补全必须到达客户端：{text}"
    );
    assert!(
        hits.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "必须真的换号重发过"
    );
    server.abort();
}

/// 模型级深队列（**非** CALL_COUNT_LIMIT，retry_after > 90s）不轮换：
/// 换号对模型级排队无意义 ⇒ 维持既有契约 —— 立即 429 + Retry-After，只打一个账号。
#[tokio::test]
async fn deep_queue_still_returns_429_without_rotation() {
    let root = std::env::temp_dir().join(format!("buddy-queue-deep-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var(config::BUDDY_SWITCH_HOME_ENV, &root);
    for uid in ["deep-a", "deep-b"] {
        account::upsert_account_for(
            QoderRegion::Cn,
            RawAccount {
                uid: uid.into(),
                domain: "qoder.com.cn".into(),
                access_token: Some(format!("dt-{uid}")),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_for_mock = hits.clone();
    async fn deep_upstream(
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        method: Method,
    ) -> Response {
        if method == Method::GET {
            return Json(json!({"chat":[{"key":"dfmodel","display_name":"DeepSeek Flash","max_input_tokens":128000}]})).into_response();
        }
        hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // 模型级深队列：isQueued 但**不带** CALL_COUNT_LIMIT（10605 家族之外）。
        let queue = json!({"isQueued":true,"queueCount":2796,"queueType":"p3","retryAfterSeconds":30,"serviceAvailable":true,"subCode":"RESOURCE_LIMIT","waitTime":170});
        let inner = json!({"code":"10610","message":queue.to_string(),"subCode":"RESOURCE_LIMIT"});
        let error = json!({"code":"403","message":inner.to_string()});
        let envelope = json!({"body":error.to_string(),"statusCodeValue":403});
        (
            [("content-type", "text/event-stream")],
            format!("data: {envelope}\n\ndata: {{\"body\":\"[DONE]\"}}\n"),
        )
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let hits = hits_for_mock;
        axum::serve(
            listener,
            Router::new().fallback(any(move |method: Method| {
                let hits = hits.clone();
                async move { deep_upstream(hits, method).await }
            })),
        )
        .await
        .unwrap()
    });

    let state = QoderGatewayState::new(QoderGatewayConfig::default())
        .with_upstream(format!("http://{address}"));
    let response = chat_for_region(
        state,
        QoderRegion::Cn,
        Bytes::from(
            json!({
                "model":"dfmodel","stream":false,"max_tokens":16,
                "messages":[{"role":"user","content":"fixture"}]
            })
            .to_string(),
        ),
        Protocol::Anthropic,
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "170");
    assert!(
        hits.load(std::sync::atomic::Ordering::SeqCst) == 1,
        "模型级深队列（170s > 90s 上限）不得轮换重发"
    );
    server.abort();
}

/// CALL_COUNT_LIMIT 是**账号级**调用次数限制（免费模型每日/短窗限额）：
/// 即使上游建议等待很长（该号今日额度已尽），也必须换号试下一个账号 ——
/// 换号后仍全部排队才如实 429（Retry-After 取建议等待的最大值）。
#[tokio::test]
async fn call_count_limit_rotates_even_when_the_wait_is_long() {
    let root = std::env::temp_dir().join(format!("buddy-queue-ccl-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var(config::BUDDY_SWITCH_HOME_ENV, &root);
    for uid in ["ccl-a", "ccl-b"] {
        account::upsert_account_for(
            QoderRegion::Cn,
            RawAccount {
                uid: uid.into(),
                domain: "qoder.com.cn".into(),
                access_token: Some(format!("dt-{uid}")),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_for_mock = hits.clone();
    async fn ccl_upstream(
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        method: Method,
    ) -> Response {
        if method == Method::GET {
            return Json(json!({"chat":[{"key":"dfmodel","display_name":"DeepSeek Flash","max_input_tokens":128000}]})).into_response();
        }
        hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let queue = json!({"isQueued":true,"queueCount":0,"queueType":"p3","retryAfterSeconds":30,"serviceAvailable":true,"subCode":"CALL_COUNT_LIMIT","waitTime":170});
        let inner =
            json!({"code":"10605","message":queue.to_string(),"subCode":"CALL_COUNT_LIMIT"});
        let error = json!({"code":"403","message":inner.to_string()});
        let envelope = json!({"body":error.to_string(),"statusCodeValue":403});
        (
            [("content-type", "text/event-stream")],
            format!("data: {envelope}\n\ndata: {{\"body\":\"[DONE]\"}}\n"),
        )
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let hits = hits_for_mock;
        axum::serve(
            listener,
            Router::new().fallback(any(move |method: Method| {
                let hits = hits.clone();
                async move { ccl_upstream(hits, method).await }
            })),
        )
        .await
        .unwrap()
    });

    let state = QoderGatewayState::new(QoderGatewayConfig::default())
        .with_upstream(format!("http://{address}"));
    let response = chat_for_region(
        state,
        QoderRegion::Cn,
        Bytes::from(
            json!({
                "model":"dfmodel","stream":false,"max_tokens":16,
                "messages":[{"role":"user","content":"fixture"}]
            })
            .to_string(),
        ),
        Protocol::Anthropic,
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "170");
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "CALL_COUNT_LIMIT 必须轮换到第二个账号（170s 长等待也不例外）"
    );
    server.abort();
}

/// 单账号 + 30s 短限流 ⇒ 换号无号可换，但**整体睡一次后重开一轮**应成功：
/// 高峰期三账号同限时的真实形态，网关内部吸收，客户端无感。
#[tokio::test]
async fn short_queue_single_account_retries_after_the_advised_wait() {
    let root = std::env::temp_dir().join(format!("buddy-queue-timed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var(config::BUDDY_SWITCH_HOME_ENV, &root);
    account::upsert_account_for(
        QoderRegion::Cn,
        RawAccount {
            uid: "timed-a".into(),
            domain: "qoder.com.cn".into(),
            access_token: Some("dt-timed-a".into()),
            ..Default::default()
        },
    )
    .unwrap();

    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits_for_mock = hits.clone();
    async fn timed_upstream(
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        method: Method,
    ) -> Response {
        if method == Method::GET {
            return Json(json!({"chat":[{"key":"dfmodel","display_name":"DeepSeek Flash","max_input_tokens":128000}]})).into_response();
        }
        let nth = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if nth == 0 {
            let queue = json!({"isQueued":true,"queueCount":0,"queueType":"p3","retryAfterSeconds":30,"serviceAvailable":true,"subCode":"CALL_COUNT_LIMIT","waitTime":30});
            let inner =
                json!({"code":"10605","message":queue.to_string(),"subCode":"CALL_COUNT_LIMIT"});
            let error = json!({"code":"403","message":inner.to_string()});
            let envelope = json!({"body":error.to_string(),"statusCodeValue":403});
            return (
                [("content-type", "text/event-stream")],
                format!("data: {envelope}\n\ndata: {{\"body\":\"[DONE]\"}}\n"),
            )
                .into_response();
        }
        let chunk = json!({"choices":[{"index":0,"delta":{"content":"TIMED_OK"},"finish_reason":null}],"id":"c1","object":"chat.completion.chunk","created":1,"model":"dfmodel"});
        (
            [("content-type", "text/event-stream")],
            format!(
                "data: {}\n\ndata: {{\"body\":\"[DONE]\"}}\n",
                json!({"body":chunk.to_string(),"statusCodeValue":200})
            ),
        )
            .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let hits = hits_for_mock;
        axum::serve(
            listener,
            Router::new().fallback(any(move |method: Method| {
                let hits = hits.clone();
                async move { timed_upstream(hits, method).await }
            })),
        )
        .await
        .unwrap()
    });

    let state = QoderGatewayState::new(QoderGatewayConfig::default())
        .with_upstream(format!("http://{address}"));
    let response = chat_for_region(
        state,
        QoderRegion::Cn,
        Bytes::from(
            json!({
                "model":"dfmodel","stream":false,"max_tokens":16,
                "messages":[{"role":"user","content":"fixture"}]
            })
            .to_string(),
        ),
        Protocol::Anthropic,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "睡过上游建议的 30s 后重试应当成功"
    );
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        serde_json::to_string(&body).unwrap().contains("TIMED_OK"),
        "{}",
        serde_json::to_string(&body).unwrap()
    );
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "第一次排队 + 睡后重试 = 恰好两次上游调用"
    );
    server.abort();
}
