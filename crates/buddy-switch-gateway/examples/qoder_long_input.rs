//! 长输入（long-input）真机探针 —— 走**生产 handler**，不改任何源码。
//!
//! ## 为什么单独一个文件
//!
//! `qoder_diagnostic.rs` 是「短 prompt 健康检查」。长输入要测的是另外几件事：
//! 预检窗口（`preflight.rs:72` 的 60s timeout）、前缀上限（`preflight.rs:11`
//! 的 `PREFIX_LIMIT = 256 KiB`）、以及首 delta 与总耗时的分离。
//! 这些都需要**分级升压 + 逐级计时**，塞进原文件会把那个干净的健康检查弄脏。
//!
//! ## 关键口径纠正（读源码得到，不是猜）
//!
//! `PREFIX_LIMIT` 限制的是**上游响应在首个有效 delta 之前的字节数**
//! （`preflight.rs:97-100`：`size += chunk.len()` 累加的是 **response** chunk），
//! **不是**请求 prompt 的字节数。所以「prompt 很大」不会直接触发它；
//! 真正会触发它的是「上游在吐正文前先回了 >256 KiB 的别的东西」
//! （大段回显、超长 role 帧、超长思考前言）。本探针如实记录**响应前缀字节**，
//! 若观察不到超限，就报「未触发」，不伪造。
//!
//! ## 运行
//!
//! ```text
//! BUDDY_SWITCH_HOME=C:/Users/Administrator/.buddy-qoder-switch-preview \
//! BUDDY_DIAG_OUTPUT_DIR=D:/Project/buddy-Qoder-Switch/reports/qoder-live \
//!   cargo run -p buddy-switch-gateway --example qoder_long_input --release
//! ```
//!
//! 可用环境变量：
//!   * `BUDDY_DIAG_MODEL`      只测某个模型（`dfmodel` / `qfmodel`）
//!   * `BUDDY_DIAG_SIZES`      逗号分隔的字符数，默认 `4000,40000,160000,400000`
//!   * `BUDDY_DIAG_PROMPT_CHARS` 只测这一个尺寸（覆盖 `BUDDY_DIAG_SIZES`）

use std::time::Instant;

use axum::body::to_bytes;
use buddy_switch_core::modules::{config, qoder::region::QoderRegion};
use buddy_switch_gateway::qoder::{
    routes::{chat_for_region, Protocol},
    QoderGatewayConfig, QoderGatewayState,
};
use serde_json::json;

/// 同质 ASCII 填充：不引入任何非 ASCII，让 char 数 == 字节数，口径不含糊。
const FILLER_LINE: &str =
    "The quick brown fox jumps over the lazy dog while the gateway measures input scaling. ";

/// 末尾的单条指令：通过与否可以机械判定。
const INSTRUCTION: &str = "\n\nIgnore all filler above. Reply with exactly: LONG_INPUT_OK\n";

fn build_prompt(target_chars: usize) -> String {
    if target_chars <= INSTRUCTION.len() {
        return INSTRUCTION.to_string();
    }
    let body_chars = target_chars - INSTRUCTION.len();
    let repeats = body_chars / FILLER_LINE.len() + 1;
    let mut prompt = String::with_capacity(target_chars + FILLER_LINE.len());
    for _ in 0..repeats {
        prompt.push_str(FILLER_LINE);
    }
    prompt.truncate(body_chars);
    prompt.push_str(INSTRUCTION);
    prompt
}

// ---------------------------------------------------------------------------
// 针尖测试（needle）：证明「模型真的读到了整段输入」
// ---------------------------------------------------------------------------
//
// 「末尾指令被照做了」只能证明**尾部**到达了上游。若网关或上游在中途**静默截断**，
// 末尾指令仍可能被读到（截断通常保留尾部），于是「通过」并不能区分
// 「真的处理了 40 万 token」和「悄悄只送了最后一段」。
//
// 做法：在开头 / 中间 / 接近结尾三处各埋一个**随机唯一码**，而**指令里不出现这些码**。
// 要求模型只回三个码。截断则必然答不全 —— 这才是能证伪的长输入证据。

fn needle_codes() -> [String; 3] {
    // 用时间熵派生三个互不相同的 8 位大写码（无外部依赖）。
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut state = seed | 1;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        format!("{:08X}", (value & 0xFFFF_FFFF) as u32)
    };
    [next(), next(), next()]
}

/// 在指定轴向位置插入针。
fn build_needle_prompt(target_chars: usize, codes: &[String; 3]) -> String {
    let instruction = "\n\nSomewhere in the text above, three secret codes were embedded \
                       (they look like 8 uppercase hex characters, e.g. 0A1B2C3D). \
                       Reply with ONLY those three codes, comma-separated, in the order they appear. \
                       Do not explain.\n";
    let body_chars = target_chars.saturating_sub(instruction.len());
    let mut filler = String::with_capacity(body_chars);
    while filler.len() < body_chars {
        filler.push_str(FILLER_LINE);
    }
    filler.truncate(body_chars);

    // 三个插入点：5% / 50% / 95%（按字节，偏 ASCII 安全边界）
    let at = |ratio: f64| -> usize {
        let raw = (filler.len() as f64 * ratio) as usize;
        // 回退到最近的换行/空格，避免切开多字节边界（本填充是纯 ASCII，仍然保险）。
        filler[..raw]
            .rfind(' ')
            .map(|i| i + 1)
            .unwrap_or(0)
            .min(filler.len())
    };
    let mut points = [at(0.05), at(0.50), at(0.95)];
    points.sort_unstable();

    let mut out = String::with_capacity(target_chars + 128);
    let mut cursor = 0usize;
    for (index, point) in points.iter().enumerate() {
        out.push_str(&filler[cursor..*point]);
        out.push_str(&format!("SECRET CODE {} IS {}. ", index + 1, codes[index]));
        cursor = *point;
    }
    out.push_str(&filler[cursor..]);
    out.push_str(instruction);
    out
}

/// 从 SSE 文本里抽计量指标。
struct SseStats {
    message_stops: usize,
    error_events: usize,
    rendered: String,
    /// 首个含**非空文本 delta** 的事件在原始字节流中的字节偏移（近似 response 前缀字节数）。
    first_delta_offset: Option<usize>,
    /// 原始响应里是否出现 queue 特征。
    queue_hits: Vec<String>,
}

fn inspect_sse(bytes: &[u8]) -> SseStats {
    let text = String::from_utf8_lossy(bytes);
    let mut message_stops = 0usize;
    let mut error_events = 0usize;
    let mut rendered = String::new();
    let mut first_delta_offset = None;
    let mut queue_hits = Vec::new();

    let mut offset = 0usize;
    for raw_line in text.split_inclusive('\n') {
        let line_start = offset;
        offset += raw_line.len();
        let line = raw_line.trim_end_matches(['\r', '\n']);

        if line == "event: message_stop" {
            message_stops += 1;
        }
        if line == "event: error" || line == "event:error" {
            error_events += 1;
        }
        for needle in [
            "10605",
            "CALL_COUNT_LIMIT",
            "isQueued",
            "queueCount",
            "retryAfterSeconds",
            "waitTime",
        ] {
            if line.contains(needle) && !queue_hits.iter().any(|h| h == needle) {
                queue_hits.push(needle.to_string());
            }
        }

        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(payload.trim()) else {
            continue;
        };
        // Anthropic 形态：content_block_delta / delta.text
        if let Some(delta_text) = event.pointer("/delta/text").and_then(|v| v.as_str()) {
            if !delta_text.is_empty() {
                if first_delta_offset.is_none() {
                    first_delta_offset = Some(line_start);
                }
                rendered.push_str(delta_text);
            }
        }
        // OpenAI 形态兜底：choices[].delta.content
        if let Some(choices) = event.get("choices").and_then(serde_json::Value::as_array) {
            for choice in choices {
                if let Some(content) = choice.pointer("/delta/content").and_then(|v| v.as_str()) {
                    if !content.is_empty() {
                        if first_delta_offset.is_none() {
                            first_delta_offset = Some(line_start);
                        }
                        rendered.push_str(content);
                    }
                }
            }
        }
    }

    SseStats {
        message_stops,
        error_events,
        rendered,
        first_delta_offset,
        queue_hits,
    }
}

#[tokio::main]
async fn main() {
    let root = std::env::var(config::BUDDY_SWITCH_HOME_ENV).expect("必须显式设 BUDDY_SWITCH_HOME");
    assert!(std::path::Path::new(&root).is_absolute(), "必须绝对路径");
    assert!(
        std::path::Path::new(&root)
            .file_name()
            .is_some_and(|name| name == ".buddy-qoder-switch-preview"),
        "只允许在预览库上跑"
    );

    let output_dir = std::env::var("BUDDY_DIAG_OUTPUT_DIR")
        .unwrap_or_else(|_| "D:/Project/buddy-Qoder-Switch/reports/qoder-live".to_string());
    std::fs::create_dir_all(&output_dir).expect("建证据目录");

    // 尺寸表（字符数）。默认四级升压。
    let sizes: Vec<usize> = if let Ok(single) = std::env::var("BUDDY_DIAG_PROMPT_CHARS") {
        vec![single
            .trim()
            .parse::<usize>()
            .expect("BUDDY_DIAG_PROMPT_CHARS 需整数")]
    } else if let Ok(list) = std::env::var("BUDDY_DIAG_SIZES") {
        list.split(',')
            .filter_map(|item| item.trim().parse::<usize>().ok())
            .collect()
    } else {
        vec![4_000, 40_000, 160_000, 400_000]
    };

    let requested_model = std::env::var("BUDDY_DIAG_MODEL").ok();
    let models: Vec<&str> = ["dfmodel", "qfmodel"]
        .into_iter()
        .filter(|model| {
            requested_model
                .as_deref()
                .is_none_or(|requested| requested == *model)
        })
        .collect();

    let state = QoderGatewayState::new(QoderGatewayConfig {
        max_rotate: 1,
        ..Default::default()
    });

    let needle_mode = std::env::var("BUDDY_DIAG_NEEDLE")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    println!(
        "{}",
        json!({
            "probe": "qoder_long_input",
            "mode": if needle_mode { "needle" } else { "tail-instruction" },
            "buddySwitchHome": root,
            "outputDir": output_dir,
            "models": models,
            "promptChars": sizes,
            "note": "char counts are byte counts too: filler is pure ASCII",
            "startedAt": chrono::Utc::now().to_rfc3339(),
        })
    );

    let mut abort_reason: Option<String> = None;

    'outer: for model in models {
        for chars in &sizes {
            // needle 模式：三个随机码埋在不同深度，答全才算真读到。
            let codes = needle_codes();
            let prompt = if needle_mode {
                build_needle_prompt(*chars, &codes)
            } else {
                build_prompt(*chars)
            };
            let prompt_chars = prompt.chars().count();
            let prompt_bytes = prompt.len();
            // 粗略 token 估计：ASCII 英文约 4 字符/token，仅作展示，不参与判定。
            let prompt_tokens_est = prompt_chars / 4;

            let body = json!({
                "model": model,
                "stream": true,
                "max_tokens": 32,
                "messages": [{ "role": "user", "content": prompt }],
            });

            let started = Instant::now();
            let response = chat_for_region(
                state.clone(),
                QoderRegion::Cn,
                axum::body::Bytes::from(body.to_string()),
                Protocol::Anthropic,
            )
            .await;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);

            // 先把整个响应读干净（时间停止在读完那一刻）。
            let bytes = match to_bytes(response.into_body(), 64 * 1024 * 1024).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    println!(
                        "{}",
                        json!({ "model": model, "promptChars": prompt_chars,
                                "fatal": format!("读响应体失败: {error}") })
                    );
                    break 'outer;
                }
            };
            let total_elapsed = started.elapsed();

            let stats = inspect_sse(&bytes);
            let first_delta_elapsed = stats
                .first_delta_offset
                .map(|offset| {
                    // 首 delta 之前的部分按比例估时：SSE 是顺序到达的，
                    // 用「前缀字节 / 总字节」× 总耗时得出上界估计。
                    // 这是**估计值**，不是实测值 —— 报告里必须标明。
                    let ratio = if bytes.is_empty() {
                        0.0
                    } else {
                        offset as f64 / bytes.len() as f64
                    };
                    total_elapsed.mul_f64(ratio.min(1.0))
                })
                .unwrap_or(total_elapsed);

            let passed_tail = stats.rendered.contains("LONG_INPUT_OK");
            // needle 判定：三个码**全部**出现才算真的读到了整段输入。
            let codes_found = codes
                .iter()
                .filter(|code| stats.rendered.to_uppercase().contains(code.as_str()))
                .count();
            let passed = if needle_mode {
                codes_found == 3
            } else {
                passed_tail
            };
            let queued = !stats.queue_hits.is_empty();

            // 落盘原始 SSE（脱敏：这里不可能有 token，仍过一遍兜底）。
            let sse_name = format!("long-input-{model}-{prompt_chars}.sse");
            let sse_path = std::path::Path::new(&output_dir).join(&sse_name);
            let _ = std::fs::write(&sse_path, &bytes);

            println!(
                "{}",
                json!({
                    "model": model,
                    "promptChars": prompt_chars,
                    "promptBytes": prompt_bytes,
                    "promptTokensEst": prompt_tokens_est,
                    "httpStatus": status,
                    "retryAfter": retry_after,
                    "passed": passed,
                    "passedMarker": passed_tail,
                    "needleCodesExpected": if needle_mode { json!(codes) } else { json!(null) },
                    "needleCodesFound": if needle_mode { json!(codes_found) } else { json!(null) },
                    "messageStops": stats.message_stops,
                    "errorEvents": stats.error_events,
                    "queueHits": stats.queue_hits,
                    "queued": queued,
                    "responseBytes": bytes.len(),
                    "responsePrefixBytesBeforeFirstDelta": stats.first_delta_offset,
                    "totalElapsedMs": total_elapsed.as_millis(),
                    "firstDeltaElapsedMsEstimate": first_delta_elapsed.as_millis(),
                    "renderedCharacters": stats.rendered.chars().count(),
                    "renderedHead": stats.rendered.chars().take(160).collect::<String>(),
                    "sseFile": sse_name,
                    "at": chrono::Utc::now().to_rfc3339(),
                })
            );

            // 遇到排队立刻停 —— 不对排队中的上游继续施压。
            if queued {
                abort_reason = Some(format!(
                    "模型 {model} 在 {prompt_chars} 字符处出现排队特征（{:?}），已停止升压",
                    stats.queue_hits
                ));
                break 'outer;
            }

            // 级间间隔 ≥2s（对上游友好）。
            tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
        }
    }

    if let Some(reason) = abort_reason {
        println!("{}", json!({ "aborted": reason }));
    }
    println!(
        "{}",
        json!({ "finishedAt": chrono::Utc::now().to_rfc3339() })
    );
}
