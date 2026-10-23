//! Opt-in raw upstream diagnostics. Never executes tools or changes account/config files.
use buddy_switch_core::modules::{
    config,
    qoder::{
        self, account,
        api::{self, ChatFrame, ModelMeta},
        cosy,
        region::{self, QoderRegion},
    },
};
use buddy_switch_gateway::qoder::upstream_error::UpstreamFailure;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Observation {
    first_output_ms: Option<u64>,
    last_output_ms: Option<u64>,
    text_chars: usize,
    reasoning_chars: usize,
    bytes: usize,
    tools: BTreeMap<i64, Value>,
    finishes: Vec<Value>,
    usage: Option<Value>,
    output_after_finish: usize,
    done: bool,
    error_status: Option<u16>,
    frames: Vec<Value>,
}
impl Observation {
    fn line(&mut self, line: &str, ms: u64) {
        match api::unwrap_envelope_line(line) {
            ChatFrame::Skip => {}
            ChatFrame::Done => self.done = true,
            ChatFrame::UpstreamError { status, detail } => {
                self.error_status = Some(UpstreamFailure::from_detail(status, &detail).status);
            }
            ChatFrame::Chunk(text) => {
                let Ok(chunk) = serde_json::from_str::<Value>(&text) else {
                    return;
                };
                if let Some(error) = chunk.get("error").filter(|v| !v.is_null()) {
                    self.error_status =
                        Some(UpstreamFailure::from_detail(502, &error.to_string()).status);
                }
                if let Some(usage) = chunk.get("usage").filter(|v| !v.is_null()) {
                    self.usage = Some(usage.clone());
                }
                for choice in chunk["choices"].as_array().into_iter().flatten() {
                    let delta = &choice["delta"];
                    let text = delta["content"].as_str().unwrap_or("");
                    let reasoning = delta["reasoning_content"].as_str().unwrap_or("");
                    let calls = delta["tool_calls"].as_array();
                    let legacy = delta["function_call"].as_object().is_some();
                    let has_output = !text.is_empty()
                        || !reasoning.is_empty()
                        || calls.is_some_and(|v| !v.is_empty())
                        || legacy;
                    if has_output {
                        if !self.finishes.is_empty() {
                            self.output_after_finish += 1;
                        }
                        self.first_output_ms.get_or_insert(ms);
                        self.last_output_ms = Some(ms);
                    }
                    self.text_chars += text.chars().count();
                    self.reasoning_chars += reasoning.chars().count();
                    self.bytes += text.len() + reasoning.len();
                    for call in calls.into_iter().flatten() {
                        let index = call["index"].as_i64().unwrap_or(0);
                        let entry = self
                            .tools
                            .entry(index)
                            .or_insert(json!({"name":"","argumentBytes":0}));
                        if let Some(name) = call["function"]["name"].as_str() {
                            entry["name"] = json!(name);
                        }
                        if let Some(arguments) = call["function"]["arguments"].as_str() {
                            entry["argumentBytes"] = json!(
                                entry["argumentBytes"].as_u64().unwrap_or(0)
                                    + arguments.len() as u64
                            );
                            self.bytes += arguments.len();
                        }
                    }
                    let finish = choice["finish_reason"].as_str().filter(|v| !v.is_empty());
                    if let Some(reason) = finish {
                        self.finishes.push(json!({"ms":ms,"reason":reason}));
                    }
                    self.frames.push(json!({"ms":ms,"textChars":text.chars().count(),"reasoningChars":reasoning.chars().count(),"toolDeltas":calls.map_or(0,Vec::len),"legacyFunctionCall":legacy,"finish":finish}));
                }
            }
        }
    }
    fn summary(&self, total_ms: u64) -> Value {
        let active_ms = self
            .last_output_ms
            .zip(self.first_output_ms)
            .map(|(last, first)| last - first);
        let actual_tokens = self
            .usage
            .as_ref()
            .and_then(|v| v["completion_tokens"].as_u64());
        json!({"elapsedMs":total_ms,"firstOutputMs":self.first_output_ms,"lastOutputMs":self.last_output_ms,
            "activeOutputMs":active_ms,"textChars":self.text_chars,"reasoningChars":self.reasoning_chars,
            "outputBytes":self.bytes,"actualCompletionTokens":actual_tokens,
            "actualTokensPerActiveSecond":actual_tokens.zip(active_ms).filter(|(_,ms)|*ms>0).map(|(tokens,ms)|tokens as f64*1000.0/ms as f64),
            "outputBytesPerActiveSecond":active_ms.filter(|ms|*ms>0).map(|ms|self.bytes as f64*1000.0/ms as f64),
            "toolCalls":self.tools,"finishFrames":self.finishes,"outputFramesAfterFinish":self.output_after_finish,
            "upstreamDone":self.done,"businessErrorStatus":self.error_status,"usage":self.usage,
            "frames":self.frames})
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let home = std::env::var(config::BUDDY_SWITCH_HOME_ENV)
        .map_err(|_| "Explicit BUDDY_SWITCH_HOME required")?;
    if !std::path::Path::new(&home).is_absolute() {
        return Err("Absolute home required".into());
    }
    let selected = std::env::var("BUDDY_DIAG_REGION").unwrap_or_else(|_| "cn".into());
    let region = QoderRegion::parse(&selected).ok_or("Invalid region")?;
    let case = std::env::var("BUDDY_DIAG_CASE").unwrap_or_else(|_| "tool".into());
    if !matches!(case.as_str(), "tool" | "text") {
        return Err("case must be tool or text".into());
    }
    let model = std::env::var("BUDDY_DIAG_MODEL").unwrap_or_else(|_| "dfmodel".into());
    let output = std::env::var("BUDDY_DIAG_OUTPUT_DIR")
        .map_err(|_| "Explicit diagnostic output directory required")?;
    let account = account::load_accounts_for(region)
        .accounts
        .into_iter()
        .find(|a| a.access_token.as_deref().is_some_and(|v| !v.is_empty()))
        .ok_or("No access credential")?;
    let name = account::display_name(&account);
    let input = cosy::CosyAccountInput {
        uid: &account.uid,
        nickname: &name,
        access_token: account.access_token.as_deref().unwrap_or(""),
        refresh_token: "",
        user_type: None,
        org_id: None,
        org_name: None,
    };
    let catalog = api::fetch_model_list(region, &input)
        .await
        .map_err(|_| "Catalog request failed")?;
    let entry = catalog
        .iter()
        .find(|v| v["key"].as_str() == Some(&model))
        .ok_or("Requested model not in catalog")?;
    let meta = ModelMeta::from_catalog_entry(entry);
    let mut payload = json!({"model":model,"stream":true,"max_tokens":768,"messages":[{"role":"user","content":if case=="tool" {"Call diagnostic_echo exactly once with text GATEWAY_TIMING_OK. Use the tool, not a description of your intention. This tool only echoes data."} else {"Output the integers 1 through 128 as a JSON array. Do not skip any numbers. No explanation or code fence."}}]});
    if case == "tool" {
        payload["tools"] = json!([{"type":"function","function":{"name":"diagnostic_echo","description":"Echo a diagnostic string; no external action.","parameters":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}}]);
    }
    if let Ok(effort) = std::env::var("BUDDY_DIAG_EFFORT") {
        payload["reasoning_effort"] = json!(effort);
    }
    let body = api::build_chat_body(&payload, &meta, region, "")?;
    let parameters = body["parameters"].clone();
    let encoded = cosy::qoder_encode(body.to_string().as_bytes());
    let session = cosy::shared_cache().get(&input)?;
    let headers = session.headers(&encoded, qoder::PATH_CHAT_SSE, Some(&model), true);
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(45))
        .build()
        .map_err(|_| "HTTP client failed")?;
    let mut request = client
        .post(format!(
            "{}{}",
            region::gateway_candidates(region)[0],
            qoder::PATH_CHAT_SSE_ENCODED
        ))
        .body(encoded);
    for (key, value) in headers {
        request = request.header(key, value);
    }
    let started = Instant::now();
    let response = request
        .send()
        .await
        .map_err(|_| "Upstream transport failed")?;
    let status = response.status().as_u16();
    let headers_ms = started.elapsed().as_millis() as u64;
    let mut observation = Observation::default();
    let mut buffer = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let mut upstream = response.bytes_stream();
    let mut timed_out = false;
    let mut read_failed = false;
    let mut prefix_limit = false;
    let mut wire_bytes = 0usize;
    loop {
        let item = match tokio::time::timeout_at(deadline, upstream.next()).await {
            Ok(item) => item,
            Err(_) => {
                timed_out = true;
                break;
            }
        };
        let Some(item) = item else { break };
        let Ok(chunk) = item else {
            read_failed = true;
            break;
        };
        wire_bytes += chunk.len();
        if wire_bytes > 2 * 1024 * 1024 {
            prefix_limit = true;
            break;
        }
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            observation.line(
                &String::from_utf8_lossy(&line),
                started.elapsed().as_millis() as u64,
            );
        }
        if observation.done || observation.error_status.is_some() {
            break;
        }
    }
    if !buffer.is_empty() {
        observation.line(
            &String::from_utf8_lossy(&buffer),
            started.elapsed().as_millis() as u64,
        );
    }
    let mut report = observation.summary(started.elapsed().as_millis() as u64);
    report["region"] = json!(region.as_str());
    report["case"] = json!(case);
    report["model"] = json!(model);
    report["catalogDisplayName"] = json!(meta.display_name);
    report["parameters"] = parameters;
    report["httpStatus"] = json!(status);
    report["headersMs"] = json!(headers_ms);
    report["timedOut"] = json!(timed_out);
    report["readFailed"] = json!(read_failed);
    report["wireLimitExceeded"] = json!(prefix_limit);
    report["singleAttempt"] = json!(true);
    report["executedTools"] = json!(false);
    std::fs::create_dir_all(&output).map_err(|_| "Cannot create output directory")?;
    let file = std::path::Path::new(&output).join(format!(
        "raw-{}-{}-{}.json",
        region.as_str(),
        case,
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    ));
    std::fs::write(&file, serde_json::to_vec_pretty(&report).unwrap())
        .map_err(|_| "Cannot save report")?;
    report.as_object_mut().unwrap().remove("frames");
    println!("{}", report);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_late_tools_and_does_not_invent_token_rates() {
        let mut observation = Observation::default();
        for (ms, chunk) in [
            (
                100,
                json!({"choices":[{"delta":{"content":"中文"},"finish_reason":"stop"}]}),
            ),
            (
                200,
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"diagnostic_echo","arguments":"{}"}}]}}]}),
            ),
        ] {
            observation.line(
                &format!(
                    "data: {}",
                    json!({"body":chunk.to_string(),"statusCodeValue":200})
                ),
                ms,
            );
        }
        let summary = observation.summary(300);
        assert_eq!(summary["outputFramesAfterFinish"], 1);
        assert_eq!(summary["toolCalls"]["0"]["name"], "diagnostic_echo");
        assert_eq!(summary["outputBytes"], 8);
        assert!(summary["actualTokensPerActiveSecond"].is_null());
        assert_eq!(summary["upstreamDone"], false);
    }
}
