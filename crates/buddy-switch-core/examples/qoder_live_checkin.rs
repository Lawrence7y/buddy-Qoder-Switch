//! 真机签到验证探针（**产物外**，只用于 `qoder-live-verifier` 的现场取证）。
//!
//! 走的是**生产代码路径**（`checkin::claim_for` → `campaign::checkin_for`），不是
//! 另写一套 HTTP：这样才能证明「集成后的代码」到底做了什么，而不是「我以为它会做什么」。
//!
//! ## 为什么默认安全
//!
//! `checkin_for` 对 `CampaignClass::Already` 的条目**只归类、不发 claim**
//! （`campaign.rs:1028-1035`）。因此当今日奖励已被领过（活动列表全是 `CLAIMED`）时，
//! 本探针全程只有两次 GET，**不会向 claim 端点发请求**。
//! 真要领取请显式设 `QODER_LIVE_ALLOW_CLAIM=1`。
//!
//! ## 运行
//!
//! ```text
//! # 只读：读取预览库（绝不碰 ~/.buddy-switch 原库）
//! BUDDY_SWITCH_HOME=C:/Users/Administrator/.buddy-qoder-switch-preview \
//! QODER_LIVE_REGION=cn QODER_LIVE_UID=<uid> \
//!   cargo run -p buddy-switch-core --example qoder_live_checkin --release
//! ```
//!
//! ⚠️ `BUDDY_SWITCH_HOME` 指向的是**家目录**，代码再拼 `.buddy-switch`；
//! 所以这里传 `C:/Users/Administrator/.buddy-qoder-switch-preview`。
//!
//! ## 红线
//!
//! 全程零令牌输出：只打印 uid（脱敏）、活动 id / 状态 / 额度，**不打印 token**。

use std::path::PathBuf;

use buddy_switch_core::modules::qoder::{account, campaign, checkin, credits, paths, region};
use serde_json::{json, Value};

/// 脱敏：只留前 6 后 4（与 `store::mask` 同口径）。
fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 10 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(6).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}…{tail}")
}

/// 落盘前的凭证清洗：把任何 `eyJ…` / `Bearer …` / 超长 base64 片段打掉。
///
/// 本探针本该永远不碰到 token，但取证文件会被提交进仓库 —— 这一层是「万一」的兜底。
fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for token in text.split_inclusive(|c: char| c.is_whitespace() || c == '"' || c == ',') {
        let core = token.trim_matches(|c: char| c.is_whitespace() || c == '"' || c == ',');
        let looks_secret = core.starts_with("eyJ")
            || core.len() > 120
            || core.eq_ignore_ascii_case("bearer")
            || core.starts_with("Bearer ");
        if looks_secret {
            out.push_str("<REDACTED>");
            out.push_str(&token[core.len()..]);
        } else {
            out.push_str(token);
        }
    }
    out
}

/// 读账号库里某个 uid 的本地状态（**不读 token**，只读盖章字段）。
fn local_state(region_arg: region::QoderRegion, uid: &str) -> Value {
    let file = account::load_accounts_for(region_arg);
    match file.accounts.iter().find(|item| item.uid == uid) {
        Some(item) => json!({
            "found": true,
            "uid": mask(&item.uid),
            "nickname": item.nickname,
            "lastCheckin": item.last_checkin,
            "campaignCodes": item.campaign_codes,
            "campaignBlockedUntil": item.campaign_blocked_until,
            "updatedAt": item.updated_at,
            "hasAccessToken": item.access_token.as_deref().is_some_and(|t| !t.is_empty()),
        }),
        None => json!({ "found": false, "uid": mask(uid) }),
    }
}

// crate 只开了 `rt`（不是 `rt-multi-thread`），与 `buddy-switch-core` 的依赖声明一致。
#[tokio::main(flavor = "current_thread")]
async fn main() {
    // ---- 环境 -------------------------------------------------------------
    let region_arg = match std::env::var("QODER_LIVE_REGION").as_deref() {
        Ok("global") => region::QoderRegion::Global,
        _ => region::QoderRegion::Cn,
    };
    let uid = std::env::var("QODER_LIVE_UID").expect("需 QODER_LIVE_UID（账号库里的 uid）");
    let allow_claim = matches!(
        std::env::var("QODER_LIVE_ALLOW_CLAIM")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    );
    let home = std::env::var("BUDDY_SWITCH_HOME").unwrap_or_else(|_| "<unset>".into());

    // 安全闸：绝不在用户的生产库上跑（本探针会写 lastCheckin）。
    let accounts_path = paths::accounts_file_for(region_arg);
    let resolved = std::fs::canonicalize(&accounts_path).unwrap_or_else(|_| accounts_path.clone());
    let production = std::fs::canonicalize(PathBuf::from(
        std::env::var("USERPROFILE").unwrap_or_default() + "/.buddy-switch",
    ))
    .ok();
    let is_production = production
        .as_ref()
        .is_some_and(|prod| resolved.starts_with(prod));

    println!("== qoder live checkin probe ==");
    println!("BUDDY_SWITCH_HOME = {home}");
    println!("accounts file     = {}", resolved.display());
    println!("region            = {}", region_arg.as_str());
    println!("uid               = {}", mask(&uid));
    println!("allowClaim        = {allow_claim}");
    if is_production {
        println!("\n!! REFUSING TO RUN: accounts file resolves inside the production");
        println!("!! directory (~/.buddy-switch). Re-run with BUDDY_SWITCH_HOME pointing");
        println!("!! at the preview copy. Nothing was written.");
        std::process::exit(3);
    }

    let report_dir = PathBuf::from("D:/Project/buddy-Qoder-Switch/reports/qoder-live");
    let _ = std::fs::create_dir_all(&report_dir);
    let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S").to_string();

    // ---- 1. 账号本地状态（改之前）-----------------------------------------
    let before = local_state(region_arg, &uid);
    println!("\n-- local state BEFORE --");
    println!(
        "{}",
        serde_json::to_string_pretty(&before).unwrap_or_default()
    );

    // ---- 2. 额度基线 -------------------------------------------------------
    let access = account::find_account_for(region_arg, &uid)
        .and_then(|item| item.access_token.clone())
        .expect("账号库中找不到该 uid 的 access token");
    let quota_before = credits::region_view(region_arg).await;
    let row_before = quota_before["accounts"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["uid"] == json!(uid)))
        .cloned()
        .unwrap_or(Value::Null);
    println!("\n-- quota BEFORE --");
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "remaining": row_before.get("remaining"),
            "used": row_before.get("used"),
            "total": row_before.get("total"),
            "ok": row_before.get("ok"),
        }))
        .unwrap_or_default()
    );

    // ---- 3. 活动列表（生产解析路径）--------------------------------------
    // 先把「身份到底从哪来」查清楚：这决定 desktop_headers 到底发不发 cosy-machine*。
    let exe = buddy_switch_core::modules::qoder::native::runtime_info_exe(region_arg);
    println!("\n-- native identity source --");
    println!(
        "runtime_info_exe   = {}",
        exe.as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<none>".to_string())
    );
    println!(
        "nativeDisabled()   = {}",
        buddy_switch_core::modules::qoder::native::disabled()
    );
    println!(
        "cachedIdentity     = {}",
        match buddy_switch_core::modules::qoder::native::cached_identity(region_arg, &uid, false) {
            Some(_) => "Some(<machine identity present — values not printed>)",
            None => "None",
        }
    );

    let snapshot = campaign::fetch_campaigns(region_arg, &uid, &access).await;
    println!("\n-- campaigns (production parser) --");
    println!(
        "ok={} available={} showCampaign={} claimable={} identity={} machineHeaders={} hint={:?} error={:?}",
        snapshot.ok,
        snapshot.available,
        snapshot.show_campaign,
        snapshot.claimable,
        snapshot.identity,
        snapshot.machine_headers,
        snapshot.hint,
        snapshot.error
    );
    let mut items = Vec::new();
    for item in &snapshot.items {
        let class = campaign::classify(item);
        println!(
            "- key={} id={} class={:?} action={} claimStatus={} kind={} amount={} startAt={} endAt={}",
            item.campaign_key,
            item.campaign_id,
            class,
            item.action_type,
            item.claim_status,
            item.benefit_kind,
            item.benefit_amount,
            item.start_at,
            item.end_at
        );
        items.push(campaign::campaign_to_value(item));
    }

    // 无 claimable 且未显式允许 ⇒ 不调用 claim_for（它仍只读，但没必要多打一次上游）。
    // 注意：即使有 claimable，未 allow_claim 也绝不落 claim —— claim_for 会真的领。
    let has_claimable = items.iter().any(|item| item["class"] == "claimable");
    if has_claimable && !allow_claim {
        println!(
            "\n!! 列表中存在 claimable 活动，但未设 QODER_LIVE_ALLOW_CLAIM=1 —— 不调用 claim_for。"
        );
    }

    // ---- 4. 生产入口 checkin::claim_for -----------------------------------
    let claim_result = if allow_claim || !has_claimable {
        println!("\n-- checkin::claim_for (production entry) --");
        match checkin::claim_for(region_arg, &uid).await {
            Ok(value) => {
                let text = serde_json::to_string_pretty(&value).unwrap_or_default();
                println!("{}", scrub(&text));
                Some(value)
            }
            Err(error) => {
                println!("ERR {error}");
                Some(json!({ "error": error }))
            }
        }
    } else {
        println!("\n-- checkin::claim_for SKIPPED (would claim; not authorised) --");
        None
    };

    // ---- 5. 账号本地状态（改之后）----------------------------------------
    let after = local_state(region_arg, &uid);
    println!("\n-- local state AFTER --");
    println!(
        "{}",
        serde_json::to_string_pretty(&after).unwrap_or_default()
    );

    // ---- 6. 额度（改之后）-------------------------------------------------
    let quota_after = credits::region_view(region_arg).await;
    let row_after = quota_after["accounts"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["uid"] == json!(uid)))
        .cloned()
        .unwrap_or(Value::Null);
    println!("\n-- quota AFTER --");
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "remaining": row_after.get("remaining"),
            "used": row_after.get("used"),
            "total": row_after.get("total"),
            "ok": row_after.get("ok"),
        }))
        .unwrap_or_default()
    );

    // ---- 7. 落盘 -----------------------------------------------------------
    let artifact = json!({
        "generatedAt": chrono::Local::now().to_rfc3339(),
        "probe": "qoder_live_checkin (examples/)",
        "env": {
            "buddySwitchHome": home,
            "accountsFile": resolved.display().to_string(),
            "region": region_arg.as_str(),
            "allowClaim": allow_claim,
        },
        "localStateBefore": before,
        "localStateAfter": after,
        "campaigns": {
            "ok": snapshot.ok,
            "available": snapshot.available,
            "showCampaign": snapshot.show_campaign,
            "claimableFlag": snapshot.claimable,
            "identity": snapshot.identity,
            "machineHeaders": snapshot.machine_headers,
            "hint": snapshot.hint,
            "error": snapshot.error,
            "items": items,
        },
        "claimForResult": claim_result,
        "quotaBefore": {
            "remaining": row_before.get("remaining"),
            "used": row_before.get("used"),
            "total": row_before.get("total"),
        },
        "quotaAfter": {
            "remaining": row_after.get("remaining"),
            "used": row_after.get("used"),
            "total": row_after.get("total"),
        },
    });
    let text = serde_json::to_string_pretty(&artifact).unwrap_or_default();
    let out = report_dir.join(format!("checkin-for-{}-{stamp}.json", region_arg.as_str()));
    let _ = std::fs::write(&out, scrub(&text));
    println!("\nsaved {}", out.display());
}
