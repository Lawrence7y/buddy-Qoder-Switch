//! Qoder business failures can arrive inside an HTTP-200 stream. In particular
//! 403 + 10605/CALL_COUNT_LIMIT is model queuing, not a dead access credential.
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct UpstreamFailure {
    pub status: u16,
    pub kind: &'static str,
    pub message: String,
    pub retry_after: Option<u64>,
    /// `10605 / CALL_COUNT_LIMIT`：**账号级**调用次数限制（免费模型限额）。
    /// 换号可能仍有余量 ⇒ 无论建议等待多久都值得轮换；与模型级深队列相反。
    pub call_count_limit: bool,
}

#[derive(Default)]
struct QueueFields {
    queued: bool,
    limited: bool,
    retry: u64,
    wait: u64,
    count: Option<u64>,
}

fn inspect(value: &Value, depth: usize, fields: &mut QueueFields) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Object(map) => {
            fields.queued |= map.get("isQueued").and_then(Value::as_bool) == Some(true);
            fields.limited |= map.get("subCode").and_then(Value::as_str)
                == Some("CALL_COUNT_LIMIT")
                || map.get("code").and_then(Value::as_str) == Some("10605")
                || map.get("code").and_then(Value::as_u64) == Some(10605);
            fields.retry = fields.retry.max(
                map.get("retryAfterSeconds")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            fields.wait = fields
                .wait
                .max(map.get("waitTime").and_then(Value::as_u64).unwrap_or(0));
            fields.count = map
                .get("queueCount")
                .and_then(Value::as_u64)
                .or(fields.count);
            for value in map.values() {
                inspect(value, depth + 1, fields);
            }
        }
        Value::String(text) if text.len() < 64 * 1024 => {
            if let Ok(value) = serde_json::from_str::<Value>(text) {
                inspect(&value, depth + 1, fields);
            }
        }
        _ => {}
    }
}

impl UpstreamFailure {
    pub fn from_detail(status: u16, detail: &str) -> Self {
        let mut fields = QueueFields::default();
        if let Ok(value) = serde_json::from_str(detail) {
            inspect(&value, 0, &mut fields);
        }
        if fields.queued || fields.limited {
            let declared = fields.retry.max(fields.wait);
            let seconds = if declared == 0 {
                60
            } else {
                declared.clamp(1, 3600)
            };
            let queue = fields
                .count
                .map(|n| format!("，排队 {n} 个请求"))
                .unwrap_or_default();
            // ★ CALL_COUNT_LIMIT 是**账号级**调用次数限制（免费模型每日/短窗限额）：
            //   池里换个账号可能仍有余量 —— 与「模型级深队列」的处置完全不同
            //   （后者换号无用）。时长也不代表不可换：hours 级意味着「这个号今天
            //   到顶了」，恰好该轮到下一个号。
            return Self { status:429, kind:"rate_limit_error", call_count_limit: fields.limited,
                retry_after:Some(seconds),
                message:format!("Qoder 模型正在排队/限流（10605 / CALL_COUNT_LIMIT{queue}），请等待 {seconds} 秒后重试；尚未生成回复。") };
        }
        Self {
            status: if (400..=599).contains(&status) {
                status
            } else {
                502
            },
            kind: if status == 429 {
                "rate_limit_error"
            } else {
                "upstream_error"
            },
            message: format!(
                "Qoder 上游错误（HTTP {status}）：{}",
                detail.chars().take(400).collect::<String>()
            ),
            call_count_limit: false,
            retry_after: None,
        }
    }
    pub fn empty() -> Self {
        Self {
            status: 502,
            kind: "upstream_error",
            message: "Qoder 上游在生成正文或工具调用前结束，不能作为成功回复。".into(),
            call_count_limit: false,
            retry_after: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn nested_queue_403_is_rate_limit_not_bad_auth() {
        let inner = json!({"isQueued":true,"queueCount":2796,"waitTime":170,"retryAfterSeconds":30,"subCode":"CALL_COUNT_LIMIT"});
        let mid = json!({"code":"10605","message":inner.to_string(),"subCode":"CALL_COUNT_LIMIT"});
        let outer = json!({"code":"403","message":mid.to_string()});
        let error = UpstreamFailure::from_detail(403, &outer.to_string());
        assert_eq!(error.status, 429);
        assert_eq!(error.retry_after, Some(170));
        assert_eq!(error.kind, "rate_limit_error");
        assert!(error.message.contains("2796"));
        assert_eq!(
            UpstreamFailure::from_detail(403, r#"{"message":"invalid credential"}"#).status,
            403
        );
    }
}
