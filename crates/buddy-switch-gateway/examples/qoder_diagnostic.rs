//! Bounded, opt-in live check through the production handler. No ZCode session edits.
use axum::body::{to_bytes, Bytes};
use buddy_switch_core::modules::{config, qoder::region::QoderRegion};
use buddy_switch_gateway::qoder::{
    routes::{chat_for_region, Protocol},
    QoderGatewayConfig, QoderGatewayState,
};
use serde_json::json;

#[tokio::main]
async fn main() {
    let root =
        std::env::var(config::BUDDY_SWITCH_HOME_ENV).expect("explicit preview home required");
    assert!(std::path::Path::new(&root).is_absolute());
    assert!(
        std::path::Path::new(&root)
            .file_name()
            .is_some_and(|n| n == ".buddy-qoder-switch-preview"),
        "preview-only diagnostic"
    );
    assert!(std::path::Path::new(&root)
        .join(".buddy-switch/qoder/accounts.cn.json")
        .is_file());
    let state = QoderGatewayState::new(QoderGatewayConfig {
        max_rotate: 1,
        ..Default::default()
    });
    let requested = std::env::var("BUDDY_DIAG_MODEL").ok();
    for model in ["dfmodel", "qfmodel"].into_iter().filter(|model| {
        requested
            .as_deref()
            .is_none_or(|requested| requested == *model)
    }) {
        let response = chat_for_region(
            state.clone(),
            QoderRegion::Cn,
            Bytes::from(
                json!({
                    "model":model,"stream":true,"max_tokens":32,
                    "messages":[{"role":"user","content":"Reply with exactly GATEWAY_PROBE_OK."}]
                })
                .to_string(),
            ),
            Protocol::Anthropic,
        )
        .await;
        let status = response.status().as_u16();
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("bounded response");
        let text = String::from_utf8_lossy(&bytes);
        let mut rendered = String::new();
        let mut tool_blocks = 0;
        for line in text.lines().filter_map(|line| line.strip_prefix("data:")) {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                if event.pointer("/delta/type").and_then(|v| v.as_str()) == Some("text_delta") {
                    if let Some(text) = event.pointer("/delta/text").and_then(|v| v.as_str()) {
                        rendered.push_str(text);
                    }
                }
                if event
                    .pointer("/content_block/type")
                    .and_then(|v| v.as_str())
                    == Some("tool_use")
                {
                    tool_blocks += 1;
                }
            }
        }
        if let Ok(directory) = std::env::var("BUDDY_DIAG_OUTPUT_DIR") {
            std::fs::write(
                std::path::Path::new(&directory).join(format!("queue-live-{model}.sse")),
                &bytes,
            )
            .expect("save diagnostic response");
        }
        let error = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v.get("error").cloned());
        println!(
            "{}",
            json!({"time":chrono::Utc::now().to_rfc3339(),"model":model,"protocol":"anthropic",
            "httpStatus":status,"retryAfter":retry,"messageStops":text.matches("event: message_stop").count(),
                "errorEvents":text.matches("event: error").count(),"hasDiagnosticMarker":rendered.contains("GATEWAY_PROBE_OK"),
                "renderedCharacters":rendered.chars().count(),"toolBlocks":tool_blocks,
            "responseBytes":bytes.len(),"error":error})
        );
    }
}
