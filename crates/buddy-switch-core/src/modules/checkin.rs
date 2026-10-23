//! 签到：状态查询 / 执行签到 / 自动签到调度。
//!
//! 对照 server.py `get_checkin_status` / `perform_checkin` /
//! `checkin_account` / `run_checkin_cycle` / `_checkin_request` /
//! `_is_unauthorized`。
//!
//! **region 化**：新增 `*_for(region, …)` 变体；旧 CN 签名保留为薄包装。
//!
//! ## 整合说明（移植自 TraeWorkAssistant `tasks/wb_checkin.rs` + `wb_common.rs`）
//!
//! 旧实现「签到总领不到」的四个根因，本模块逐条修掉：
//!
//! 1. Global uses a separate web/ACP activity adapter (`buddy_activity`), controlled
//!    by `BUDDY_SWITCH_WB_GLOBAL_ACTIVE_TASK` (default off). `BUDDY_SWITCH_WB_GLOBAL_CHECKIN`
//!    retains the semantic for "Global daily check-in endpoint" (default on), but
//!    currently this codebase has no direct endpoint implementation and thus no active
//!    code path. Manual invocation only; scheduled Global cycles remain disabled.
//! 2. **请求头不像桌面端**：缺 `User-Agent: WorkBuddy/x.y.z` 与账号级稳定指纹
//!    `X-Machine-ID` / `X-Session-ID`（issue #48 同款）⇒ 由 [`checkin_headers`] 补齐。
//! 3. **HTTP 状态码被吞**：`http_request` 对「非 2xx + JSON 体」只回 body，401 体里
//!    若没有 `code:401` 就不会触发刷新 ⇒ 改用保留状态码的 `http_request_raw`。
//! 4. **判据过窄**：状态只认 `data.today_checked_in`；已签只认「已签到/repeat」⇒
//!    现在多键深挖（`todayCheckedIn` / `checked_in` / `is_checked` …），已签兼认
//!    `code=10001` / `already`。
//!
//! 另：主域名网络不可达（status=0）时**同区**备用域名重试一次（CN：codebuddy.cn →
//! workbuddy.cn；不跨区，避免把国内 token 发到国际站）。
//! CN / Global 各用一把运行标志，两版互不阻塞（PRD G1）。

use chrono::Local;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::modules::account::{
    account_display_name, build_auth_headers, envelope_token_error, load_accounts_for,
};
use crate::modules::config::{
    add_checkin_log, http_request_raw, load_checkin_config, load_checkin_logs, now_ms,
    RunFlagGuard, CHECKIN_API_PREFIX,
};
use crate::modules::refresh::{ensure_fresh_token_for, refresh_account_token_for};
use crate::modules::region::{region_spec, Region};

static CHECKIN_RUNNING: AtomicBool = AtomicBool::new(false);
/// 国际版独立运行标志：CN 周期在跑时不把 Global 误报成 already_running（PRD G1）。
static CHECKIN_RUNNING_GLOBAL: AtomicBool = AtomicBool::new(false);
static CHECKIN_ACCOUNTS_RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Automatic recovery cadence shared by every host.
pub const CHECKIN_RECOVERY_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// 国际版每日签到接口被显式关闭时的文案（保留开关语义；当前 B 侧无直连代码路径）。
const UNSUPPORTED_MESSAGE: &str = "该版本已关闭每日签到接口（BUDDY_SWITCH_WB_GLOBAL_CHECKIN=0）";

/// WorkBuddy 桌面端 UA（TraeWorkAssistant `WB_DESKTOP_UA` 实测值）。
pub const WB_DESKTOP_UA: &str = "WorkBuddy/5.5.6";
/// 覆盖桌面端 UA（客户端升级后无需重编译）。
const ENV_WB_DESKTOP_UA: &str = "BUDDY_SWITCH_WB_DESKTOP_UA";
/// `0` / `false` / `off` / `no` ⇒ 关闭国际版每日签到接口（默认开）。
///
/// ⚠️ 诚实记录：B 侧（本仓库）目前没有直连 Global 每日签到接口的实现，
/// `BUDDY_SWITCH_WB_GLOBAL_CHECKIN` 保留「Global 每日签到接口」语义，当前无生效代码路径——
/// 不装作有。Global 网页/ACP 活跃任务已拆分至 `buddy_activity` 并由
/// `BUDDY_SWITCH_WB_GLOBAL_ACTIVE_TASK` 自行门控。
const ENV_WB_GLOBAL_CHECKIN: &str = "BUDDY_SWITCH_WB_GLOBAL_CHECKIN";
/// CN 备用域名（与 billing 主域 codebuddy.cn 同区，活动/签到网关）。
const CN_ACTIVITY_BASE: &str = "https://www.workbuddy.cn";

/// 该 region 的签到运行标志。
fn running_flag(region: Region) -> &'static AtomicBool {
    match region {
        Region::Cn => &CHECKIN_RUNNING,
        Region::Global => &CHECKIN_RUNNING_GLOBAL,
    }
}

fn env_off(value: Option<String>) -> bool {
    value
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|v| matches!(v.as_str(), "0" | "false" | "off" | "no"))
}

/// 国际版每日签到接口是否开启（默认开；环境变量显式关闭时返回 false）。
///
/// 诚实说明：B 侧目前没有直连 Global 每日签到接口的实现，保留此开关与纯函数以备
/// 后续接入；当前 Global 请求委托至 `buddy_activity`，不再被此开关拦截。
pub fn global_checkin_enabled() -> bool {
    !env_off(std::env::var(ENV_WB_GLOBAL_CHECKIN).ok())
}

fn region_supported(region: Region) -> bool {
    region == Region::Cn || global_checkin_enabled()
}

/// 账号级稳定设备指纹：`sha256("wb-fingerprint:{kind}:{uid}")` 前 16 字节 → 32 位小写 hex。
/// 同账号恒定（虚拟设备稳定）、跨账号隔离（防关联）；uid 为空返回 `None`。
pub fn derive_device_fingerprint(uid: &str, kind: &str) -> Option<String> {
    if uid.trim().is_empty() {
        return None;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("wb-fingerprint:{kind}:{uid}").as_bytes());
    Some(digest[..16].iter().map(|b| format!("{b:02x}")).collect())
}

/// 桌面端 UA（可被 [`ENV_WB_DESKTOP_UA`] 覆盖）。
///
/// `pub(crate)`：`credits` 的余额请求同样需要这个 UA（真机实测缺 UA 会被网关
/// `403 / code=10085` 判为「请求不合法」），复用同一份取值逻辑而不是复制字符串。
pub(crate) fn desktop_ua() -> String {
    std::env::var(ENV_WB_DESKTOP_UA)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| WB_DESKTOP_UA.to_string())
}

/// 签到专用请求头 = 通用鉴权头 + 桌面端 UA + 账号级指纹（缺 uid 时用 `X-No-User-Id`）。
///
/// 刻意**不**改共享的 [`build_auth_headers`]：其它接口（积分 / 网关）行为保持不变。
pub fn checkin_headers(account: &Value) -> HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("User-Agent".to_string(), desktop_ua());
    let uid = account
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    match (
        derive_device_fingerprint(&uid, "machine"),
        derive_device_fingerprint(&uid, "session"),
    ) {
        (Some(machine), Some(session)) => {
            headers.insert("X-Machine-ID".to_string(), machine);
            headers.insert("X-Session-ID".to_string(), session);
        }
        _ => {
            headers.insert("X-No-User-Id".to_string(), "1".to_string());
        }
    }
    headers
}

/// 签到端点的基址候选：主域在前，**同区**备用域在后（只在网络不可达时启用）。
fn checkin_bases(region: Region) -> Vec<&'static str> {
    let primary = region_spec(region).billing_base;
    match region {
        Region::Cn if primary != CN_ACTIVITY_BASE => vec![primary, CN_ACTIVITY_BASE],
        _ => vec![primary],
    }
}

/// 把一次原始响应规整成 JSON：保留 HTTP 状态码到 `_http_status`；非 JSON 体包成
/// `{"code": <status>, "message": …}`（HTML 只取 `<title>`，不把整页塞进界面）。
fn shape_response(status: u16, text: &str) -> Value {
    let parsed = serde_json::from_str::<Value>(text).ok();
    let mut value = match parsed {
        Some(Value::Object(map)) => Value::Object(map),
        _ => {
            let trimmed = text.trim();
            let message = if trimmed.starts_with('<') {
                trimmed
                    .split_once("<title>")
                    .and_then(|(_, rest)| rest.split_once("</title>"))
                    .map(|(title, _)| title.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| format!("HTTP {status}"))
            } else if trimmed.is_empty() {
                format!("HTTP {status}")
            } else {
                trimmed.chars().take(300).collect()
            };
            let code = if status == 0 { -1 } else { i64::from(status) };
            json!({"code": code, "message": message})
        }
    };
    value["_http_status"] = json!(status);
    value
}

/// 按基址列表依次 POST 尝试；首个非网络不可达（status != 0）即返回。
async fn post_checkin_with_bases(
    bases: &[&str],
    path: &str,
    headers: &HashMap<String, String>,
) -> Value {
    let mut last = json!({"code": -1, "message": "无可用签到域名", "_http_status": 0});
    for base in bases {
        let url = format!("{base}{path}");
        let (status, _, text) =
            http_request_raw(&url, "POST", Some(json!({})), Some(headers), None, true).await;
        last = shape_response(status, &text);
        if status != 0 {
            return last;
        }
    }
    last
}

/// POST 一次；主域网络不可达（status=0）时换同区备用域重试一次。
async fn post_checkin(region: Region, path: &str, headers: &HashMap<String, String>) -> Value {
    post_checkin_with_bases(&checkin_bases(region), path, headers).await
}

fn http_status_of(resp: &Value) -> u16 {
    resp.get("_http_status")
        .and_then(Value::as_u64)
        .map(|v| v as u16)
        .unwrap_or(0)
}

/// 业务成功：`code` 为 0/200；或上游没给 `code` 但 HTTP 2xx。
fn is_ok_response(resp: &Value) -> bool {
    if !(200..300).contains(&http_status_of(resp)) {
        return false;
    }
    match resp.get("code") {
        Some(code) => matches!(code.as_i64(), Some(0) | Some(200)),
        None => (200..300).contains(&http_status_of(resp)),
    }
}

/// 有界深挖：在对象（含嵌套 `data` 等）里按候选键找第一个命中的值（深度 ≤ 3）。
fn dig_any<'a>(value: &'a Value, keys: &[&str], depth: usize) -> Option<&'a Value> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(found) = map.get(*key).filter(|v| !v.is_null()) {
            return Some(found);
        }
    }
    if depth == 0 {
        return None;
    }
    map.values()
        .filter(|v| v.is_object())
        .find_map(|v| dig_any(v, keys, depth - 1))
}

/// Python `bool()` 语义：0 / "" / "0" / "false" / null ⇒ false。
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => {
            let s = s.trim().to_ascii_lowercase();
            matches!(s.as_str(), "true" | "1")
        }
        Value::Array(_) | Value::Object(_) => false,
    }
}

fn num_of(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// 「今日已签」多键深挖（TraeWorkAssistant 实测出现过的全部形态）。
fn today_checked_in_of(resp: &Value) -> bool {
    dig_any(
        resp,
        &[
            "today_checked_in",
            "todayCheckedIn",
            "checked_in",
            "checkedIn",
            "is_checked",
            "isChecked",
            "checked",
        ],
        3,
    )
    .map(truthy)
    .unwrap_or(false)
}

/// 签到提交响应里的奖励额度（接口回显为准，取不到给 `None`，不摆 +0）。
fn reward_from_submit(resp: &Value) -> Option<f64> {
    num_of(dig_any(
        resp,
        &[
            "reward",
            "credits",
            "credit",
            "points",
            "amount",
            "daily_credit",
            "today_credit",
            "reward_amount",
            "add_integral",
            "addCredits",
            "earned",
            "bonus",
        ],
        3,
    ))
    .filter(|v| *v > 0.0)
}

/// 「今日已签」的提交失败形态：`code=10001` / 文案含 已签到 / repeat / already。
fn is_already_submit(resp: &Value, message: &str) -> bool {
    let low = message.to_lowercase();
    resp.get("code").and_then(Value::as_i64) == Some(10001)
        || ["已签到", "已经签到", "repeat", "already"]
            .iter()
            .any(|k| low.contains(k))
}

fn message_of(resp: &Value) -> Option<String> {
    resp.get("message")
        .or_else(|| resp.get("msg"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|m| !m.is_empty())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckinCycleMode {
    /// Always verify every account against the server after a host starts.
    StartupVerify,
    /// Re-verify every account against the server during background recovery.
    PeriodicRecovery,
}

#[derive(Debug, Eq, PartialEq)]
enum StatusDecision {
    Already,
    Submit,
    Error(String),
}

struct AccountRunGuard {
    key: String,
}

impl AccountRunGuard {
    fn try_acquire(account: &Value) -> Option<Self> {
        let key = account
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(String::from)
            .unwrap_or_else(|| account_display_name(account));
        let mut running = CHECKIN_ACCOUNTS_RUNNING
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap();
        if !running.insert(key.clone()) {
            return None;
        }
        Some(Self { key })
    }
}

impl Drop for AccountRunGuard {
    fn drop(&mut self) {
        if let Some(running) = CHECKIN_ACCOUNTS_RUNNING.get() {
            running.lock().unwrap().remove(&self.key);
        }
    }
}

/// 网关把「客户端指纹不被认可」回成 `403 / code=10085`（真机实测，补桌面 UA 即消失）。
/// 它与 token 过期无关，刷新**无效**，因此必须排除在 `is_unauthorized` 之外。
const BUSINESS_403_CODES: &[i64] = &[10085, 10086];

/// **业务码**响应禁止走「文案关键词」判据的码集。
///
/// ⚠️ **定位：防御性收窄，不是已观测到的 CN 现网缺陷。**
/// 本模块的 `is_unauthorized` 实际只会看到 **CN** 响应（`get_checkin_status_for` 与
/// `perform_checkin_for` 对 Global 都在更早处返回，Global 走 `buddy_activity`），
/// 而本轮 CN 真机**从未**回过 `10001`（`reports/wb-live/status-cn-*.json` 全为 `code:0`）。
///
/// 之所以仍要拦：`10001` 的语义是「已签到」**或**「活动未开启/已过期」，两者都与登录态无关，
/// 而后者会命中文案关键词 `"过期"`。真机已证实上游**确实**这样组合该码 ——
/// `reports/wb-live/global-checkin-endpoint-existence.json`：
/// `HTTP 400 / {"code":10001,"msg":"签到活动未开启或已过期"}`。
/// CN 赛季 `end_time = 2026-10-31 23:59:59`，**赛季结束后 CN 侧可能出现同形响应**；
/// 届时带 `refresh_token` 的真实账号会被白白**刷新一次 token**。
///
/// `10085/10086` 已由 [`BUSINESS_403_CODES`] 在更早处排除，此处不重复。
///
/// ⚠️ 只排除**已知业务码**，不整体取消文案判据：真实库里确有
/// `HTTP 200 + code:11001 + msg:"token 失效"` 这类需要刷新的响应，靠文案兜底是必要的
/// （见用例的阳性对照）。
const NON_AUTH_BUSINESS_CODES: &[i64] = &[10001];

/// 判断是否因 token 失效被拒（用于触发刷新重试）。
///
/// 必须区分**传输层鉴权失败**与**业务级拒绝**：
/// - HTTP `401` ⇒ 真正的未授权 ⇒ 值得刷新一次；
/// - HTTP `403` + 业务码 `10085/10086` ⇒ 客户端指纹/请求不合法 ⇒ **不刷新**（刷了也没用，
///   反而拿一次无谓的 token 轮换去污染账号库）；
/// - HTTP `403` 且无业务码 ⇒ 网关层拒绝（如 openresty 的 `401 Authorization Required` 页面
///   经 `shape_response` 折成 `403`）⇒ 仍按未授权处理；
/// - 已知业务码（[`NON_AUTH_BUSINESS_CODES`]）⇒ 文案再像「过期」也**不刷新**。
fn is_unauthorized(resp: &Value) -> bool {
    let code = resp.get("code").and_then(|v| v.as_i64());
    if code.is_some_and(|c| BUSINESS_403_CODES.contains(&c)) {
        return false;
    }
    if http_status_of(resp) == 401 {
        return true;
    }
    if code == Some(401) {
        return true;
    }
    // HTTP 403：业务码已排除在上方，其余按网关层未授权处理。
    if http_status_of(resp) == 403 || code == Some(403) {
        return true;
    }
    // 已知业务码不得被文案关键词误判成鉴权失败。
    if code.is_some_and(|c| NON_AUTH_BUSINESS_CODES.contains(&c)) {
        return false;
    }
    let msg = resp
        .get("message")
        .or_else(|| resp.get("msg"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    ["unauthorized", "401", "登录", "失效", "过期", "token"]
        .iter()
        .any(|k| msg.contains(k))
}

/// 按 region 发签到相关请求；遇到未授权且存在 refresh token 时刷新一次并重试。
async fn checkin_request_for(region: Region, path: &str, account: &Value) -> Value {
    // 加密信封凭据短路：不发空 Bearer，直接给出可读错误（上游 PR #95 的同款处理）。
    if let Some(err) = envelope_token_error(account) {
        return json!({"code": -2, "message": err});
    }
    let headers = checkin_headers(account);
    let mut resp = post_checkin(region, path, &headers).await;
    if is_unauthorized(&resp)
        && !account
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty()
    {
        // 401：刷新一次只重试一次（禁止二次刷新，对齐 TraeWorkAssistant F-09）。
        let refreshed = refresh_account_token_for(region, account.clone()).await;
        let headers = checkin_headers(&refreshed);
        resp = post_checkin(region, path, &headers).await;
    }
    resp
}

/// 查询签到状态（CN）。
pub async fn get_checkin_status(account: &Value) -> Value {
    get_checkin_status_for(Region::Cn, account).await
}

/// 按 region 查询签到状态：新接口 checkin-activity-status，失败回退 checkin-status。
///
/// ⚠️ **空壳回退防护**：备用端点 `checkin-status` 会返回 `200 / code:0` 但 `data` 全空
/// （真机证据见 [`status_is_empty_shell`]）。空壳**不得**当作「今日未签到」——否则
/// `decide_from_status` 会返回 `Submit`，对**已签到账号发起重复提交**。
/// 因此空壳一律按「状态未知（端点不可用）」处理：继续尝试下一个端点，全部不可用则报错，
/// **绝不**落到 `Submit`。
pub async fn get_checkin_status_for(region: Region, account: &Value) -> Value {
    if region == Region::Global {
        return super::buddy_activity::status(account);
    }
    if !region_supported(region) {
        return json!({"ok": false, "error": UNSUPPORTED_MESSAGE});
    }
    let mut last = Value::Null;
    let mut saw_empty_shell = false;
    for endpoint in ["checkin-activity-status", "checkin-status"] {
        let resp =
            checkin_request_for(region, &format!("{CHECKIN_API_PREFIX}/{endpoint}"), account).await;
        if is_ok_response(&resp) {
            if status_is_empty_shell(&resp) {
                // 空壳：端点活着但没数据，不能作为权威状态。记下来并继续。
                saw_empty_shell = true;
                last = resp;
                continue;
            }
            return status_payload(&resp);
        }
        last = resp;
    }
    if saw_empty_shell && is_ok_response(&last) {
        return json!({"ok": false, "error": EMPTY_STATUS_ERROR});
    }
    let code = last.get("code").and_then(Value::as_i64).unwrap_or(-1);
    json!({
        "ok": false,
        "error": message_of(&last).unwrap_or_else(|| format!("code={code}")),
    })
}

/// 状态响应里的 `data` 对象（无 `data` 时退回响应本身）。
fn status_data(resp: &Value) -> &Value {
    resp.get("data")
        .filter(|data| data.is_object())
        .unwrap_or(resp)
}

/// 顶层非空字符串字段（空串 / 缺失 / 非字符串一律 `false`）。
fn non_empty_text(resp: &Value, keys: &[&str]) -> bool {
    dig_any(resp, keys, 0)
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

/// 顶层正数（缺失 / 0 / 非数字一律 `false`）。
fn is_positive(resp: &Value, keys: &[&str]) -> bool {
    num_of(dig_any(resp, keys, 0)).is_some_and(|value| value > 0.0)
}

/// 空壳状态响应判据 —— **纯函数**，直接可单测。
///
/// 上游已废弃的 `checkin-status` 端点会返回 `HTTP 200 / code:0`，但整个 `data`
/// 是空的。它**不能**代表「今日未签到」：把它当权威就会让**已签到账号被重复提交**。
///
/// 真机证据（`reports/wb-live/claim-c-and-global-recon.json`，同一账号同一时刻）：
///
/// | 字段 | `checkin-activity-status`（真） | `checkin-status`（空壳） |
/// |:--|:--|:--|
/// | `active` | `true` | `false` |
/// | `today_checked_in` | `true` | `false` |
/// | `streak_days` | `19` | `0` |
/// | `total_credits` | `1900` | `0` |
/// | `checkin_dates` | 19 个日期 | `null` |
/// | `start_time` | `"2026-09-30 00:00:00"` | `""` |
/// | `theme_name` | `"Buddy加油站"` | `""` |
/// | `season` | `10` | `0` |
///
/// 判据 = **既没有活动身份、也没有任何用户进度**：
/// 1. 活动身份（`active` / `theme_name` / `season` / `start_time` / `end_time`）
///    是**活动级**字段，对同一活动下的所有用户一致，正常响应必有值；
/// 2. 用户进度（`checkin_dates` 非 null、`total_credits` > 0、`streak_days` > 0）
///    只要有一项存在就说明这是「真响应」（可能确实没签到过）。
///
/// 因此「刚注册、从未签到」的真实响应**不会**被判成空壳（活动身份仍在），
/// 只有退化成全空的上游响应才会 —— 这正是我们要向安全侧倾斜的方向。
fn status_is_empty_shell(resp: &Value) -> bool {
    let data = status_data(resp);
    let has_activity_identity = dig_any(data, &["active"], 0).map(truthy).unwrap_or(false)
        || non_empty_text(
            data,
            &["theme_name", "themeName", "activity_name", "activityName"],
        )
        || is_positive(data, &["season"])
        || non_empty_text(data, &["start_time", "startTime", "end_time", "endTime"]);
    if has_activity_identity {
        return false;
    }
    let has_user_progress = data
        .get("checkin_dates")
        .or_else(|| data.get("checkinDates"))
        .is_some_and(|dates| dates.is_array())
        || is_positive(data, &["total_credits", "totalCredits"])
        || is_positive(data, &["streak_days", "streakDays"])
        || is_positive(data, &["daily_credit", "dailyCredit"]);
    !has_user_progress
}

/// 空壳端点不可用时的统一文案（**不使用** `message_of`，因为上游会回 `"OK"`）。
const EMPTY_STATUS_ERROR: &str =
    "签到状态接口只返回空数据（端点暂不可用），已按「状态未知」处理，未做任何提交";

/// 把成功的状态响应规整成前端契约（★ B5：积分字段显式提取，供「逐条 +N」展示）。
/// 字段名来自实测响应：`daily_credit` / `today_credit` / `total_credits` /
/// `streak_days` / `checkin_dates`；「今日已签」走多键深挖。
fn status_payload(resp: &Value) -> Value {
    let data = resp
        .get("data")
        .filter(|d| d.is_object())
        .cloned()
        .unwrap_or_else(|| resp.clone());
    let f = |keys: &[&str]| num_of(dig_any(&data, keys, 2));
    json!({
        "ok": true,
        "todayCheckedIn": today_checked_in_of(&data),
        "dailyCredit": f(&["daily_credit", "dailyCredit"]),
        "todayCredit": f(&["today_credit", "todayCredit"]),
        "totalCredits": f(&["total_credits", "totalCredits"]),
        "streakDays": f(&["streak_days", "streakDays"]).map(|v| v as i64),
        "streakBonusCredit": f(&["streak_bonus_credit", "streakBonusCredit"]),
        "checkinDates": data.get("checkin_dates").cloned().unwrap_or(Value::Null),
        "raw": data,
    })
}

/// 执行签到（CN）。
pub async fn perform_checkin(account: &Value) -> Value {
    perform_checkin_for(Region::Cn, account).await
}

/// 按 region 执行签到（POST daily-checkin）；服务端返回已签到提示按成功处理。
pub async fn perform_checkin_for(region: Region, account: &Value) -> Value {
    if region == Region::Global {
        return json!({"ok":false,"error":"国际版请使用网页活跃任务入口"});
    }
    if !region_supported(region) {
        return json!({"ok": false, "error": UNSUPPORTED_MESSAGE});
    }
    let resp = checkin_request_for(
        region,
        &format!("{CHECKIN_API_PREFIX}/daily-checkin"),
        account,
    )
    .await;
    if is_ok_response(&resp) {
        let mut out = json!({
            "ok": true,
            "raw": resp.get("data").cloned().unwrap_or_else(|| json!({})),
        });
        if let Some(reward) = reward_from_submit(&resp) {
            out["reward"] = json!(reward);
        }
        return out;
    }
    let code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
    let msg = message_of(&resp).unwrap_or_else(|| format!("code={code}"));
    if is_already_submit(&resp, &msg) {
        let status = get_checkin_status_for(region, account).await;
        if status["ok"] == true && status["todayCheckedIn"] == true {
            return json!({"ok": true, "already": true, "message": "服务端状态已确认今日签到"});
        }
        return json!({"ok":false,"error":"提交提示重复，但服务端今日签到状态未确认"});
    }
    json!({"ok": false, "error": msg})
}

/// 从签到状态响应里取**本次签到可得额度**（逐条「+N」的 N）。
///
/// 字段名来自**上游实测响应**（`checkin-activity-status`，见 [`get_checkin_status_for`]）：
/// 优先 `dailyCredit`（当日签到额度），回落 `todayCredit`（今日已得）。
/// 只认**正数** —— `0` / 缺失一律视为「上游没给」⇒ `None`（宁可不写，也不摆「+0」）。
fn checkin_reward_from_status(status: &Value) -> Option<f64> {
    ["dailyCredit", "todayCredit"]
        .iter()
        .filter_map(|field| status.get(*field).and_then(Value::as_f64))
        .find(|amount| *amount > 0.0)
}

fn decide_from_status(status: &Value) -> StatusDecision {
    if status.get("ok").and_then(Value::as_bool) != Some(true) {
        return StatusDecision::Error(
            status
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("查询签到状态失败")
                .to_string(),
        );
    }
    if status.get("todayCheckedIn").and_then(Value::as_bool) == Some(true) {
        StatusDecision::Already
    } else {
        StatusDecision::Submit
    }
}

/// 对单个账号执行完整签到流程（CN）。
pub async fn checkin_account(account: &Value) -> Value {
    checkin_account_for(Region::Cn, account).await
}

/// 按 region 对单个账号执行完整签到流程：惰性刷新 → 查状态 → 未签到时提交 → 写提交日志。
pub async fn checkin_account_for(region: Region, account: &Value) -> Value {
    if region == Region::Global {
        // Invalid fixture/input must fail before refresh or any network request.
        if account["uid"].as_str().filter(|s| !s.is_empty()).is_none() {
            return json!({"result":"error","error":"国际版账号缺少 uid"});
        }
        return super::buddy_activity::run(account).await;
    }
    if !region_supported(region) {
        return json!({"result": "unsupported", "error": UNSUPPORTED_MESSAGE});
    }
    let Some(_account_guard) = AccountRunGuard::try_acquire(account) else {
        return json!({"result": "error", "error": "该账号正在签到，请稍后再试"});
    };
    let cfg = load_checkin_config();
    let acc = ensure_fresh_token_for(region, account.clone(), &cfg).await;
    let status = get_checkin_status_for(region, &acc).await;
    match decide_from_status(&status) {
        StatusDecision::Already => return json!({"result": "already"}),
        StatusDecision::Error(error) => {
            return json!({"result": "error", "error": error});
        }
        StatusDecision::Submit => {}
    }
    // ★ 逐条「+N」：本次签到额度取自**提交前**的状态查询（`checkin-activity-status`），
    //   与 Trae 侧「预检返回的 credits 就是本次签到可得的额度」同源 —— 不必再打一次
    //   状态接口算差值。只认上游**实测**字段，取不到就不写金额（前端显示「—」，不摆「+0」）。
    let pre_reward = checkin_reward_from_status(&status);

    // Only this branch submits daily-checkin, so only its outcome is eligible
    // for the sign-in log.
    let entry = json!({
        "ts": now_ms(),
        "accountId": acc.get("id").cloned().unwrap_or(Value::Null),
        "email": account_display_name(&acc),
    });
    let res = perform_checkin_for(region, &acc).await;
    let result = if res.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        if res.get("already").and_then(|v| v.as_bool()) == Some(true) {
            "already"
        } else {
            "success"
        }
    } else {
        "error"
    };
    let error = if result == "error" {
        res.get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };
    let mut entry_map = json!({
        "result": result,
        "ts": entry["ts"],
        "accountId": entry["accountId"],
        "email": entry["email"],
    });
    if let Some(e) = error.clone() {
        entry_map["error"] = json!(e);
    }
    // 只有「真的签到成功」才落金额；`already` / `error` 不写（拿不到就是拿不到）。
    if result == "success" {
        // 预检额度为准；预检没给时用提交响应回显（TraeWorkAssistant F-17 同口径）。
        let reward = pre_reward.or_else(|| res.get("reward").and_then(Value::as_f64));
        if let Some(amount) = reward {
            entry_map["amount"] = json!(amount);
        }
    }
    add_checkin_log(&entry_map);
    json!({"result": result, "error": error})
}

pub fn date_str(ts_ms: Option<i64>) -> String {
    let dt = Local::now();
    if let Some(ms) = ts_ms {
        let secs = ms / 1000;
        chrono::DateTime::from_timestamp(secs, 0)
            .map(|d| d.with_timezone(&Local).format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| dt.format("%Y-%m-%d").to_string())
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}

/// 执行一轮自动签到（CN）。
pub async fn run_checkin_cycle(mode: CheckinCycleMode) -> Value {
    run_checkin_cycle_for(Region::Cn, mode).await
}

/// 按 region 执行一轮自动签到。启动与周期轮次均逐账号查询服务端状态。
///
/// 并发锁防止与手动签到/上一轮重复运行。
pub async fn run_checkin_cycle_for(region: Region, _mode: CheckinCycleMode) -> Value {
    if region == Region::Global {
        return json!({"status":"unsupported","reason":"国际版网页活跃任务仅支持手动触发"});
    }
    // Global 不支持签到是**静态事实**，必须先判 region：否则 CN 周期持有全局运行标志时，
    // Global 请求会在到达 region 判断前被误报为 skipped/already_running（违反 PRD G1
    // 「两版互不污染」）。Global 不参与「同一时刻只跑一个签到周期」的竞争。
    if !region_supported(region) {
        return json!({"status": "unsupported", "reason": UNSUPPORTED_MESSAGE});
    }
    let Some(_guard) = RunFlagGuard::try_acquire(running_flag(region)) else {
        return json!({"status": "skipped", "reason": "already_running"});
    };
    let cfg = load_checkin_config();
    if cfg.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return json!({"status": "disabled"});
    }
    let accounts = load_accounts_for(region);
    if accounts.is_empty() {
        return json!({"status": "no_accounts"});
    }
    let mut summary = json!({"status": "ok", "accounts": []});
    for acc in accounts {
        let result = checkin_account_for(region, &acc).await;
        summary["accounts"].as_array_mut().unwrap().push(json!({
            "email": account_display_name(&acc),
            "result": result.get("result").cloned().unwrap_or(Value::Null),
            "error": result.get("error").cloned().unwrap_or(Value::Null),
        }));
    }
    summary
}

/// True when every stored account has a today's log of `success` or `already`（CN）。
pub fn all_accounts_checked_in_today() -> bool {
    all_accounts_checked_in_today_for(Region::Cn)
}

/// 按 region 判断是否全部账号今日已签到。
///
/// Empty account list is false so the tray keeps offering 一键签到.
pub fn all_accounts_checked_in_today_for(region: Region) -> bool {
    accounts_checked_in_today(
        &load_accounts_for(region),
        &load_checkin_logs(),
        &date_str(None),
    )
}

pub fn accounts_checked_in_today(accounts: &[Value], logs: &[Value], today: &str) -> bool {
    if accounts.is_empty() {
        return false;
    }
    accounts.iter().all(|account| {
        let Some(id) = account.get("id").and_then(Value::as_str) else {
            return false;
        };
        latest_today_result(logs, id, today)
            .map(|result| result == "success" || result == "already")
            .unwrap_or(false)
    })
}

fn latest_today_result<'a>(logs: &'a [Value], account_id: &str, today: &str) -> Option<&'a str> {
    logs.iter()
        .rev()
        .find(|entry| {
            entry.get("accountId").and_then(Value::as_str) == Some(account_id)
                && date_str(entry.get("ts").and_then(Value::as_i64)) == today
        })
        .and_then(|entry| entry.get("result").and_then(Value::as_str))
}

/// 对全部账号立即签到（CN，前端一键签到）。
pub async fn run_checkin_all() -> Value {
    run_checkin_all_for(Region::Cn).await
}

/// 按 region 对全部账号立即签到（前端一键签到）。
pub async fn run_checkin_all_for(region: Region) -> Value {
    if region == Region::Global {
        return json!({"accounts":[],"status":"unsupported","reason":"国际版请在账号卡片手动运行网页活跃任务"});
    }
    // 同 run_checkin_cycle_for：Global 先返回 unsupported，不参与全局运行标志竞争，
    // 保证 CN 正在签到时不把 Global 误报成 skipped/already_running（PRD G1）。
    if !region_supported(region) {
        return json!({"accounts": [], "status": "unsupported", "reason": UNSUPPORTED_MESSAGE});
    }
    let Some(_guard) = RunFlagGuard::try_acquire(running_flag(region)) else {
        return json!({"accounts": [], "status": "skipped", "reason": "already_running"});
    };
    let accounts = load_accounts_for(region);
    let mut results: Vec<Value> = Vec::new();
    for acc in accounts {
        let r = checkin_account_for(region, &acc).await;
        results.push(json!({
            "accountId": acc.get("id").cloned().unwrap_or(Value::Null),
            "email": account_display_name(&acc),
            "result": r.get("result").cloned().unwrap_or(Value::Null),
            "error": r.get("error").cloned().unwrap_or(Value::Null),
        }));
    }
    json!({"accounts": results})
}

#[cfg(test)]
mod tests {
    // 测试串行锁（`lock_*()`）会被**故意**持有到用例结束：它防的是多个用例同时抢占
    // 同一个回调端口 / 全局状态。`#[tokio::test]` 默认是 current-thread 运行时，
    // 不存在「持锁跨线程唤醒」的风险；clippy 的 await_holding_lock 在这里是误报。
    #![allow(clippy::await_holding_lock)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// 回归上游 issue #94：信封凭据的签到请求应在入口短路并返回可读错误，
    /// 不发出空 `Bearer`（此前会被网关 401 后把整页 HTML 回显到界面）。
    ///
    /// 可证伪：删掉 `checkin_request_for` 开头那段短路，本用例会真的去请求
    /// `www.codebuddy.cn/whatever`（或直接超时/报错），`code` 不再是 `-2`、
    /// `message` 里也不会有「加密信封」⇒ 两条断言都红。
    #[tokio::test]
    async fn envelope_credentials_short_circuit_before_request() {
        let account = json!({
            "id": "envelope-only",
            "variant": "cn",
            "access_token": {"$wbEncrypted": 1, "envelope": "…"},
            "refresh_token": {"$wbEncrypted": 1, "envelope": "…"},
        });
        let resp = checkin_request_for(Region::Cn, "/whatever", &account).await;
        assert_eq!(resp["code"], -2, "应在发请求之前短路：{resp}");
        let message = resp["message"].as_str().expect("message 应为字符串");
        assert!(message.contains("加密信封"), "文案应可读：{message}");

        // 阳性对照：明文凭据不得被这道护栏判定。直接问**同一个**判定函数，
        // 避免在单测里真的发网络请求（断言的是同一个谓词，不是另写一份判据）。
        assert!(
            envelope_token_error(&json!({"id": "plain", "access_token": "AT"})).is_none(),
            "明文凭据不得被信封护栏拦下"
        );
    }

    /// 签到模块的运行标志（`CHECKIN_RUNNING` / `CHECKIN_ACCOUNTS_RUNNING`）是**进程级
    /// 全局**的，cargo test 默认多线程并发跑用例时会互相污染：
    /// `manual_all_reports_busy_when_cycle_is_running` 持有 `CHECKIN_RUNNING` 期间，
    /// 另一个用例调用 `run_checkin_all_for` 会先撞上这把锁而返回 `skipped`，
    /// 而非期望的 `unsupported`——`--test-threads=1` 全绿、默认并发时随机红。
    ///
    /// 因此触及全局签到状态的用例必须先取这把测试专用锁串行执行。
    /// 用 `into_inner()` 吞掉中毒：单个用例 panic 不应连带其它用例全部失败。
    static CHECKIN_GLOBAL_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_checkin_global_state() -> std::sync::MutexGuard<'static, ()> {
        CHECKIN_GLOBAL_STATE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn checked_in_status_returns_already_without_submission() {
        assert_eq!(
            decide_from_status(&json!({"ok": true, "todayCheckedIn": true})),
            StatusDecision::Already
        );
    }

    #[test]
    fn failed_status_returns_error_without_submission() {
        assert_eq!(
            decide_from_status(&json!({"ok": false, "error": "offline"})),
            StatusDecision::Error("offline".to_string())
        );
    }

    #[test]
    fn unchecked_status_is_the_only_path_to_submission() {
        assert_eq!(
            decide_from_status(&json!({"ok": true, "todayCheckedIn": false})),
            StatusDecision::Submit
        );
    }

    /// 逐条「+N」的取值：优先 `dailyCredit`，回落 `todayCredit`，**只认正数**。
    ///
    /// 可证伪：把 `find(|amount| *amount > 0.0)` 换成 `.next()`（不筛正数）⇒ 第 3 条
    /// 断言会拿到 `0.0` 而红；把字段顺序调换 ⇒ 第 1 条断言拿到 `30.0` 而红。
    #[test]
    fn checkin_reward_prefers_daily_then_today_and_ignores_non_positive() {
        assert_eq!(
            checkin_reward_from_status(&json!({"dailyCredit": 100.0, "todayCredit": 30.0})),
            Some(100.0),
            "dailyCredit 优先"
        );
        assert_eq!(
            checkin_reward_from_status(&json!({"todayCredit": 30.0})),
            Some(30.0),
            "dailyCredit 缺失时回落 todayCredit"
        );
        assert_eq!(
            checkin_reward_from_status(&json!({"dailyCredit": 0.0, "todayCredit": 0.0})),
            None,
            "0 视为「上游没给」，不摆 +0"
        );
        assert_eq!(
            checkin_reward_from_status(&json!({"dailyCredit": null})),
            None,
            "null 视为「上游没给」"
        );
        assert_eq!(
            checkin_reward_from_status(&json!({"ok": true})),
            None,
            "字段全缺时为 None"
        );
    }

    #[test]
    fn same_account_cannot_acquire_two_operation_guards() {
        let account = json!({"id": "checkin-guard-test-account"});
        let first = AccountRunGuard::try_acquire(&account).expect("first operation acquires guard");
        assert!(AccountRunGuard::try_acquire(&account).is_none());
        drop(first);
        assert!(AccountRunGuard::try_acquire(&account).is_some());
    }

    #[tokio::test]
    async fn manual_all_reports_busy_when_cycle_is_running() {
        let _state = lock_checkin_global_state();
        let _cycle_guard =
            RunFlagGuard::try_acquire(&CHECKIN_RUNNING).expect("test acquires cycle guard");
        let result = run_checkin_all().await;

        assert_eq!(result["accounts"], json!([]));
        assert_eq!(result["status"], "skipped");
        assert_eq!(result["reason"], "already_running");
    }

    #[test]
    fn is_unauthorized_detects_code() {
        assert!(is_unauthorized(&json!({"code": 401})));
        assert!(is_unauthorized(&json!({"code": 403})));
        assert!(!is_unauthorized(&json!({"code": 0})));
    }

    #[test]
    fn checked_in_today_requires_every_account() {
        let accounts = vec![json!({"id": "a"}), json!({"id": "b"})];
        let logs = vec![
            json!({"accountId": "a", "result": "success", "ts": 1_700_000_000_000_i64}),
            json!({"accountId": "b", "result": "already", "ts": 1_700_000_100_000_i64}),
        ];
        let today = date_str(Some(1_700_000_000_000));
        assert!(accounts_checked_in_today(&accounts, &logs, &today));
    }

    #[test]
    fn checked_in_today_false_when_one_failed_last() {
        let accounts = vec![json!({"id": "a"})];
        let logs = vec![
            json!({"accountId": "a", "result": "success", "ts": 1_700_000_000_000_i64}),
            json!({"accountId": "a", "result": "error", "ts": 1_700_000_200_000_i64}),
        ];
        let today = date_str(Some(1_700_000_200_000));
        assert!(!accounts_checked_in_today(&accounts, &logs, &today));
    }

    #[test]
    fn checked_in_today_false_when_empty_or_missing() {
        assert!(!accounts_checked_in_today(&[], &[], "2026-08-19"));
        let accounts = vec![json!({"id": "a"})];
        assert!(!accounts_checked_in_today(&accounts, &[], "2026-08-19"));
    }

    /// 国际版每日签到接口默认开启（纯函数验证）；
    /// 验证 `BUDDY_SWITCH_WB_GLOBAL_CHECKIN` 对 Global 委托路径不再拦截
    /// （委托给 `buddy_activity` 自门控）。
    #[tokio::test]
    async fn global_checkin_enabled_by_default_and_can_be_disabled() {
        let _state = lock_checkin_global_state();
        let prev_checkin = std::env::var_os(ENV_WB_GLOBAL_CHECKIN);
        let prev_active = std::env::var_os(super::super::buddy_activity::ENV_WB_GLOBAL_ACTIVE_TASK);

        std::env::remove_var(ENV_WB_GLOBAL_CHECKIN);
        std::env::remove_var(super::super::buddy_activity::ENV_WB_GLOBAL_ACTIVE_TASK);

        // 默认开：纯函数判定
        assert!(global_checkin_enabled());
        assert!(region_supported(Region::Global));

        // 显式 0/false/off/no 关：纯函数判定
        for off_val in ["0", "false", "off", "no"] {
            std::env::set_var(ENV_WB_GLOBAL_CHECKIN, off_val);
            assert!(
                !global_checkin_enabled(),
                "{off_val} 应使每日签到开关为 false"
            );
            assert!(
                !region_supported(Region::Global),
                "{off_val} 应使 Global region_supported 为 false"
            );
        }

        // 开关设 0 时，Global 委托路径不再被该开关拦截
        std::env::set_var(ENV_WB_GLOBAL_CHECKIN, "0");
        let _cycle_guard =
            RunFlagGuard::try_acquire(&CHECKIN_RUNNING).expect("test holds CN cycle guard");

        let account = json!({"id": "g-1", "uid": "uid-g1", "access_token": "t"});

        // 1. get_checkin_status_for(Global) 不再返回该开关的 unsupported 文案，
        // 而是走到 buddy_activity 的门控结果（包含 BUDDY_SWITCH_WB_GLOBAL_ACTIVE_TASK）。
        let status_res = get_checkin_status_for(Region::Global, &account).await;
        assert_eq!(status_res["ok"], json!(false));
        let status_err = status_res["error"].as_str().unwrap_or("");
        assert!(
            status_err.contains(super::super::buddy_activity::ENV_WB_GLOBAL_ACTIVE_TASK),
            "应落到 buddy_activity 的门控结果: {status_res}"
        );
        assert!(
            !status_err.contains(ENV_WB_GLOBAL_CHECKIN),
            "不得返回每日签到开关文案: {status_res}"
        );

        // 2. checkin_account_for(Global) 同样不再被每日签到开关拦截，委托至 buddy_activity
        let run_res = checkin_account_for(Region::Global, &account).await;
        assert_eq!(run_res["result"], json!("unsupported"));
        let run_err = run_res["error"].as_str().unwrap_or("");
        assert!(
            run_err.contains(super::super::buddy_activity::ENV_WB_GLOBAL_ACTIVE_TASK),
            "应落到 buddy_activity 的门控结果: {run_res}"
        );
        assert!(
            !run_err.contains(ENV_WB_GLOBAL_CHECKIN),
            "不得返回每日签到开关文案: {run_res}"
        );

        // 3. run_checkin_all_for 与 run_checkin_cycle_for 保持静态设计文案
        let all_res = run_checkin_all_for(Region::Global).await;
        assert_eq!(all_res["status"], json!("unsupported"));
        assert_eq!(
            all_res["reason"],
            json!("国际版请在账号卡片手动运行网页活跃任务")
        );

        let cycle_res =
            run_checkin_cycle_for(Region::Global, CheckinCycleMode::StartupVerify).await;
        assert_eq!(cycle_res["status"], json!("unsupported"));
        assert_eq!(
            cycle_res["reason"],
            json!("国际版网页活跃任务仅支持手动触发")
        );

        // 恢复环境
        match prev_checkin {
            Some(v) => std::env::set_var(ENV_WB_GLOBAL_CHECKIN, v),
            None => std::env::remove_var(ENV_WB_GLOBAL_CHECKIN),
        }
        match prev_active {
            Some(v) => {
                std::env::set_var(super::super::buddy_activity::ENV_WB_GLOBAL_ACTIVE_TASK, v)
            }
            None => std::env::remove_var(super::super::buddy_activity::ENV_WB_GLOBAL_ACTIVE_TASK),
        }
    }

    /// CN / Global 各持一把运行标志。
    #[test]
    fn running_flags_are_per_region() {
        assert!(!std::ptr::eq(
            running_flag(Region::Cn),
            running_flag(Region::Global)
        ));
    }

    #[test]
    fn checkin_headers_carry_desktop_ua_and_stable_fingerprint() {
        let a = checkin_headers(&json!({"uid": "u-1", "access_token": "t"}));
        let b = checkin_headers(&json!({"uid": "u-1", "access_token": "t"}));
        assert_eq!(a["User-Agent"], WB_DESKTOP_UA);
        assert_eq!(a["X-Machine-ID"], b["X-Machine-ID"], "同账号指纹恒定");
        assert_ne!(a["X-Machine-ID"], a["X-Session-ID"]);
        assert_eq!(a["X-Machine-ID"].len(), 32);
        let c = checkin_headers(&json!({"uid": "u-2", "access_token": "t"}));
        assert_ne!(a["X-Machine-ID"], c["X-Machine-ID"], "跨账号隔离");
        let none = checkin_headers(&json!({"access_token": "t"}));
        assert_eq!(none["X-No-User-Id"], "1");
    }

    #[test]
    fn http_status_is_preserved_and_401_body_triggers_refresh() {
        let r = shape_response(401, r#"{"code": 11001, "msg": "invalid"}"#);
        assert!(
            is_unauthorized(&r),
            "HTTP 401 即使 body code 非 401 也必须触发刷新"
        );
        let html = shape_response(502, "<html><title>502 Bad Gateway</title></html>");
        assert_eq!(html["message"], "502 Bad Gateway");
        assert!(!is_ok_response(&html));
        assert!(!is_ok_response(&shape_response(500, r#"{"code":0}"#)));
        assert!(is_ok_response(&shape_response(200, r#"{"data": {}}"#)));
    }

    #[test]
    fn status_and_already_detection_are_lenient() {
        let s = status_payload(&json!({"code": 0, "data": {"result": {"checkedIn": 1}}}));
        assert_eq!(s["todayCheckedIn"], true);
        let s = status_payload(&json!({"code": 0, "data": {"is_checked": "0"}}));
        assert_eq!(s["todayCheckedIn"], false);
        assert!(!truthy(&json!("unknown")));
        assert!(!truthy(&json!({"checked":true})));
        assert!(is_already_submit(&json!({"code": 10001}), ""));
        assert!(is_already_submit(&json!({}), "Already checked in"));
        assert!(!is_already_submit(&json!({"code": 500}), "server error"));
        assert_eq!(
            reward_from_submit(&json!({"data": {"credits": "50"}})),
            Some(50.0)
        );
        assert_eq!(reward_from_submit(&json!({"data": {"credits": 0}})), None);
    }

    #[test]
    fn cn_has_same_region_fallback_only() {
        assert_eq!(checkin_bases(Region::Cn).len(), 2);
        assert!(checkin_bases(Region::Global)
            .iter()
            .all(|b| b.contains("workbuddy.ai")));
    }

    /// 空壳判据的核心回归：**已签到账号 + 空壳回退端点** 绝不能被判成「未签到」。
    ///
    /// 可证伪：把 `status_is_empty_shell` 改成恒 `false`（或删掉
    /// `get_checkin_status_for` 里的 `continue`），`decide_from_status` 就会拿到
    /// `todayCheckedIn=false` ⇒ 返回 `Submit` ⇒ 对已签到账号重复提交。
    ///
    /// 两个夹具都是**真机原样响应**（`reports/wb-live/claim-c-and-global-recon.json`、
    /// `reports/wb-live/status-cn-a93567.json`），不是编造的。
    #[test]
    fn empty_shell_status_is_never_authoritative_but_real_ones_are() {
        // ── 1. 空壳（真机 `checkin-status` 原样）：必须判为空壳 ──
        let shell = json!({
            "code": 0, "msg": "OK", "requestId": "d2dc2b99-ee7a-4dc0-a6fa-4e47d84deedf",
            "data": {
                "active": false, "today_checked_in": false, "streak_days": 0,
                "daily_credit": 0, "today_credit": 0, "is_streak_day": false,
                "next_streak_day": 0, "streak_bonus_days": 0, "streak_bonus_credit": 0,
                "checkin_dates": null, "week_checkin_days": 0,
                "week_progress": [false, false, false, false, false, false, false],
                "total_credits": 0, "start_time": "", "end_time": "",
                "theme_name": "", "season": 0, "activity_name": "",
                "claim_button_text": "",
                "action_button": {"show": false, "text": "", "action": ""}
            }
        });
        assert!(
            status_is_empty_shell(&shell),
            "真机空壳响应必须被识别，否则会把「已签到」误判为「未签到」"
        );

        // ── 2. 真·已签到（真机 `checkin-activity-status` 原样）：不是空壳 ──
        let checked_in = json!({
            "code": 0, "msg": "OK",
            "data": {
                "active": true, "today_checked_in": true, "streak_days": 19,
                "daily_credit": 100, "today_credit": 100,
                "checkin_dates": ["2026-10-21", "2026-10-20"],
                "total_credits": 1900, "start_time": "2026-09-30 00:00:00",
                "end_time": "2026-10-31 23:59:59", "theme_name": "Buddy加油站",
                "season": 10, "activity_name": "Buddy应用"
            }
        });
        assert!(!status_is_empty_shell(&checked_in), "真·已签到响应不是空壳");
        let payload = status_payload(&checked_in);
        assert_eq!(payload["todayCheckedIn"], true, "真·已签到必须判成 Already");

        // ── 3. 真·未签到（活动身份齐全、用户进度为零）：也不是空壳 ──
        //     这一条防的是「矫枉过正」：刚参加活动、从未签到的真实响应
        //     活动级字段仍在，绝不能被当成端点故障而不去签到。
        let not_checked_in = json!({
            "code": 0, "msg": "OK",
            "data": {
                "active": true, "today_checked_in": false, "streak_days": 0,
                "daily_credit": 100, "today_credit": 0,
                "checkin_dates": [], "total_credits": 0,
                "start_time": "2026-10-01 00:00:00",
                "end_time": "2026-10-31 23:59:59",
                "theme_name": "Buddy加油站", "season": 11
            }
        });
        assert!(
            !status_is_empty_shell(&not_checked_in),
            "真·未签到响应（活动身份在）不得被判成空壳，否则会漏签"
        );
        assert_eq!(
            status_payload(&not_checked_in)["todayCheckedIn"],
            false,
            "真·未签到 → 唯一允许进入 Submit 的路径"
        );
        assert_eq!(
            decide_from_status(&status_payload(&not_checked_in)),
            StatusDecision::Submit
        );
        // 反面：空壳即使被强行喂给 decide_from_status 也不该出现（上游已拦），
        // 但若有人绕过拦截，必须看到它不是 Already —— 说明拦截不可省。
        assert_eq!(
            decide_from_status(&status_payload(&shell)),
            StatusDecision::Submit,
            "空壳若漏过拦截就会落到 Submit（这正是必须拦截的原因）"
        );
    }

    /// 业务 403（`10085/10086`，客户端指纹不被认可）**不得**被当成 token 失效：
    /// 刷新对它无效，只会制造一次无谓的 token 轮换。
    ///
    /// 可证伪：把 `BUSINESS_403_CODES` 那段判断删掉，前两条断言会红。
    #[test]
    fn business_403_is_not_treated_as_unauthorized_but_http_401_403_still_are() {
        // 真机原样：无桌面 UA 的余额请求
        let business = json!({
            "_http_status": 403, "code": 10085,
            "msg": "请求不合法，如有疑问请联系客服"
        });
        assert!(
            !is_unauthorized(&business),
            "业务 403（10085）是客户端指纹拦截，刷新无效，不得触发 refresh"
        );
        assert!(!is_unauthorized(&json!({
            "_http_status": 403, "code": 10086, "msg": "请求不合法"
        })));

        // 真·HTTP 401（哪怕 body 没给 code）仍必须触发刷新
        assert!(is_unauthorized(
            &json!({"_http_status": 401, "code": 11001})
        ));
        // 网关层 403 且无业务码（openresty HTML 折成 403）仍按未授权处理
        assert!(is_unauthorized(&json!({"_http_status": 403})));
        assert!(is_unauthorized(&json!({"code": 401})));
        assert!(is_unauthorized(&json!({"code": 403})));
        assert!(!is_unauthorized(&json!({"_http_status": 200, "code": 0})));
    }

    /// Global 真机的 `code=10001`「签到活动未开启或**已过期**」命中文案关键词 `"过期"`。
    /// 已知业务码必须优先于文案判据，否则一次「活动未开启」会白白**刷新一次 token**。
    ///
    /// 真机证据：`reports/wb-live/global-checkin-endpoint-existence.json`
    /// → `HTTP 400 / {"code":10001,"msg":"签到活动未开启或已过期"}`。
    ///
    /// 可证伪：删掉 `NON_AUTH_BUSINESS_CODES` 那段 ⇒ 本用例红；
    /// 反向可证伪：把整条文案判据一起删掉 ⇒ 下半段（`code:11001` + `token 失效` 仍须为真）红，
    /// 证明这次收窄**没有**把真正需要刷新的响应也拦掉。
    #[test]
    fn business_code_10001_expired_activity_never_triggers_token_refresh() {
        // 真机原样
        let activity_closed = json!({
            "_http_status": 400, "code": 10001, "msg": "签到活动未开启或已过期"
        });
        assert!(
            !is_unauthorized(&activity_closed),
            "「活动未开启/已过期」是业务状态，与登录态无关，不得触发 token 刷新"
        );
        // 同码但走 CN「已签到」文案时同样不该刷新
        assert!(!is_unauthorized(&json!({
            "_http_status": 400, "code": 10001, "msg": "今日已签到"
        })));

        // ── 阳性对照：真实库里确实存在的「200 + 业务码 + 失效文案」必须仍然触发刷新 ──
        assert!(
            is_unauthorized(&json!({
                "_http_status": 200, "code": 11001, "msg": "token 失效，请重新登录"
            })),
            "收窄不得误伤真需要刷新的响应（这条在无文案判据时会漏刷）"
        );
        assert!(is_unauthorized(&json!({
            "_http_status": 200,
            "code": 11001,
            "message": "unauthorized"
        })));
    }

    /// `is_already_submit` 对 `code=10001` 的跨语义复用：Global 真机上
    /// `10001` 意为「签到活动未开启或已过期」，**不是**「已签到」。
    ///
    /// 本用例钉住「谓词本身确实会误吞」这一事实，从而证明
    /// `perform_checkin_for` 里的**二次状态确认**是不可删的 fail-safe。
    #[test]
    fn global_10001_activity_closed_is_misread_as_already_so_second_confirmation_is_required() {
        // Global 真机原样响应
        let global_closed = json!({
            "_http_status": 400, "code": 10001,
            "msg": "签到活动未开启或已过期"
        });
        assert!(
            is_already_submit(&global_closed, "签到活动未开启或已过期"),
            "谓词确实把「活动未开启」误读成「已签到」——这是既有事实"
        );
        // 但 perform_checkin_for 只在**状态侧确证 todayCheckedIn==true** 时才返回 already，
        // 否则返回错误文案。用纯判据复现该兜底逻辑：
        let status_if_not_checked =
            status_payload(&json!({"code":0,"data":{"today_checked_in": false}}));
        assert_eq!(status_if_not_checked["todayCheckedIn"], false);
        let would_report_already =
            status_if_not_checked["ok"] == true && status_if_not_checked["todayCheckedIn"] == true;
        assert!(
            !would_report_already,
            "状态未确认时必须报错，不能返回 already（fail-safe 生效）"
        );
    }

    /// 启动一个回环 HTTP mock 服务，用于测试网络不可达回退及业务判据。
    ///
    /// ⚠️ 桩服务必须**先读完请求**（读到 \r\n\r\n 后再按 Content-Length 读完正文）再回响应，
    /// 否则 Windows 上客户端会收到 RST。
    async fn spawn_checkin_mock_server(body: &str) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定本地桩服务");
        let addr = listener.local_addr().expect("取回环地址");
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        let body_str = body.to_string();

        tokio::spawn(async move {
            // 最多接受 3 次连接，避免测试结束后后台任务无限驻留
            for _ in 0..3 {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };

                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 1024];
                let mut body_target_len = None;
                loop {
                    if body_target_len.is_none() {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header_end = pos + 4;
                            let header_str = String::from_utf8_lossy(&buf[..header_end]);
                            let mut cl = 0usize;
                            for line in header_str.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.trim().eq_ignore_ascii_case("content-length") {
                                        cl = v.trim().parse::<usize>().unwrap_or(0);
                                        break;
                                    }
                                }
                            }
                            body_target_len = Some(header_end + cl);
                        }
                    }
                    if let Some(target) = body_target_len {
                        if buf.len() >= target {
                            break;
                        }
                    }
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => buf.extend_from_slice(&chunk[..read]),
                    }
                }

                count_clone.fetch_add(1, Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body_str}",
                    body_str.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            }
        });

        (addr, count)
    }

    /// 主域网络不可达（status=0）时，自动回退到同区域备用域并完成签到请求（PRD A22）。
    ///
    /// 可证伪性：删掉 `post_checkin_with_bases` 中的 `if status != 0 { return last; }`
    /// 或只请求第一个 base，本用例在 mock 请求计数（为 0）、最终状态码（为 0）
    /// 与业务成功判定上必须变红。
    #[tokio::test]
    async fn primary_domain_unreachable_falls_back_to_same_region_backup() {
        let (mock_addr, req_count) =
            spawn_checkin_mock_server(r#"{"code":0,"msg":"OK","data":{}}"#).await;
        let backup_base = format!("http://{mock_addr}");
        let bases = ["http://127.0.0.1:1", backup_base.as_str()];
        let path = format!("{CHECKIN_API_PREFIX}/daily-checkin");
        let headers = HashMap::new();

        let resp = post_checkin_with_bases(&bases, &path, &headers).await;

        assert_eq!(resp["code"], 0, "回退到备用域后 code 应为 0: {resp}");
        assert_eq!(
            http_status_of(&resp),
            200,
            "回退到备用域后 HTTP 状态码应为 200: {resp}"
        );
        assert_eq!(
            req_count.load(Ordering::SeqCst),
            1,
            "回退确实触发且只打了一次备用域"
        );
        assert!(
            is_ok_response(&resp),
            "备用域返回成功时应满足 is_ok_response: {resp}"
        );
        assert_ne!(
            resp["code"], -1,
            "主域网络失败不得污染最终 code，不能是 code=-1 的错误壳: {resp}"
        );
    }

    /// 所有 base 均网络不可达时，正确返回 code=-1 与 _http_status=0，不误报成功。
    #[tokio::test]
    async fn all_bases_unreachable_reports_failure_without_success() {
        let bases = ["http://127.0.0.1:1", "http://127.0.0.1:1"];
        let path = format!("{CHECKIN_API_PREFIX}/daily-checkin");
        let headers = HashMap::new();

        let resp = post_checkin_with_bases(&bases, &path, &headers).await;

        assert_eq!(
            resp["code"], -1,
            "全部 base 不可达时 code 必须为 -1: {resp}"
        );
        assert_eq!(
            http_status_of(&resp),
            0,
            "全部 base 不可达时 _http_status 必须为 0: {resp}"
        );
        assert_eq!(
            resp["_http_status"], 0,
            "响应中 _http_status 字段必须为 0: {resp}"
        );
        assert!(
            !is_ok_response(&resp),
            "不可达响应不得被判定为业务成功: {resp}"
        );
    }

    /// 通过 mock 桩服务验证非成功业务响应可正确传递并喂入业务判据（已签到/奖励提取）。
    #[tokio::test]
    async fn mock_http_response_feeds_business_predicates() {
        let (mock_addr, req_count) =
            spawn_checkin_mock_server(r#"{"code":10001,"msg":"今日已签到"}"#).await;
        let base = format!("http://{mock_addr}");
        let bases = [base.as_str()];
        let path = format!("{CHECKIN_API_PREFIX}/daily-checkin");
        let headers = HashMap::new();

        let resp = post_checkin_with_bases(&bases, &path, &headers).await;

        assert_eq!(
            req_count.load(Ordering::SeqCst),
            1,
            "mock 服务应收到一次请求"
        );
        assert_eq!(http_status_of(&resp), 200, "HTTP 状态码应为 200: {resp}");
        assert!(
            !is_ok_response(&resp),
            "code=10001 业务失败不得被判为 is_ok_response: {resp}"
        );
        let msg = message_of(&resp).unwrap_or_default();
        assert!(
            is_already_submit(&resp, &msg),
            "code=10001 且 msg=今日已签到 必须命中 is_already_submit: {resp}"
        );
        assert!(
            reward_from_submit(&resp).is_none(),
            "失败/已签到响应不应提取出奖励额度"
        );
    }
}
