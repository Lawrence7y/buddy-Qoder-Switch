//! 工具调用**文本泄漏**的宽松还原（DeepSeek / dfmodel 截断问题的网关侧兜底）。
//!
//! ## 症状（ZCode 实拍，`qoder-cn:dfmodel`）
//!
//! ```text
//! 快照逻辑与数据库都对上了。我再取一次实时快照…… [assistant 请求调用工具]
//! [{"arguments":"{\"command\":\"…\"}","name":"Bash"}, …]</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜function_calls>
//! ```
//!
//! 模型把工具调用写进了**正文**：客户端收到 `finish_reason=stop` + 一段文本，
//! 以为本轮结束 ⇒ Agent 循环「截断」。原有守卫 [`api::parse_leaked_tool_calls`]
//! 只认「**整段**恰好是 marker + JSON 数组」，以下三种真实形态全部漏网：
//!
//! 1. marker **前面有说明文字**（上图）；
//! 2. JSON 数组**后面拖着 DeepSeek 原生 DSML 收尾标签**（上图）；
//! 3. 模型直接吐**原生 DSML 块**：`<｜DSML｜function_calls><｜DSML｜invoke name="Bash">
//!    <｜DSML｜parameter name="command" string="true">ls</｜DSML｜parameter>…`。
//!
//! ## 安全边界（宁漏不误）
//!
//! - 先走原有严格判据（行为不变）；
//! - 宽松判据**只在请求声明了工具**（`allowed` 非空）时启用，且每个调用名都必须
//!   命中声明集合、参数必须是合法 JSON —— 普通「讨论这个标记」的回复不会被误判；
//! - 还原成功时：marker 之前的说明文字保留为正文，DSML 残标签剥掉。

use std::collections::HashSet;
use std::sync::OnceLock;

use buddy_switch_core::modules::qoder::api;
use regex::Regex;
use serde_json::{json, Map, Value};

/// DSML 起始标记（全角 `｜` 为 DeepSeek 原生形态，半角为部分渲染/转写形态）。
pub const DSML_MARKERS: [&str; 2] = ["<｜DSML｜", "<|DSML|"];

fn dsml_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"<\s*(/?)\s*[｜|]\s*DSML\s*[｜|]\s*([A-Za-z_]+)([^>]*)>").expect("DSML 正则")
    })
}

fn attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"([A-Za-z_]+)\s*=\s*"([^"]*)""#).expect("属性正则"))
}

/// 剥掉文本里所有 DSML 标签（残留收尾标签清理用）。
pub fn strip_dsml_tags(text: &str) -> String {
    dsml_tag_re().replace_all(text, "").into_owned()
}

fn allowed_ok(name: &str, allowed: &HashSet<String>) -> bool {
    !name.trim().is_empty() && allowed.contains(name.trim())
}

fn new_call(name: &str, arguments: String) -> Value {
    json!({
        "id": format!("call_{}", uuid::Uuid::new_v4().simple()),
        "type": "function",
        "function": { "name": name.trim(), "arguments": arguments },
    })
}

/// 把 marker 后的 JSON 数组条目校验并规整成 OpenAI `tool_calls`。
fn calls_from_array(items: &[Value], allowed: &HashSet<String>) -> Option<Vec<Value>> {
    if items.is_empty() {
        return None;
    }
    let mut calls = Vec::new();
    for item in items {
        let object = item.as_object()?;
        // 兼容 `{name, arguments}` 与 OpenAI 形 `{function:{name, arguments}}`。
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .unwrap_or(object);
        let name = function.get("name").and_then(Value::as_str)?;
        if !allowed_ok(name, allowed) {
            return None;
        }
        let arguments = match function.get("arguments").or_else(|| function.get("input")) {
            Some(Value::String(text)) => text.clone(),
            Some(other @ (Value::Object(_) | Value::Array(_))) => {
                serde_json::to_string(other).unwrap_or_default()
            }
            None | Some(Value::Null) => "{}".to_string(),
            _ => return None,
        };
        if !arguments.trim().is_empty() && serde_json::from_str::<Value>(&arguments).is_err() {
            return None;
        }
        calls.push(new_call(name, arguments));
    }
    Some(calls)
}

fn join_text(before: &str, after: &str) -> String {
    let before = before.trim_end();
    let after = after.trim();
    match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (false, true) => before.to_string(),
        (true, false) => after.to_string(),
        (false, false) => format!("{before}\n{after}"),
    }
}

/// 形态 1+2：正文任意位置的 `LEAK_MARKER` + JSON 数组（可带围栏 / DSML 残标签）。
fn recover_marker(text: &str, allowed: &HashSet<String>) -> Option<(Vec<Value>, String)> {
    let pos = text.find(api::LEAK_MARKER)?;
    let before = &text[..pos];
    let mut rest = text[pos + api::LEAK_MARKER.len()..].trim_start();
    if let Some(stripped) = rest.strip_prefix("```") {
        rest = stripped.trim_start();
        if rest
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("json"))
        {
            rest = rest[4..].trim_start();
        }
    }
    if !rest.starts_with('[') {
        return None;
    }
    let mut iter = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    let array = iter.next()?.ok()?;
    let consumed = iter.byte_offset();
    let calls = calls_from_array(array.as_array()?, allowed)?;
    let mut tail = rest[consumed..].trim_start();
    if let Some(stripped) = tail.strip_prefix("```") {
        tail = stripped;
    }
    let tail = strip_dsml_tags(tail);
    Some((calls, join_text(before, &tail)))
}

/// 形态 3：DeepSeek 原生 DSML 工具调用块。
fn recover_dsml(text: &str, allowed: &HashSet<String>) -> Option<(Vec<Value>, String)> {
    let re = dsml_tag_re();
    let mut block_start: Option<usize> = None;
    let mut block_end: Option<usize> = None;
    let mut calls = Vec::new();
    let mut invoke: Option<(String, Map<String, Value>)> = None;
    let mut param: Option<(String, bool, usize)> = None;

    for caps in re.captures_iter(text) {
        let whole = caps.get(0)?;
        let closing = !caps[1].is_empty();
        let tag = caps[2].to_ascii_lowercase();
        let attrs: Map<String, Value> = attr_re()
            .captures_iter(&caps[3])
            .map(|a| (a[1].to_string(), Value::String(a[2].to_string())))
            .collect();
        match (tag.as_str(), closing) {
            ("function_calls" | "tool_calls", false) => {
                block_start.get_or_insert(whole.start());
            }
            ("function_calls" | "tool_calls", true) => {
                block_end = Some(whole.end());
            }
            ("invoke", false) => {
                if invoke.is_some() || param.is_some() {
                    return None;
                }
                block_start.get_or_insert(whole.start());
                let name = attrs.get("name").and_then(Value::as_str)?.to_string();
                invoke = Some((name, Map::new()));
            }
            ("invoke", true) => {
                if param.is_some() {
                    return None;
                }
                let (name, args) = invoke.take()?;
                if !allowed_ok(&name, allowed) {
                    return None;
                }
                calls.push(new_call(&name, Value::Object(args).to_string()));
                block_end = Some(whole.end());
            }
            ("parameter", false) => {
                if invoke.is_none() || param.is_some() {
                    return None;
                }
                let name = attrs.get("name").and_then(Value::as_str)?.to_string();
                let is_string = attrs
                    .get("string")
                    .and_then(Value::as_str)
                    .map(|v| !v.eq_ignore_ascii_case("false"))
                    .unwrap_or(true);
                param = Some((name, is_string, whole.end()));
            }
            ("parameter", true) => {
                let Some((name, is_string, start)) = param.take() else {
                    continue; // 孤立收尾标签（形态 2 的残留）忽略
                };
                let raw = &text[start..whole.start()];
                let value = if is_string {
                    Value::String(raw.to_string())
                } else {
                    serde_json::from_str::<Value>(raw.trim()).ok()?
                };
                if let Some((_, args)) = invoke.as_mut() {
                    if args.insert(name, value).is_some() {
                        return None;
                    }
                }
            }
            _ => {}
        }
    }
    if calls.is_empty() || invoke.is_some() || param.is_some() {
        return None;
    }
    let start = block_start?;
    let end = block_end.unwrap_or(text.len()).max(start);
    let tail = strip_dsml_tags(&text[end..]);
    Some((calls, join_text(&text[..start], &tail)))
}

/// 统一入口：先严格（原行为），再宽松（仅声明了工具时）。
///
/// 返回 `(tool_calls, clean_text)`，语义与 [`api::parse_leaked_tool_calls`] 相同。
pub fn recover_tool_calls(
    text: &str,
    allowed: Option<&HashSet<String>>,
) -> (Option<Vec<Value>>, String) {
    let (strict, cleaned) = api::parse_leaked_tool_calls(text, allowed);
    if strict.is_some() {
        return (strict, cleaned);
    }
    let Some(allowed) = allowed.filter(|set| !set.is_empty()) else {
        return (None, text.to_string());
    };
    if let Some((calls, clean)) = recover_marker(text, allowed) {
        return (Some(calls), clean);
    }
    if let Some((calls, clean)) = recover_dsml(text, allowed) {
        return (Some(calls), clean);
    }
    (None, text.to_string())
}

/// 所有「可能开启一段泄漏」的标记。
pub fn leak_markers() -> [&'static str; 3] {
    [api::LEAK_MARKER, DSML_MARKERS[0], DSML_MARKERS[1]]
}

/// 正文中**最早**可能开启泄漏的位置：完整标记出现处，或文本末尾恰是某标记前缀处
/// （跨帧分片）。返回字节下标（落在字符边界上）。
pub fn find_leak_start(text: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    for marker in leak_markers() {
        if let Some(pos) = text.find(marker) {
            best = Some(best.map_or(pos, |b: usize| b.min(pos)));
        }
        // 末尾部分前缀：text 的某个后缀 == marker 的前缀。
        for (idx, _) in text.char_indices() {
            let suffix = &text[idx..];
            if suffix.len() < marker.len() && marker.starts_with(suffix) {
                best = Some(best.map_or(idx, |b| b.min(idx)));
                break;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_fenced_prose_does_not_panic_or_become_a_call() {
        let text = "[assistant 请求调用工具]```中文示例，并非调用```";
        assert!(recover_tool_calls(text, Some(&tools())).0.is_none());
    }

    #[test]
    fn incomplete_or_invalid_dsml_must_not_execute_a_partial_call() {
        for text in [
            "<｜DSML｜invoke name=\"Bash\"><｜DSML｜parameter name=\"command\">ls</｜DSML｜invoke>",
            "<｜DSML｜invoke name=\"Bash\"><｜DSML｜parameter name=\"timeout\" string=\"false\">not_json</｜DSML｜parameter></｜DSML｜invoke>",
            "<｜DSML｜invoke name=\"Bash\">",
            "<｜DSML｜invoke name=\"Bash\"></｜DSML｜invoke><｜DSML｜invoke name=\"Read\">",
        ] { assert!(recover_tool_calls(text, Some(&tools())).0.is_none(), "{text}"); }
    }

    fn tools() -> HashSet<String> {
        ["Bash", "Read"].iter().map(|s| s.to_string()).collect()
    }

    /// ZCode 实拍形态：前置说明 + marker + JSON 数组 + DSML 收尾残标签。
    #[test]
    fn recovers_screenshot_shape() {
        let text = "快照逻辑与数据库都对上了。我再取一次实时快照。 [assistant 请求调用工具]\n\
[{\"arguments\":\"{\\\"command\\\":\\\"ls -la\\\",\\\"description\\\":\\\"x\\\"}\",\"name\":\"Bash\"},\
{\"arguments\":\"{\\\"command\\\":\\\"pwd\\\"}\",\"name\":\"Bash\"}]\
</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜function_calls>";
        let (calls, clean) = recover_tool_calls(text, Some(&tools()));
        let calls = calls.expect("必须还原");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["function"]["name"], "Bash");
        let args: Value =
            serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["command"], "ls -la");
        assert_eq!(clean, "快照逻辑与数据库都对上了。我再取一次实时快照。");
        assert!(!clean.contains("DSML"));
    }

    #[test]
    fn recovers_native_dsml_block() {
        let text = "先看看目录。\n<｜DSML｜function_calls>\n<｜DSML｜invoke name=\"Bash\">\n\
<｜DSML｜parameter name=\"command\" string=\"true\">ls -la</｜DSML｜parameter>\n\
<｜DSML｜parameter name=\"timeout\" string=\"false\">30</｜DSML｜parameter>\n\
</｜DSML｜invoke>\n</｜DSML｜function_calls>";
        let (calls, clean) = recover_tool_calls(text, Some(&tools()));
        let calls = calls.expect("必须还原 DSML");
        assert_eq!(calls.len(), 1);
        let args: Value =
            serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["command"], "ls -la");
        assert_eq!(args["timeout"], 30);
        assert_eq!(clean, "先看看目录。");
    }

    #[test]
    fn undeclared_tool_or_no_tools_is_not_recovered() {
        let text = "说明 [assistant 请求调用工具]\n[{\"name\":\"rm_rf\",\"arguments\":\"{}\"}]";
        assert!(recover_tool_calls(text, Some(&tools())).0.is_none());
        // 没声明工具 ⇒ 宽松判据不启用（只剩原严格判据，而这段不是整段形态）。
        assert!(recover_tool_calls(text, None).0.is_none());
    }

    #[test]
    fn plain_discussion_is_not_recovered() {
        let text = "日志里出现了 [assistant 请求调用工具] 这个标记，是什么意思？";
        let (calls, clean) = recover_tool_calls(text, Some(&tools()));
        assert!(calls.is_none());
        assert_eq!(clean, text);
    }

    #[test]
    fn strict_shape_still_works() {
        let text = "[assistant 请求调用工具]\n[{\"name\":\"Read\",\"arguments\":\"{}\"}]";
        let (calls, clean) = recover_tool_calls(text, Some(&tools()));
        assert_eq!(calls.unwrap().len(), 1);
        assert!(clean.is_empty());
    }

    #[test]
    fn find_leak_start_handles_mid_text_and_split_prefix() {
        assert_eq!(find_leak_start("hello"), None);
        assert_eq!(find_leak_start("ab [assistant 请求调用工具]"), Some(3));
        assert_eq!(find_leak_start("ab [assis"), Some(3));
        assert_eq!(find_leak_start("ab <｜DS"), Some(3));
        assert_eq!(find_leak_start("ab <|DSML|invoke"), Some(3));
    }
}
