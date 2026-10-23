//! Read until the first meaningful upstream delta before committing HTTP 200.
//! Raw prefix bytes are replayed unchanged, including fragmented tool calls.
//!
//! ## 三条硬约束（改这里之前先读）
//!
//! 1. **只在拿到证据时才否决 200**：只有上游在首个正文 / 工具调用**之前**已经发来错误帧
//!    （含排队限流），才回非 2xx。这是「HTTP 200 + 空补全」与「客户端把限流记成成功回合」
//!    的修复点（issue #46；真机证据见 `reports/QUEUE-FIX.md`，规格见
//!    `docs/specs/QODER-PLATFORM.md` §7.1）。ZCode 会把 200 + 帧内 `event: error`
//!    记成 `model.sdk.stream.completed`（成功回合）⇒ 用户看到「空回答」；
//!    真实非 2xx 会被记成 `retryable: true` 并**自动重试**。
//! 2. **等待窗口到点不是上游故障**：`QD_PREFLIGHT_WAIT_SECS` 到点时若流仍然健康 ⇒ **fail open**，
//!    已缓冲前缀原样回放、继续以 200 流式转发。预检只负责「提前发现**已经到达**的错误」，
//!    它**不是首 token 截止时间** —— 推理模型在长输入下首 token 可能远超 60 秒，
//!    把超时当 504 会直接杀掉长输入任务（客户端还会拿它去重试一个本该成功的请求）。
//! 3. **前缀上限同理 fail open**（[`PREFIX_LIMIT`]）：超限说明上游在吐正文前先回了一大段
//!    别的东西（大段回显 / 超长 role 帧 / 超长思考前言），继续扣着只会白等，原样放行让下游判定。
//!
//! ## 首字节预检超时配置化（PRD A04）
//!
//! 默认首字节预检等待时限由原先的 60 s 改为 25 s：
//! 依据 PRD A04——上游 60 s 的首字节超时可能晚于主力客户端自身的首字节超时，
//! 导致客户端先超时重试而上游仍在排队等待；PRD 建议默认 20–30 s，故取 25。
//! 网关通过环境变量 `BUDDY_SWITCH_QODER_PREFLIGHT_FIRST_BYTE_SECS` 支持配置化：
//! - 未设置或为空：使用默认 25 秒；
//! - 1..=120 的整数：生效使用该值；
//! - 非法值（非整数、越界如 0、负数、121、非数字、带小数点等）：回落至默认 25 秒并向 stderr 输出一次性提示；
//! - 256 KiB 字节上限（[`PREFIX_LIMIT`]）保持硬编码不变。
use super::upstream_error::UpstreamFailure;
use axum::body::Bytes;
use buddy_switch_core::modules::qoder::api::{self, ChatFrame};
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use std::sync::Once;
use std::time::Duration;
use std::{io, pin::Pin};

pub type RawUpstream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;
const PREFIX_LIMIT: usize = 256 * 1024;

/// 首字节超时环境变量名。
pub const ENV_PREFLIGHT_FIRST_BYTE_SECS: &str = "BUDDY_SWITCH_QODER_PREFLIGHT_FIRST_BYTE_SECS";

/// 首字节预检等待超时的默认时限（秒）。
///
/// 依据 PRD A04：上游 60 s 的首字节超时可能晚于主力客户端自身的首字节超时，
/// 导致客户端先超时重试而上游仍在排队等待；PRD 建议默认 20–30 s，故取 25。
pub const DEFAULT_FIRST_BYTE_TIMEOUT_SECS: u64 = 25;

/// 首字节预检允许配置的最小与最大时限（秒）。
pub const MIN_FIRST_BYTE_TIMEOUT_SECS: u64 = 1;
pub const MAX_FIRST_BYTE_TIMEOUT_SECS: u64 = 120;

/// 非法首字节配置告警单例（确保进程生命周期内最多向 stderr 提示一次）。
static WARN_INVALID_TIMEOUT_ONCE: Once = Once::new();

fn warn_invalid_first_byte_timeout(raw: &str) {
    WARN_INVALID_TIMEOUT_ONCE.call_once(|| {
        eprintln!(
            "[buddy-switch-gateway] 警告：环境变量 BUDDY_SWITCH_QODER_PREFLIGHT_FIRST_BYTE_SECS 的值 {:?} 非法（仅允许 1..=120 的整数），已回落至默认值 25 秒。",
            raw
        );
    });
}

/// 解析首字节超时秒数配置（纯函数，入参为纯输入，与 env 读取动作分离）。
///
/// - `None` 或 `Some("")` ⇒ 缺省值 25 秒（不告警）；
/// - `1..=120` 的整数字符串 ⇒ 返回对应数值；
/// - 其它非法字符串（非整数、0、负数、越界、带小数点、含空白等）⇒ 回落 25 秒，并向 stderr 输出一次性提示。
///
/// 注：采用严格解析语义，不对入参做 trim。`" 25 "` 因包含首尾空白不被视作有效整数，
/// 会归入非法输入矩阵并回落至 25 秒。
fn parse_preflight_first_byte_secs(raw: Option<&str>) -> u64 {
    let Some(raw) = raw else {
        return DEFAULT_FIRST_BYTE_TIMEOUT_SECS;
    };
    if raw.is_empty() {
        return DEFAULT_FIRST_BYTE_TIMEOUT_SECS;
    }
    match raw.parse::<u64>() {
        Ok(val) if (MIN_FIRST_BYTE_TIMEOUT_SECS..=MAX_FIRST_BYTE_TIMEOUT_SECS).contains(&val) => {
            val
        }
        _ => {
            warn_invalid_first_byte_timeout(raw);
            DEFAULT_FIRST_BYTE_TIMEOUT_SECS
        }
    }
}

/// 纯函数：根据新环境变量与旧版环境变量的纯输入解析预检等待时长（用于单测与实际运行时解耦）。
fn resolve_wait_window(
    first_byte_env: Option<&str>,
    legacy_wait_env: Option<&str>,
) -> Option<Duration> {
    // 旧版逃生口兼容：QD_PREFLIGHT_WAIT_SECS=0 整体跳过预检
    if let Some(legacy) = legacy_wait_env {
        if let Ok(sec) = legacy.trim().parse::<u64>() {
            if sec == 0 {
                return None;
            }
            // 若显式传入了旧版等待秒数（且新版环境变量未设置或为空），兼容旧版测试场景（如 qoder_gateway_e2e 的 1 秒）
            if first_byte_env.is_none() || first_byte_env.is_some_and(|s| s.is_empty()) {
                return Some(Duration::from_secs(sec));
            }
        }
    }

    let secs = parse_preflight_first_byte_secs(first_byte_env);
    Some(Duration::from_secs(secs))
}

/// 汇总错误文案时的单条 / 总预算（字符）。
///
/// 保留「多行都有份」的分配：非信封错误里第二行是 Jackson 的 `localizedMessage`，
/// 第三行才是业务文案（`Internal Server Error`）。若让第一条吃满整个预算，
/// 最可操作的那句会被挤掉。
const DETAIL_BUDGET: usize = 200;
const TOTAL_BUDGET: usize = 500;

/// 预检的等待窗口覆盖（`BUDDY_SWITCH_QODER_PREFLIGHT_FIRST_BYTE_SECS` 与 `QD_PREFLIGHT_WAIT_SECS`）。
///
/// - `QD_PREFLIGHT_WAIT_SECS=0` ⇒ **整体跳过预检**（发完就回 200，最老的行为；排查用逃生口）；
/// - `BUDDY_SWITCH_QODER_PREFLIGHT_FIRST_BYTE_SECS` ⇒ 覆盖首字节预检等待秒数（1..=120，默认 25）；
/// - 未设置新变量且设置旧变量时，兼容既有旧版配置（如 1 秒测试窗口）。
fn wait_window() -> Option<Duration> {
    let first_byte_env = std::env::var(ENV_PREFLIGHT_FIRST_BYTE_SECS).ok();
    let legacy_env = std::env::var("QD_PREFLIGHT_WAIT_SECS").ok();
    resolve_wait_window(first_byte_env.as_deref(), legacy_env.as_deref())
}

/// 流式请求的预检策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamMode {
    /// 默认：错误帧先于正文到达 ⇒ 回非 2xx（客户端会重试）。
    Reject,
    /// 逃生口 `QD_PREFLIGHT_STREAM_MODE=inband`：恢复「200 + 帧内错误」。
    Inband,
}

fn stream_mode() -> StreamMode {
    match std::env::var("QD_PREFLIGHT_STREAM_MODE") {
        Ok(value)
            if matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "inband" | "in_band" | "200"
            ) =>
        {
            StreamMode::Inband
        }
        _ => StreamMode::Reject,
    }
}

/// 一行上游 SSE 的**预检**结论（与 `sse::process_line` 的转发结论是两回事）。
enum Inspection {
    /// 还没到决断点（注释 / 空行 / 角色帧 / 无内容 delta）。
    Continue,
    /// 已经出现正文或工具调用 ⇒ 可以提交 200。
    Ready,
    /// 上游错误帧。
    Error { status: u16, detail: String },
    /// 上游在**没有任何正文**的情况下宣告结束（`[DONE]` / `finish_reason`）⇒ 空补全。
    EmptyEnd,
}

fn inspect(line: &str) -> Inspection {
    match api::unwrap_envelope_line(line) {
        ChatFrame::Skip => Inspection::Continue,
        ChatFrame::Done => Inspection::EmptyEnd,
        ChatFrame::UpstreamError { status, detail } => Inspection::Error { status, detail },
        ChatFrame::Chunk(text) => {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if let Some(error) = value.get("error").filter(|value| !value.is_null()) {
                    return Inspection::Error {
                        status: 502,
                        detail: error.to_string(),
                    };
                }
                for choice in value
                    .get("choices")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let delta = &choice["delta"];
                    let has_text = ["content", "reasoning_content"].iter().any(|key| {
                        delta[*key].as_str().is_some_and(|text| !text.is_empty())
                            || delta[*key]
                                .as_array()
                                .is_some_and(|items| !items.is_empty())
                    });
                    if has_text
                        || delta["tool_calls"]
                            .as_array()
                            .is_some_and(|items| !items.is_empty())
                    {
                        return Inspection::Ready;
                    }
                    if choice["finish_reason"]
                        .as_str()
                        .is_some_and(|reason| !reason.is_empty())
                    {
                        return Inspection::EmptyEnd;
                    }
                }
            }
            Inspection::Continue
        }
    }
}

/// 上游错误的**累计证据**。
///
/// 为什么不能「见到第一行错误就返回」：非信封错误的可读文案是**多行**的 ——
/// `event:error` 行只有标记，Jackson 的 `localizedMessage` 在第二行，
/// 业务行 `{"success":false,…,"msgInfo":"Internal Server Error"}` 在第三行
/// （真机取证 2026-10-17，见 `docs/specs/QODER-PLATFORM.md` §7.1）。
/// 只取第一行会把客户端可见文案退化成一句「上游以 event:error 结束」，
/// 把 `Internal Server Error` 这种可操作信息丢掉 —— 那正是用户拿不到排查线索的原因。
#[derive(Default)]
struct ErrorEvidence {
    /// 第一行错误帧给出的（已规范化的）HTTP 状态。
    status: Option<u16>,
    details: Vec<String>,
}

impl ErrorEvidence {
    /// 记一行错误证据。返回 `Some` ⇒ **必须立刻决断**（排队 / 限流要带 `Retry-After`，
    /// 等不到后面几行）。
    fn push(&mut self, status: u16, detail: &str) -> Option<UpstreamFailure> {
        let failure = UpstreamFailure::from_detail(status, detail);
        if failure.retry_after.is_some() {
            return Some(failure);
        }
        if self.status.is_none() {
            self.status = Some(failure.status);
        }
        let detail = detail.trim();
        if !detail.is_empty() && !self.details.iter().any(|known| known == detail) {
            self.details.push(detail.to_string());
        }
        None
    }

    /// 汇总成客户端可见的失败：**状态取第一帧，文案取全部帧**。
    fn failure(&self) -> Option<UpstreamFailure> {
        let status = self.status?;
        let mut joined = String::new();
        for detail in &self.details {
            if joined.chars().count() >= TOTAL_BUDGET {
                break;
            }
            let piece: String = detail.chars().take(DETAIL_BUDGET).collect();
            if piece.is_empty() {
                continue;
            }
            if !joined.is_empty() {
                joined.push('；');
            }
            joined.push_str(&piece);
        }
        let joined: String = joined.chars().take(TOTAL_BUDGET).collect();
        Some(UpstreamFailure {
            status,
            kind: if status == 429 {
                "rate_limit_error"
            } else {
                "upstream_error"
            },
            call_count_limit: false,
            message: format!("Qoder 上游错误（HTTP {status}）：{joined}"),
            retry_after: None,
        })
    }
}

/// 把上游字节流统一成 [`RawUpstream`]（读取失败只保留一句话，不把上游细节带到日志）。
fn normalize<S, E>(upstream: S) -> RawUpstream
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Debug,
{
    Box::pin(upstream.map(|result| result.map_err(|_| io::Error::other("Qoder 上游流读取失败"))))
}

/// 不预检直接透传（`QD_PREFLIGHT_WAIT_SECS=0` 与 `inband` 逃生口共用）。
fn pass_through<S, E>(upstream: S) -> RawUpstream
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Debug,
{
    normalize(upstream)
}

/// 放行：把**已经读掉**的前缀原样接回流首，后续字节继续从上游取。
/// 逐字节等价于「没做预检」，因此 fail open 不会改写响应体。
fn release_prefix(prefix: Vec<Result<Bytes, io::Error>>, upstream: RawUpstream) -> RawUpstream {
    Box::pin(futures_util::stream::iter(prefix).chain(upstream))
}

/// 预检：读到首个有效上游增量（或**已经到达**的错误证据）为止，再决定能否提交 200。
///
/// - `Err(UpstreamFailure)` ⇒ 上游在正文之前就失败了（调用方据此回非 2xx）；
/// - `Ok(stream)` ⇒ 可以提交 200；`stream` 已把预读字节原样接回，**不会丢字节**。
pub async fn prepare<S, E>(upstream: S) -> Result<RawUpstream, UpstreamFailure>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Debug,
{
    match wait_window() {
        Some(wait) => prepare_with(upstream, Some(wait)).await,
        None => Ok(pass_through(upstream)),
    }
}

/// 流式请求（`stream: true`）的预检入口。
///
/// 与非流式的差别只有一处逃生口：`QD_PREFLIGHT_STREAM_MODE=inband` 时**完全不预检**，
/// 恢复「先回 200、错误只在帧里」的旧行为。默认 `reject` = 本文件模块头第 1 条。
pub async fn prepare_stream<S, E>(upstream: S) -> Result<RawUpstream, UpstreamFailure>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Debug,
{
    if stream_mode() == StreamMode::Inband {
        return Ok(pass_through(upstream));
    }
    prepare(upstream).await
}

/// 根据配置秒数构建 preflight 等待窗口 Option<Duration>。
fn wait_window_from_first_byte_secs(secs: u64) -> Option<Duration> {
    Some(Duration::from_secs(secs))
}

/// 预检的可配置超时秒数入口（私有内部路径，供自定义秒数直接验证）。
#[allow(dead_code)]
async fn prepare_with_first_byte_secs<S, E>(
    upstream: S,
    secs: u64,
) -> Result<RawUpstream, UpstreamFailure>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Debug,
{
    prepare_with(upstream, wait_window_from_first_byte_secs(secs)).await
}

/// 预检的**等待窗口可注入**版本（单测用短窗口，不依赖环境变量，天然并发安全）。
async fn prepare_with<S, E>(
    upstream: S,
    wait: Option<Duration>,
) -> Result<RawUpstream, UpstreamFailure>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Debug,
{
    let mut upstream = normalize(upstream);
    let mut prefix: Vec<Result<Bytes, io::Error>> = Vec::new();
    let mut buffer = Vec::new();
    let mut size = 0usize;
    let mut evidence = ErrorEvidence::default();
    let deadline = wait.map(|wait| tokio::time::Instant::now() + wait);

    /// 一轮取数的结果。
    enum Step {
        Item(Option<Result<Bytes, io::Error>>),
        /// 等待窗口到点：预检放弃继续扣留。
        WindowElapsed,
    }

    loop {
        let step = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, upstream.next()).await {
                Ok(item) => Step::Item(item),
                Err(_elapsed) => Step::WindowElapsed,
            },
            None => Step::Item(upstream.next().await),
        };
        let next = match step {
            Step::Item(item) => item,
            // ★ 硬约束 2：窗口到点而流仍然健康 ⇒ fail open（前缀原样回放，继续 200）。
            //   只有**已经拿到**错误证据时才允许在这里否决。
            Step::WindowElapsed => {
                return match evidence.failure() {
                    Some(failure) => Err(failure),
                    None => Ok(release_prefix(prefix, upstream)),
                }
            }
        };
        match next {
            Some(Ok(chunk)) => {
                size += chunk.len();
                buffer.extend_from_slice(&chunk);
                prefix.push(Ok(chunk));
                while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=end).collect();
                    match inspect(&String::from_utf8_lossy(&line)) {
                        Inspection::Continue => {}
                        Inspection::Ready => {
                            return match evidence.failure() {
                                // 错误帧先于正文到达 ⇒ 非 2xx（客户端据此重试）。
                                Some(failure) => Err(failure),
                                None => Ok(release_prefix(prefix, upstream)),
                            };
                        }
                        Inspection::Error { status, detail } => {
                            if let Some(failure) = evidence.push(status, &detail) {
                                return Err(failure);
                            }
                        }
                        Inspection::EmptyEnd => {
                            return Err(evidence.failure().unwrap_or_else(UpstreamFailure::empty))
                        }
                    }
                }
                // ★ 硬约束 3：前缀超限 ⇒ fail open（已缓冲字节一个不少地回放）。
                if size > PREFIX_LIMIT {
                    return match evidence.failure() {
                        Some(failure) => Err(failure),
                        None => Ok(release_prefix(prefix, upstream)),
                    };
                }
            }
            Some(Err(_)) => {
                if let Some(failure) = evidence.failure() {
                    return Err(failure);
                }
                return Err(UpstreamFailure {
                    status: 502,
                    kind: "upstream_error",
                    message: "Qoder 上游在首个有效响应前中断。".into(),
                    call_count_limit: false,
                    retry_after: None,
                });
            }
            None => {
                // 上游自然结束：先处理残余缓冲（末行可能没有换行符）。
                let tail = if buffer.is_empty() {
                    Inspection::Continue
                } else {
                    inspect(&String::from_utf8_lossy(&buffer))
                };
                match tail {
                    Inspection::Error { status, detail } => {
                        evidence.push(status, &detail);
                    }
                    Inspection::Ready => {
                        // 最后一帧就是正文：前缀完整，可以提交成功。
                        if evidence.failure().is_none() {
                            return Ok(release_prefix(prefix, upstream));
                        }
                    }
                    Inspection::Continue | Inspection::EmptyEnd => {}
                }
                return Err(evidence.failure().unwrap_or_else(UpstreamFailure::empty));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bytes(text: &str) -> Result<Bytes, io::Error> {
        Ok(Bytes::from(text.to_string()))
    }

    async fn drain(mut stream: RawUpstream) -> Vec<u8> {
        let mut all = Vec::new();
        while let Some(chunk) = stream.next().await {
            all.extend_from_slice(&chunk.expect("预检不得改写字节"));
        }
        all
    }

    #[tokio::test]
    async fn queue_after_role_is_an_http_error_before_streaming() {
        let error = json!({"code":"403","message":json!({"code":"10605","message":json!({"isQueued":true,"retryAfterSeconds":30,"subCode":"CALL_COUNT_LIMIT"}).to_string()}).to_string()});
        let role = json!({"body":json!({"choices":[{"delta":{"role":"assistant"}}]}).to_string(),"statusCodeValue":200});
        let text = format!(
            "data: {role}\n\ndata: {}\n",
            json!({"body":error.to_string(),"statusCodeValue":403})
        );
        let result = prepare(futures_util::stream::iter(vec![Ok::<_, io::Error>(
            Bytes::from(text),
        )]))
        .await;
        let error = result.err().expect("must reject before HTTP 200");
        assert_eq!(error.status, 429);
        assert_eq!(error.retry_after, Some(30));
    }

    #[tokio::test]
    async fn valid_prefix_is_replayed_byte_for_byte() {
        let text="data: {\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"ok\\\"}}]}\",\"statusCodeValue\":200}\n\ndata: {\"body\":\"[DONE]\"}\n";
        let chunks: Vec<_> = text
            .as_bytes()
            .chunks(7)
            .map(|s| Ok::<_, io::Error>(Bytes::copy_from_slice(s)))
            .collect();
        let mut prepared = prepare(futures_util::stream::iter(chunks)).await.unwrap();
        let mut all = Vec::new();
        while let Some(chunk) = prepared.next().await {
            all.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(all, text.as_bytes());
    }

    #[tokio::test]
    async fn empty_or_broken_upstream_never_returns_success() {
        for text in ["", "data: {\"body\":\"[DONE]\"}\n"] {
            assert_eq!(
                prepare(futures_util::stream::iter(vec![Ok::<_, io::Error>(
                    Bytes::from(text)
                )]))
                .await
                .err()
                .unwrap()
                .status,
                502
            );
        }
        assert_eq!(
            prepare(futures_util::stream::iter(vec![Err::<Bytes, _>(
                io::Error::other("broken")
            )]))
            .await
            .err()
            .unwrap()
            .status,
            502
        );
    }

    /// 真机形态（2026-10-17，Qoder CN）：`event:error` + Jackson 堆栈 +
    /// `{"success":false,…,"msgInfo":"Internal Server Error"}`。
    ///
    /// 判据不是「有没有错误」而是「**可操作文案有没有留下**」：三行都得进客户端可见文案，
    /// 否则用户只看到一句「上游以 event:error 结束」，排查方向被带偏（issue #46 的原始症状）。
    #[tokio::test]
    async fn non_envelope_error_keeps_every_actionable_line() {
        let text = concat!(
            "event:error\n",
            "data:{\"stackTrace\":[{\"className\":\"com.fasterxml.jackson.core.JsonParser\"}],\"localizedMessage\":\"Unrecognized token 'dwmxdL'\"}\n",
            "{\"success\":false,\"msgCode\":500,\"msgInfo\":\"Internal Server Error\"}\n",
        );
        let failure = prepare(futures_util::stream::iter(vec![bytes(text)]))
            .await
            .err()
            .expect("非信封错误不得回 200");
        assert_eq!(failure.status, 502, "上游外层是 HTTP 200，失败归网关 ⇒ 502");
        assert!(
            failure.message.contains("Unrecognized token"),
            "reason 必须留下：{}",
            failure.message
        );
        assert!(
            failure.message.contains("Internal Server Error"),
            "业务文案必须留下（旧实现只留第一行 ⇒ 丢掉这句）：{}",
            failure.message
        );
        // 凭据红线：错误文案里不得出现任何令牌形态。
        assert!(!failure.message.contains("dt-"), "{}", failure.message);
        assert!(!failure.message.contains("drt-"), "{}", failure.message);
    }

    /// ★ 长输入回归的**核心**用例：首 token 慢于预检窗口 ≠ 上游故障。
    ///
    /// 预检只负责「提前发现已经到达的错误」，不是首 token 截止时间。窗口到点必须
    /// fail open：**200 + 逐字节完整的响应体**（含窗口之后才到的尾巴）。
    #[tokio::test]
    async fn slow_but_healthy_upstream_fails_open_with_identical_bytes() {
        let head = "data: {\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"late\\\"}}]}\",\"statusCodeValue\":200}\n";
        let tail = "data:{\"body\":\"[DONE]\"}\n";
        // 首块刻意晚于 40ms 的窗口；第二块证明「窗口之后到达的字节不会丢」。
        let upstream = futures_util::stream::unfold(0usize, move |index| async move {
            match index {
                0 => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Some((bytes(head), 1))
                }
                1 => Some((bytes(tail), 2)),
                _ => None,
            }
        });
        let prepared = prepare_with(upstream, Some(Duration::from_millis(40)))
            .await
            .expect("慢但健康的上游必须 fail open，不能报错");
        let mut expected = String::from(head);
        expected.push_str(tail);
        assert_eq!(drain(prepared).await, expected.as_bytes());
    }

    /// 窗口到点**且**已经拿到错误证据 ⇒ 仍然否决（fail open 不能变成「吞掉真错误」）。
    #[tokio::test]
    async fn expired_window_still_rejects_when_error_evidence_already_arrived() {
        let error = "data: {\"body\":\"boom\",\"statusCodeValue\":500}\n";
        let upstream = futures_util::stream::once(async move { bytes(error) }).chain(
            futures_util::stream::once(async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                bytes("")
            }),
        );
        let failure = prepare_with(upstream, Some(Duration::from_millis(40)))
            .await
            .err()
            .expect("已经拿到错误帧就必须否决");
        assert_eq!(failure.status, 500);
    }

    /// 前缀超限（> 256 KiB 仍无正文）⇒ fail open，字节一个不少。
    #[tokio::test]
    async fn oversized_prefix_fails_open_without_dropping_bytes() {
        let noise = ": keep-alive\n".repeat(400 * 1024 / ": keep-alive\n".len() + 1);
        assert!(noise.len() > PREFIX_LIMIT);
        let tail = "data: {\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"ok\\\"}}]}\",\"statusCodeValue\":200}\n";
        let upstream = futures_util::stream::iter(vec![bytes(&noise), bytes(tail)]);
        let prepared = prepare_with(upstream, Some(Duration::from_secs(30)))
            .await
            .expect("超限必须 fail open，不是报错");
        let mut expected = noise.clone();
        expected.push_str(tail);
        assert_eq!(drain(prepared).await, expected.as_bytes());
    }

    /// 上游真的空结束（`[DONE]` 但一个正文都没有）⇒ 仍然不许当成功（issue #46 的另一半）。
    #[tokio::test]
    async fn done_without_any_delta_is_still_a_failure() {
        let upstream = futures_util::stream::iter(vec![bytes(
            "data: {\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{},\\\"finish_reason\\\":\\\"stop\\\"}]}\",\"statusCodeValue\":200}\n",
        )]);
        let failure = prepare_with(upstream, Some(Duration::from_secs(30)))
            .await
            .err()
            .expect("空补全不得回 200");
        assert_eq!(failure.status, 502);
    }

    #[test]
    fn default_first_byte_timeout_when_env_unset_is_25() {
        assert_eq!(parse_preflight_first_byte_secs(None), 25);
        assert_eq!(
            resolve_wait_window(None, None),
            Some(Duration::from_secs(25))
        );
    }

    #[test]
    fn valid_first_byte_timeout_values_within_range() {
        assert_eq!(parse_preflight_first_byte_secs(Some("1")), 1);
        assert_eq!(parse_preflight_first_byte_secs(Some("25")), 25);
        assert_eq!(parse_preflight_first_byte_secs(Some("120")), 120);
    }

    #[test]
    fn invalid_and_out_of_bound_first_byte_timeout_matrix_falls_back_to_25() {
        // 非法/越界值矩阵测试。
        // 解析语义说明：采用严格解析，不剔除首尾空白。
        // " 25 " 含空白，不符合纯整数字符串格式，回落至 25 并触发告警；
        // ""（空串）视为未设置环境变量，静默回落至 25。
        let test_cases = [
            ("0", 25),
            ("-1", 25),
            ("121", 25),
            ("1000", 25),
            ("abc", 25),
            ("", 25),
            ("25.5", 25),
            (" 25 ", 25),
        ];

        for (input, expected) in test_cases {
            assert_eq!(
                parse_preflight_first_byte_secs(Some(input)),
                expected,
                "input {:?} must fall back to {}",
                input,
                expected
            );
        }
    }

    #[tokio::test]
    async fn custom_first_byte_timeout_secs_applies_to_preflight() {
        // 1. 验证解析秒数与 Duration 构造路径
        let parsed_secs = parse_preflight_first_byte_secs(Some("10"));
        assert_eq!(parsed_secs, 10);
        let duration = wait_window_from_first_byte_secs(parsed_secs);
        assert_eq!(duration, Some(Duration::from_secs(10)));

        // 2. 验证注入自定义超时到 preflight 路径中，正常 upstream 可正常通过并完整回放
        let text = "data: {\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"custom_ok\\\"}}]}\",\"statusCodeValue\":200}\n";
        let upstream = futures_util::stream::iter(vec![bytes(text)]);
        let prepared = prepare_with_first_byte_secs(upstream, parsed_secs)
            .await
            .expect("自定义秒数下 preflight 正常流必须通过");
        assert_eq!(drain(prepared).await, text.as_bytes());

        // 3. 验证超时场景下自定义时长实际生效并 fail open
        let head = "data: {\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"late_stream\\\"}}]}\",\"statusCodeValue\":200}\n";
        let tail = "data:{\"body\":\"[DONE]\"}\n";
        let slow_upstream = futures_util::stream::unfold(0usize, move |index| async move {
            match index {
                0 => {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Some((bytes(head), 1))
                }
                1 => Some((bytes(tail), 2)),
                _ => None,
            }
        });
        // 传入短于首块到达时间的超时（例如 40ms），验证超时窗口触发 fail open 且内容完整
        let prepared_slow = prepare_with(slow_upstream, Some(Duration::from_millis(40)))
            .await
            .expect("超时后必须 fail open");
        let mut expected = String::from(head);
        expected.push_str(tail);
        assert_eq!(drain(prepared_slow).await, expected.as_bytes());
    }

    #[test]
    fn resolve_wait_window_preserves_legacy_escape_hatch_and_precedence() {
        // QD_PREFLIGHT_WAIT_SECS=0 逃生口整体跳过预检
        assert_eq!(resolve_wait_window(None, Some("0")), None);
        assert_eq!(resolve_wait_window(Some(""), Some("0")), None);

        // 旧版测试 QD_PREFLIGHT_WAIT_SECS=1 在新变量未设置时依然生效
        assert_eq!(
            resolve_wait_window(None, Some("1")),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            resolve_wait_window(Some(""), Some("1")),
            Some(Duration::from_secs(1))
        );

        // 新变量显式设置时，覆盖旧变量设置
        assert_eq!(
            resolve_wait_window(Some("42"), Some("1")),
            Some(Duration::from_secs(42))
        );
    }
}
