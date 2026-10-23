//! WorkBuddy CN 签到**写侧**真机验证探针（examples/，只新增不改动既有源码）。
//!
//! 走**生产代码路径**（`checkin::perform_checkin_for` → `post_checkin` →
//! `checkin_request_for`），不是另写一套 HTTP：这样才能证明整合版自己的
//! 请求层（桌面 UA + 指纹 + 状态码保留 + 已签判定）在真机上到底做了什么。
//!
//! ## 运行
//!
//! ```text
//! BUDDY_SWITCH_HOME=C:/Users/Administrator/.buddy-qoder-switch-preview \
//!   cargo run -p buddy-switch-core --example wb_live_checkin --release
//! ```
//!
//! ⚠️ `BUDDY_SWITCH_HOME` 指向**家目录**（代码再拼 `.buddy-switch`）。
//!
//! ## 安全闸
//!
//! - 解析出的账号文件若落在生产目录 `~/.buddy-switch` ⇒ 拒绝运行；
//! - 提交前先查服务端 `today_checked_in`：已签 ⇒ 不发 POST（防重复提交）；
//! - 只处理**第一个**带 access_token 的账号（单账号、串行，绝不并发）；
//! - 全程零令牌输出（只打印 uid 脱敏 + 结果负载）。

use std::path::PathBuf;

use buddy_switch_core::modules::region::accounts_file_for;
use buddy_switch_core::modules::{account, checkin, region::Region};
use serde_json::{json, Value};

fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 10 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(6).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}…{tail}")
}

/// 落盘前的凭证清洗（与 qoder 示例同口径）。
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

fn status_brief(status: &Value) -> Value {
    json!({
        "ok": status.get("ok"),
        "todayCheckedIn": status.get("todayCheckedIn"),
        "streakDays": status.get("streakDays"),
        "totalCredits": status.get("totalCredits"),
        "dailyCredit": status.get("dailyCredit"),
        "error": status.get("error"),
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let home = std::env::var("BUDDY_SWITCH_HOME").unwrap_or_else(|_| "<unset>".into());
    let pick_uid = std::env::var("WB_LIVE_UID").ok();

    // ---- 安全闸：绝不在生产库上跑 ----------------------------------------
    let accounts_file = accounts_file_for(Region::Cn);
    let resolved = std::fs::canonicalize(&accounts_file).unwrap_or_else(|_| accounts_file.clone());
    let production = std::fs::canonicalize(PathBuf::from(
        std::env::var("USERPROFILE").unwrap_or_default() + "/.buddy-switch",
    ))
    .ok();
    if production
        .as_ref()
        .is_some_and(|prod| resolved.starts_with(prod))
    {
        println!("!! REFUSING: accounts dir resolves inside the production ~/.buddy-switch.");
        println!("!! Set BUDDY_SWITCH_HOME to the preview copy. Nothing was written.");
        std::process::exit(3);
    }

    println!("== wb live checkin probe (production code path) ==");
    println!("BUDDY_SWITCH_HOME = {home}");
    println!("accounts file     = {}", resolved.display());

    // ---- 选号：第一个带 access_token 的账号（可用 WB_LIVE_UID 指定）--------
    let accounts = account::load_accounts_for(Region::Cn);
    let account = accounts
        .iter()
        .filter(|item| {
            pick_uid
                .as_deref()
                .is_none_or(|want| item.get("uid").and_then(Value::as_str) == Some(want))
        })
        .find(|item| {
            item.get("access_token")
                .and_then(Value::as_str)
                .is_some_and(|token| !token.is_empty())
        })
        .cloned();
    let Some(account) = account else {
        println!("!! 没有带 access_token 的 CN 账号，结束。");
        return;
    };
    let uid = account
        .get("uid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    println!("account = {} ({})", mask(&uid), mask(&uid));

    let report_dir = PathBuf::from("D:/Project/buddy-Qoder-Switch/reports/wb-live");
    let _ = std::fs::create_dir_all(&report_dir);
    let stamp = chrono::Local::now().format("%Y%m%dT%H%M%S").to_string();

    // ---- 1. 提交前服务端状态（权威判据）-----------------------------------
    let before = checkin::get_checkin_status_for(Region::Cn, &account).await;
    println!("\n-- status BEFORE --");
    println!(
        "{}",
        serde_json::to_string_pretty(&status_brief(&before)).unwrap_or_default()
    );
    if before["todayCheckedIn"] == json!(true) {
        println!("\n!! 服务端显示今日已签（可能被客户端抢先）——不发 POST。如实结束。");
        let artifact = json!({
            "generatedAt": chrono::Local::now().to_rfc3339(),
            "probe": "wb_live_checkin (examples/)",
            "account": mask(&uid),
            "outcome": "already-checked-in-before-probe",
            "statusBefore": status_brief(&before),
        });
        let out = report_dir.join(format!("checkin-write-cn-{stamp}.json"));
        let _ = std::fs::write(
            &out,
            scrub(&serde_json::to_string_pretty(&artifact).unwrap_or_default()),
        );
        println!("saved {}", out.display());
        return;
    }

    // ---- 2. 生产路径提交 --------------------------------------------------
    println!("\n-- perform_checkin_for (production entry) --");
    let result = checkin::perform_checkin_for(Region::Cn, &account).await;
    println!(
        "{}",
        scrub(&serde_json::to_string_pretty(&result).unwrap_or_default())
    );

    // ---- 3. 提交后服务端状态（翻转核验）-----------------------------------
    let after = checkin::get_checkin_status_for(Region::Cn, &account).await;
    println!("\n-- status AFTER --");
    println!(
        "{}",
        serde_json::to_string_pretty(&status_brief(&after)).unwrap_or_default()
    );

    // ---- 4. 判定与落盘 ----------------------------------------------------
    let flipped = after["todayCheckedIn"] == json!(true) && before["todayCheckedIn"] != json!(true);
    println!(
        "\n== VERDICT: todayCheckedIn {} → {}（{}）==",
        before["todayCheckedIn"],
        after["todayCheckedIn"],
        if flipped {
            "翻转 ✓"
        } else {
            "未翻转 ✗"
        }
    );

    let artifact = json!({
        "generatedAt": chrono::Local::now().to_rfc3339(),
        "probe": "wb_live_checkin (examples/, production code path)",
        "account": mask(&uid),
        "result": result,
        "statusBefore": status_brief(&before),
        "statusAfter": status_brief(&after),
        "todayCheckedInFlipped": flipped,
    });
    let out = report_dir.join(format!("checkin-write-cn-{stamp}.json"));
    let _ = std::fs::write(
        &out,
        scrub(&serde_json::to_string_pretty(&artifact).unwrap_or_default()),
    );
    println!("saved {}", out.display());
}
