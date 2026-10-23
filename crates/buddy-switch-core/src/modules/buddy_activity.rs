//! WorkBuddy Global web activity adapter. Protocol reference: MIT-licensed
//! TraeWorkAssistant/tasks/buddy_ai_checkin.rs, pinned in THIRD_PARTY_NOTICES.md.
//! This adapter owns no inference pool and never writes authentication records.
//! Non-idempotent create/prompt are sent once; uncertain receipts block replay.
//!
//! ## 自门控开关（PRD D4）
//!
//! 环境变量 `BUDDY_SWITCH_WB_GLOBAL_ACTIVE_TASK`（默认关闭）：
//! - 关闭时，[`status`] 与 [`run`] 入口在最前端短路返回 unsupported，绝不触网或读取凭据；
//! - 仅当设为 1/true/on/yes（ASCII 大小写不敏感，trim 空白）时开启本模块能力。

use super::{
    config,
    region::{region_spec, Region},
};
use reqwest::{Client, RequestBuilder};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

const MODEL: &str = "deepseek-v4.1-flash";
const PROMPT: &str = "Hi";

/// 国际版网页/ACP 活跃任务环境变量开关（默认关）。
pub const ENV_WB_GLOBAL_ACTIVE_TASK: &str = "BUDDY_SWITCH_WB_GLOBAL_ACTIVE_TASK";

/// 活跃任务关闭时的统一 unsupported 文案。
pub const UNSUPPORTED_MESSAGE: &str =
    "该实验功能默认关闭（BUDDY_SWITCH_WB_GLOBAL_ACTIVE_TASK=1 开启）";

/// 解析活跃任务开关值：
/// - 1 / true / on / yes（ASCII 大小写不敏感，trim 空白）⇒ `true`
/// - 其余一切值（含 None、空串、0 / false / off / no、未知值）⇒ `false`
pub(crate) fn parse_global_active_task_switch(raw: Option<&str>) -> bool {
    let Some(val) = raw else {
        return false;
    };
    let trimmed = val.trim();
    trimmed.eq_ignore_ascii_case("1")
        || trimmed.eq_ignore_ascii_case("true")
        || trimmed.eq_ignore_ascii_case("on")
        || trimmed.eq_ignore_ascii_case("yes")
}

/// 国际版网页活跃任务是否开启（默认关）。
pub fn global_active_task_enabled() -> bool {
    parse_global_active_task_switch(std::env::var(ENV_WB_GLOBAL_ACTIVE_TASK).ok().as_deref())
}

fn ledger_path(uid: &str) -> PathBuf {
    let key = format!("{:x}", Sha256::digest(uid.as_bytes()));
    config::store_dir()
        .join("activity.global")
        .join(format!("{key}.json"))
}

fn today() -> String {
    chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .format("%Y-%m-%d")
        .to_string()
}

fn read_ledger(uid: &str) -> Result<Value, String> {
    match std::fs::read_to_string(ledger_path(uid)) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|_| "活跃任务记录损坏，请先检查记录；不会重复创建会话".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(_) => Err("无法读取活跃任务记录".into()),
    }
}

fn save_ledger(uid: &str, ledger: &Value) -> Result<(), String> {
    let file = ledger_path(uid);
    std::fs::create_dir_all(file.parent().unwrap()).map_err(|_| "无法创建活跃任务目录")?;
    config::atomic_write(&file, &ledger.to_string())
        .map_err(|_| "活跃任务记录保存失败；不要重复提交".into())
}

// Cross-process exclusion in the isolated application store. A leftover lock
// after a crash fails closed instead of guessing that a paid action did not run.
struct AccountLock(PathBuf);
impl AccountLock {
    fn acquire(uid: &str) -> Result<Self, String> {
        let path = ledger_path(uid).with_extension("lock");
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|_| "无法创建任务锁目录")?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "该账号有正在执行或待核查的活跃任务；不会重复提交")?;
        Ok(Self(path))
    }
}
impl Drop for AccountLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn web(request: RequestBuilder, token: &str, uid: &str) -> RequestBuilder {
    let request = request.bearer_auth(token);
    let request = if uid.is_empty() {
        request
    } else {
        request.header("X-User-Id", uid)
    };
    request
        .header("X-Client-Platform", "web")
        .header("User-Agent", "Mozilla/5.0")
        .header("Origin", region_spec(Region::Global).chat_base)
        .header(
            "Referer",
            format!("{}/", region_spec(Region::Global).chat_base),
        )
}

async fn json_response(request: RequestBuilder, stage: &str) -> Result<Value, String> {
    let response = request
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|_| format!("{stage}网络失败；结果待确认"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{stage} HTTP {}", status.as_u16()));
    }
    response
        .json()
        .await
        .map_err(|_| format!("{stage}响应不是有效 JSON"))
}

fn resource_total(payload: &Value) -> Option<f64> {
    let value = payload
        .pointer("/data/Response/Data/TotalDosage")
        .or_else(|| payload.pointer("/data/TotalDosage"))?;
    let number = value.as_f64().or_else(|| value.as_str()?.parse().ok())?;
    number.is_finite().then_some(number)
}

async fn balance(client: &Client, token: &str, uid: &str) -> Option<f64> {
    let url = format!(
        "{}/v2/billing/meter/get-user-resource",
        region_spec(Region::Global).billing_base
    );
    resource_total(
        &json_response(
            web(client.post(url), token, uid).json(&json!({})),
            "查询资源",
        )
        .await
        .ok()?,
    )
}

/// Only explicit upstream completion and an observed positive balance delta
/// produce a reward receipt. Completion without that evidence remains pending.
///
/// ⚠️ `before` **必须**是紧邻 prompt 提交那一刻的快照（见 [`reward_baseline`]），
/// 不能是「建会话时」的历史值 —— 否则差值跨越任意时间跨度，无法归因到本次任务。
fn receipt(completed: bool, before: Option<f64>, after: Option<f64>) -> Value {
    let gain = before
        .zip(after)
        .map(|(b, a)| a - b)
        .filter(|g| g.is_finite() && *g > 0.0);
    if completed && gain.is_some() {
        json!({"result":"success", "amount":gain, "evidence":"completed_and_balance_delta", "balanceDelta":gain})
    } else {
        json!({"result":"pending", "amount":null, "evidence":if completed {"completed_without_reward_receipt"} else {"not_completed"},
            "error":if completed {"今日网页活跃任务已完成；官方说明奖励次日发放，到账尚未确认。再次点击只复查，不会新建会话"} else {"会话尚未完成；再次点击只复查，不会重复提交"}})
    }
}

/// 取「紧邻 prompt 提交前」的余额快照 —— **纯函数**，直接可单测。
///
/// 只认 `promptBefore`（`drive()` 在提交 prompt 之前立即写入，并同时记录
/// `promptSubmittedAt`）。**刻意不回退**到旧的 `before`：`before` 是建会话时的快照，
/// 与 prompt 提交之间可能隔了任意长时间（跨轮、跨天），用它算差值是伪归因 ——
/// 宁可返回 `None` 让结果为 `pending`，也不给出一个无法证明归属的 `success`。
fn reward_baseline(ledger: &Value) -> Option<f64> {
    let marked = ledger
        .get("promptSubmittedAt")
        .and_then(Value::as_i64)
        .is_some();
    if !marked {
        return None;
    }
    ledger["promptBefore"].as_f64().filter(|v| v.is_finite())
}

// International daily activity is independent of the CN daily-checkin campaign.
// https://www.workbuddy.ai/pricing/ specifies next-day rewards for client activity.
// checkin-activity-status.active=false must never gate the web activity flow.
fn status_from_ledger(ledger: &Value, date: &str) -> Value {
    let confirmed =
        ledger["date"] == date && ledger.pointer("/receipt/result") == Some(&json!("success"));
    let completed = ledger["date"] == date && ledger["phase"] == "completed";
    json!({"ok":true,"todayCheckedIn":confirmed,"dailyCredit":null,
        "todayCredit":if confirmed {ledger.pointer("/receipt/amount").cloned().unwrap_or(Value::Null)} else {Value::Null},
        "source":"web_activity_local_receipt",
        "status":if confirmed {"CLAIMED"} else if completed {"AWAITING_REWARD"} else {"UNCONFIRMED"},
        "activityCompletedToday":completed,
        "hint":"国际版为每日活跃奖励，官方说明次日发放；本地无到账回执不等于今日未活跃。"})
}

pub fn status(account: &Value) -> Value {
    if !global_active_task_enabled() {
        return json!({"ok": false, "error": UNSUPPORTED_MESSAGE});
    }
    let Some(uid) = account
        .get("uid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return json!({"ok":false,"error":"国际版账号缺少 uid"});
    };
    match read_ledger(uid) {
        Ok(ledger) => status_from_ledger(&ledger, &today()),
        Err(error) => json!({"ok":false,"error":error}),
    }
}

// Dropping a future on ANY error also aborts its SSE reader and closes the socket.
struct ReaderGuard(tokio::task::JoinHandle<()>);
impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn sandbox_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "沙箱地址无效")?;
    let host = url.host_str().unwrap_or("");
    // Observed from the authenticated international /session response, 2026-10-21.
    // Accept only this region and a single generated host label, never the whole
    // cloudstudio.club domain or arbitrary hosts returned by a redirect.
    let cloudstudio = [
        ".e2b.ap-singapore.sandbox.cloudstudio.club",
        ".e2b.sg2.sandbox.cloudstudio.club",
    ]
    .iter()
    .find_map(|suffix| host.strip_suffix(suffix))
    .is_some_and(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    }) && url.path() == "/acp"
        && url.query().is_none()
        && url.fragment().is_none();
    // Never send the sandbox token to a local endpoint or an arbitrary redirect.
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || !(host == "workbuddy.ai" || host.ends_with(".workbuddy.ai") || cloudstudio)
    {
        return Err("沙箱地址不在 WorkBuddy 国际版允许域内；已停止".into());
    }
    Ok(url)
}

fn acp_post(
    client: &Client,
    link: &reqwest::Url,
    token: &str,
    conn: &str,
    payload: &Value,
) -> RequestBuilder {
    web(client.post(link.clone()), token, "")
        .header("Acp-Connection-Id", conn)
        .header("Accept", "text/event-stream, application/json")
        .json(payload)
}

fn server_reply(event: &Value) -> Option<Value> {
    let id = event.get("id")?;
    let method = event.get("method")?.as_str()?;
    Some(if method == "session/request_permission" {
        json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"cancelled"}}})
    } else {
        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Activity task has no filesystem or terminal capabilities"}})
    })
}

fn take_sse_events(buffer: &mut Vec<u8>) -> Vec<Value> {
    let mut events = Vec::new();
    loop {
        let lf = buffer.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
        let crlf = buffer
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| (p, 4));
        let Some((end, separator)) = [lf, crlf].into_iter().flatten().min_by_key(|x| x.0) else {
            break;
        };
        let frame: Vec<u8> = buffer.drain(..end + separator).collect();
        let Ok(frame) = std::str::from_utf8(&frame) else {
            continue;
        };
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(event) = serde_json::from_str(&data) {
            events.push(event);
        }
    }
    events
}

async fn drive(
    client: &Client,
    token: &str,
    uid: &str,
    ledger: &mut Value,
) -> Result<bool, String> {
    let base = region_spec(Region::Global).chat_base;
    let conv = ledger["conversationId"]
        .as_str()
        .ok_or("任务缺少会话 ID")?
        .to_string();
    let state_url = format!("{base}/console/as/conversations/{conv}");
    let current = json_response(web(client.get(&state_url), token, uid), "复查会话").await?;
    if current.pointer("/data/status").and_then(Value::as_str) == Some("completed") {
        return Ok(true);
    }
    // A previously sent or uncertain prompt must never be submitted again.
    if matches!(
        ledger["phase"].as_str(),
        Some("prompt_sent" | "prompt_uncertain" | "completed")
    ) {
        return Ok(false);
    }
    let session = json_response(
        web(client.get(format!("{state_url}/session")), token, uid),
        "取得沙箱",
    )
    .await?;
    let data = session.get("data").unwrap_or(&session);
    let link = sandbox_url(data["link"].as_str().ok_or("沙箱缺少 link")?)?;
    let sandbox_token = data["token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("沙箱缺少 token")?
        .to_string();
    let sid = data["sessionId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("沙箱缺少 sessionId")?
        .to_string();
    let mut stream = client
        .get(link.clone())
        .bearer_auth(&sandbox_token)
        .header("Accept", "text/event-stream")
        .timeout(Duration::from_secs(210))
        .send()
        .await
        .map_err(|_| "建立 ACP 通道失败")?;
    if !stream.status().is_success() {
        return Err(format!("ACP 通道 HTTP {}", stream.status().as_u16()));
    }
    let conn = stream
        .headers()
        .get("Acp-Connection-Id")
        .and_then(|s| s.to_str().ok())
        .ok_or("ACP 缺少连接 ID")?
        .to_string();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(32);
    let reader_client = client.clone();
    let reader_link = link.clone();
    let reader_token = sandbox_token.clone();
    let reader_conn = conn.clone();
    let _reader = ReaderGuard(tokio::spawn(async move {
        let mut buffer = Vec::new();
        while let Ok(Some(chunk)) = stream.chunk().await {
            buffer.extend_from_slice(&chunk);
            if buffer.len() > 1024 * 1024 {
                break;
            }
            for event in take_sse_events(&mut buffer) {
                if let Some(reply) = server_reply(&event) {
                    let _ = acp_post(
                        &reader_client,
                        &reader_link,
                        &reader_token,
                        &reader_conn,
                        &reply,
                    )
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await;
                } else if event.get("id").is_some() && tx.send(event).await.is_err() {
                    return;
                }
            }
        }
    }));
    for (id, method, params) in [
        (
            1,
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false}}}),
        ),
        (
            2,
            "session/load",
            json!({"sessionId":sid,"cwd":"/","mcpServers":[]}),
        ),
        (
            3,
            "session/set_model",
            json!({"sessionId":sid,"modelId":MODEL}),
        ),
    ] {
        let payload = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let response = acp_post(client, &link, &sandbox_token, &conn, &payload)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| format!("ACP {method}请求失败"))?;
        if !response.status().is_success() {
            return Err(format!("ACP {method} HTTP {}", response.status().as_u16()));
        }
        // HTTP 202 is acceptance only. Gate the next step on the actual RPC result.
        let ack = tokio::time::timeout(Duration::from_secs(25), async {
            while let Some(event) = rx.recv().await {
                if event["id"] == id {
                    return Some(event);
                }
            }
            None
        })
        .await
        .map_err(|_| format!("ACP {method}回执超时；不会提交 prompt"))?
        .ok_or("ACP 通道已结束")?;
        if ack.get("error").is_some() || ack.get("result").is_none() {
            return Err(format!("ACP {method}未成功；不会继续提交"));
        }
    }
    ledger["phase"] = json!("prompt_uncertain");
    save_ledger(uid, ledger)?;
    // ★ 到账判据的基线：必须在**提交 prompt 之前**立刻采集，并同时打时间戳。
    //   放在这里（而不是建会话时）是为了让 before/after 夹住真正的计费动作。
    //   采集失败不阻塞提交（到账确认是加分项，不是门槛），只是结果会停在 pending。
    ledger["promptBefore"] = balance(client, token, uid)
        .await
        .map_or(Value::Null, |v| json!(v));
    ledger["promptSubmittedAt"] = json!(chrono::Utc::now().timestamp_millis());
    save_ledger(uid, ledger)?;
    let prompt = json!({"jsonrpc":"2.0","id":4,"method":"session/prompt", "params":{"sessionId":sid,"prompt":[{"type":"text","text":PROMPT}]}});
    let response = acp_post(client, &link, &sandbox_token, &conn, &prompt)
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|_| "prompt 发送结果不确定；已保留会话，禁止重复提交")?;
    if !response.status().is_success() {
        return Err(format!(
            "prompt HTTP {}；请复查已有会话",
            response.status().as_u16()
        ));
    }
    ledger["phase"] = json!("prompt_sent");
    save_ledger(uid, ledger)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    while tokio::time::Instant::now() < deadline {
        while let Ok(event) = rx.try_recv() {
            if event.get("error").is_some() {
                return Err("ACP 会话返回错误；请复查已有会话".into());
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        let snapshot =
            json_response(web(client.get(&state_url), token, uid), "等待会话完成").await?;
        match snapshot
            .pointer("/data/status")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "completed" => return Ok(true),
            "failed" | "error" | "cancelled" => {
                return Err("网页会话已进入失败终态；不会自动重建".into())
            }
            _ => {}
        }
    }
    Ok(false)
}

pub async fn run(account: &Value) -> Value {
    match run_inner(account).await {
        Ok(value) => value,
        Err(error) => json!({"result":"error","error":error}),
    }
}

async fn run_inner(account: &Value) -> Result<Value, String> {
    if !global_active_task_enabled() {
        return Ok(json!({"result": "unsupported", "error": UNSUPPORTED_MESSAGE}));
    }
    let uid = account["uid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("国际版账号缺少 uid")?;
    let _lock = AccountLock::acquire(uid)?;
    let cfg = config::load_checkin_config();
    let account =
        super::refresh::ensure_fresh_token_for(Region::Global, account.clone(), &cfg).await;
    let token = account["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("国际版账号缺少 access token")?;
    if let Some(error) = super::account::envelope_token_error(&account) {
        return Err(error);
    }
    let mut ledger = read_ledger(uid)?;
    if ledger["date"] == today() && ledger.pointer("/receipt/result") == Some(&json!("success")) {
        return Ok(json!({"result":"already","evidence":"local_confirmed_receipt"}));
    }
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "无法初始化活跃任务 HTTP 客户端")?;
    if ledger["date"] != today() && ledger["phase"] == "completed" {
        ledger = json!({});
    }
    if ledger.get("date").is_none() {
        let before = balance(&client, token, uid).await;
        ledger = json!({"date":today(),"phase":"create_uncertain","before":before,"model":MODEL});
        save_ledger(uid, &ledger)?;
        let base = region_spec(Region::Global).chat_base;
        let created = json_response(web(client.post(format!("{base}/console/as/conversations/")), token, uid)
            .json(&json!({"prompt":PROMPT,"model":MODEL,"conversationOrigin":"workbuddy-app","plugins":[]})), "创建网页会话").await?;
        let id = created
            .pointer("/data/id")
            .ok_or("建会话缺少 ID；请求结果待核查，禁止重建")?;
        let id = id
            .as_str()
            .map(str::to_string)
            .or_else(|| id.as_u64().map(|n| n.to_string()))
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
            })
            .ok_or("建会话 ID 无效")?;
        ledger["conversationId"] = json!(id);
        ledger["phase"] = json!("created");
        save_ledger(uid, &ledger)?;
    }
    if ledger["conversationId"].as_str().is_none() {
        return Err("前次创建结果不确定且没有会话 ID，请到官网核查；不会重复创建".into());
    }
    let completed = drive(&client, token, uid, &mut ledger).await?;
    // 到账判据的基线：只认紧邻 prompt 的快照（`promptBefore`）。建会话时的
    // `before` 与 prompt 之间可能隔了任意长时间，用它算差值是伪归因 —— 见 `reward_baseline`。
    let outcome = receipt(
        completed,
        reward_baseline(&ledger),
        balance(&client, token, uid).await,
    );
    ledger["receipt"] = outcome.clone();
    if completed {
        ledger["phase"] = json!("completed");
    }
    save_ledger(uid, &ledger)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_is_not_a_reward_receipt() {
        assert_eq!(receipt(true, Some(100.0), Some(100.0))["result"], "pending");
        assert!(receipt(true, None, Some(150.0))["amount"].is_null());
        assert_eq!(
            receipt(false, Some(100.0), Some(150.0))["result"],
            "pending"
        );
        assert_eq!(receipt(true, Some(100.0), Some(150.0))["amount"], 50.0);
    }
    #[test]
    fn resource_shape_and_permission_denial() {
        assert_eq!(
            resource_total(&json!({"data":{"Response":{"Data":{"TotalDosage":"123.5"}}}})),
            Some(123.5)
        );
        let reply = server_reply(&json!({"id":9,"method":"session/request_permission"})).unwrap();
        assert_eq!(
            reply.pointer("/result/outcome/outcome").unwrap(),
            "cancelled"
        );
    }
    #[test]
    fn sandbox_address_is_bounded() {
        assert!(sandbox_url("https://sandbox.workbuddy.ai/acp").is_ok());
        assert!(
            sandbox_url("https://65225-example.e2b.ap-singapore.sandbox.cloudstudio.club/acp")
                .is_ok()
        );
        assert!(sandbox_url("https://65225-example.e2b.sg2.sandbox.cloudstudio.club/acp").is_ok());
        for bad in [
            "http://sandbox.workbuddy.ai",
            "https://127.0.0.1",
            "https://workbuddy.ai.evil.test",
            "https://evilworkbuddy.ai",
            "https://a.workbuddy.ai:444/",
            "https://a.e2b.ap-singapore.sandbox.cloudstudio.club.evil.test/acp",
            "https://a.other.sandbox.cloudstudio.club/acp",
            "https://a.b.e2b.ap-singapore.sandbox.cloudstudio.club/acp",
            "https://a.e2b.ap-singapore.sandbox.cloudstudio.club/other",
            "https://a.e2b.ap-singapore.sandbox.cloudstudio.club/acp?redirect=evil",
        ] {
            assert!(sandbox_url(bad).is_err());
        }
    }

    #[test]
    fn sse_handles_split_utf8_multiline_and_crlf() {
        let text = "data: {\"id\":1,\r\ndata: \"result\":\"中文\"}\r\n\r\n";
        let split = text.find('中').unwrap() + 1;
        let mut buffer = text.as_bytes()[..split].to_vec();
        assert!(take_sse_events(&mut buffer).is_empty());
        buffer.extend_from_slice(&text.as_bytes()[split..]);
        let events = take_sse_events(&mut buffer);
        assert_eq!(events[0]["result"], "中文");
        assert!(buffer.is_empty());
    }

    /// 到账基线只认**紧邻 prompt** 的快照；跨轮复用的旧 `before` 一律不采信。
    ///
    /// 为什么这是安全红线：旧的 `before` 是「建会话时」的值，与真正扣积分/发奖励的
    /// `session/prompt` 之间可能隔了任意长时间（跨轮、跨天）。拿它算差值等于**伪归因** ——
    /// 期间任何别的原因造成的余额增长都会被算成「本次签到奖励」。
    ///
    /// 可证伪：把 `reward_baseline` 改成回退读 `ledger["before"]`，第 2、3 条断言立刻红。
    #[test]
    fn reward_baseline_only_trusts_the_snapshot_taken_next_to_the_prompt() {
        // 1. 正常路径：紧邻 prompt 的快照 + 时间戳 → 采信
        let fresh = json!({
            "date": "2026-10-21",
            "before": 999.0,                 // 旧值，必须被忽略
            "promptBefore": 204.0,
            "promptSubmittedAt": 1_792_511_000_000_i64
        });
        assert_eq!(
            reward_baseline(&fresh),
            Some(204.0),
            "必须用 promptBefore，而不是建会话时的 before"
        );

        // 2. 只有陈旧 before、没有 prompt 快照 → 拒绝（宁可 pending，不伪 success）
        let stale_only = json!({"date": "2026-10-21", "before": 100.0});
        assert_eq!(
            reward_baseline(&stale_only),
            None,
            "跨轮的 before 不得作为到账基线"
        );

        // 3. 有 promptBefore 但缺时间戳 → 拒绝（无法证明它紧邻 prompt）
        let no_timestamp = json!({"date": "2026-10-21", "promptBefore": 204.0});
        assert_eq!(reward_baseline(&no_timestamp), None);

        // 4. 快照采集失败（null）→ None，且 receipt 必须落到 pending
        let null_snapshot = json!({"promptBefore": Value::Null, "promptSubmittedAt": 1_i64});
        assert_eq!(reward_baseline(&null_snapshot), None);
        assert_eq!(
            receipt(true, reward_baseline(&null_snapshot), Some(300.0))["result"],
            "pending",
            "没有可信基线时，完成 ≠ 到账"
        );

        // 5. 反之：有可信基线且确认正delta → 才是 success
        assert_eq!(
            receipt(true, reward_baseline(&fresh), Some(304.0))["amount"],
            100.0
        );
    }

    #[test]
    fn international_activity_status_keeps_completion_separate_from_reward() {
        let date = "2026-10-21";
        let unknown = status_from_ledger(&json!({}), date);
        assert_eq!(unknown["status"], "UNCONFIRMED");
        assert_eq!(unknown["todayCheckedIn"], false);
        assert_eq!(unknown["todayCredit"], Value::Null);
        let completed = json!({"date":date,"phase":"completed","receipt":{"result":"pending"}});
        let pending = status_from_ledger(&completed, date);
        assert_eq!(pending["status"], "AWAITING_REWARD");
        assert_eq!(pending["activityCompletedToday"], true);
        assert_eq!(pending["todayCheckedIn"], false);
        assert_eq!(pending["todayCredit"], Value::Null);
        assert_eq!(
            status_from_ledger(&completed, "2026-10-22")["activityCompletedToday"],
            false
        );
        let paid =
            json!({"date":date,"phase":"completed","receipt":{"result":"success","amount":30.0}});
        assert_eq!(status_from_ledger(&paid, date)["status"], "CLAIMED");
        assert_eq!(status_from_ledger(&paid, date)["todayCredit"], 30.0);
    }

    static ACTIVITY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvRestorer {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev_home: Option<std::ffi::OsString>,
        prev_task: Option<std::ffi::OsString>,
        temp_dir: PathBuf,
    }

    impl EnvRestorer {
        fn new() -> Self {
            let lock = ACTIVITY_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let prev_home = std::env::var_os("BUDDY_SWITCH_HOME");
            let prev_task = std::env::var_os(ENV_WB_GLOBAL_ACTIVE_TASK);
            let temp_dir = std::env::temp_dir().join(format!(
                "buddy-switch-act-test-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&temp_dir).expect("create test temp dir");
            std::env::set_var("BUDDY_SWITCH_HOME", temp_dir.as_os_str());
            Self {
                _lock: lock,
                prev_home,
                prev_task,
                temp_dir,
            }
        }
    }

    impl Drop for EnvRestorer {
        fn drop(&mut self) {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("BUDDY_SWITCH_HOME", v),
                None => std::env::remove_var("BUDDY_SWITCH_HOME"),
            }
            match self.prev_task.take() {
                Some(v) => std::env::set_var(ENV_WB_GLOBAL_ACTIVE_TASK, v),
                None => std::env::remove_var(ENV_WB_GLOBAL_ACTIVE_TASK),
            }
            let _ = std::fs::remove_dir_all(&self.temp_dir);
        }
    }

    /// 默认关闭：status 与 run 均返回 unsupported 且文案包含开关名；不触网。
    #[tokio::test]
    async fn global_active_task_disabled_by_default() {
        let _env = EnvRestorer::new();
        std::env::remove_var(ENV_WB_GLOBAL_ACTIVE_TASK);
        assert!(!global_active_task_enabled());

        let account = json!({"id": "g-1", "uid": "uid-1", "access_token": "token"});
        let st = status(&account);
        assert_eq!(st["ok"], json!(false));
        let err_st = st["error"].as_str().expect("error string");
        assert!(
            err_st.contains(ENV_WB_GLOBAL_ACTIVE_TASK),
            "status 错误文案必须点名开关: {err_st}"
        );

        let rn = run(&account).await;
        assert_eq!(rn["result"], json!("unsupported"));
        let err_rn = rn["error"].as_str().expect("error string");
        assert!(
            err_rn.contains(ENV_WB_GLOBAL_ACTIVE_TASK),
            "run 错误文案必须点名开关: {err_rn}"
        );
    }

    /// =1 时通过门控：
    /// - status 用隔离的 BUDDY_SWITCH_HOME 且无 ledger 文件 ⇒ {"ok":true,...,"status":"UNCONFIRMED"}（不触网）；
    /// - run 测门控放行分支：=1 且账号缺 uid ⇒ 返回 {"result":"error","error":"国际版账号缺少 uid"}（证明门控放行且不触网）。
    #[tokio::test]
    async fn global_active_task_gate_allows_when_enabled() {
        let _env = EnvRestorer::new();
        std::env::set_var(ENV_WB_GLOBAL_ACTIVE_TASK, "1");
        assert!(global_active_task_enabled());

        let account_with_uid = json!({"id": "g-1", "uid": "uid-test-1", "access_token": "token"});
        let st = status(&account_with_uid);
        assert_eq!(st["ok"], json!(true));
        assert_eq!(st["status"], json!("UNCONFIRMED"));
        assert_eq!(st["source"], json!("web_activity_local_receipt"));

        let account_no_uid = json!({"id": "g-no-uid", "access_token": "token"});
        let rn = run(&account_no_uid).await;
        assert_eq!(rn["result"], json!("error"));
        assert_eq!(rn["error"], json!("国际版账号缺少 uid"));
    }

    /// 开关解析矩阵验证：
    /// 1/true/on/yes/TRUE/" yes "（含空白）⇒ 开；
    /// 0/false/off/no/空串/任意其它值/未设置 ⇒ 关。
    #[test]
    fn global_active_task_parsing_matrix() {
        // 关的分支：0/false/off/no/空串/任意其它值/未设置
        for val in [
            None,
            Some(""),
            Some("   "),
            Some("\t\n"),
            Some("0"),
            Some(" 0 "),
            Some("false"),
            Some("False"),
            Some("FALSE"),
            Some("off"),
            Some("Off"),
            Some("OFF"),
            Some("no"),
            Some("No"),
            Some("NO"),
            Some(" no "),
            Some("2"),
            Some("garbage"),
            Some("invalid"),
            Some("disabled"),
        ] {
            assert!(
                !parse_global_active_task_switch(val),
                "{val:?} 必须解析为关"
            );
        }

        // 开的分支：1/true/on/yes/TRUE/" yes "（含空白）
        for val in [
            "1", " 1 ", "true", "True", "TRUE", " true \n", "on", "On", "ON", " on ", "yes", "Yes",
            "YES", " yes ",
        ] {
            assert!(
                parse_global_active_task_switch(Some(val)),
                "{val:?} 必须解析为开"
            );
        }
    }
}
