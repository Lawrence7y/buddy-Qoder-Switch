//! 请求头形态 A/B 探针（**真机**）——列表端点 + claim 端点各测一遍。
//!
//! ## 为什么要有这个探针
//!
//! PRD 假设 P1-b 说「换成最小头就能领到」。而 A（TraeWorkAssistant，MIT）的
//! `issue #114` 实证说的是 **claim 端点**：带设备指纹头族被上游风控按形态拦截
//! （503 `RISK_DEPENDENCY_UNAVAILABLE`），最小头 200 通过。
//!
//! 所以 A/B 必须在 **claim 端点**上做，只在列表端点上做是不够的 —— 列表与 claim
//! 是两个不同的上游契约（`campaign.rs:705-717` 的注释也这么说）。
//!
//! ## 为什么安全
//!
//! claim 是**官方幂等**端点（A 的 R-4 抓包 + `campaign.rs` 模块头都写明）：
//! 对已领活动重复调用返回 `ALREADY_CLAIMED` / `replayed=true`，**无副作用**。
//! 因此本探针只在「今日已 `CLAIMED`」时才跑 claim 段 —— 既拿到真实形态对照，
//! 又绝不会凭空领走额度。
//!
//! 另外两条硬约束：**单账号**（服务端按「人」去重，多号同轮 = 刷同一份奖励）、
//! **相邻请求 ≥1.5s**。
//!
//! ## 运行
//!
//! ```text
//! BUDDY_SWITCH_HOME=C:/Users/Administrator/.buddy-qoder-switch-preview \
//! QODER_LIVE_REGION=cn QODER_LIVE_UID=<uid> \
//!   cargo run -p buddy-switch-core --example qoder_header_ab --release
//! ```

use std::time::Duration;

use buddy_switch_core::modules::qoder::{account, campaign, native, paths, region};
use serde_json::{json, Value};

const GAP: Duration = Duration::from_millis(1500);

fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 10 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(6).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}…{tail}")
}

/// 凭证清洗兜底（取证文件会进仓库）。
fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for token in text.split_inclusive(|c: char| c.is_whitespace() || c == '"' || c == ',') {
        let core = token.trim_matches(|c: char| c.is_whitespace() || c == '"' || c == ',');
        let secret = core.starts_with("eyJ")
            || core.len() > 120
            || core.starts_with("Bearer ")
            || core.eq_ignore_ascii_case("bearer");
        if secret {
            out.push_str("<REDACTED>");
            out.push_str(&token[core.len()..]);
        } else {
            out.push_str(token);
        }
    }
    out
}

/// 头名列表（**只取名，不取值** —— 值里有 Bearer token / 机器令牌）。
fn names(headers: &[(String, String)]) -> Vec<String> {
    headers.iter().map(|(name, _)| name.clone()).collect()
}

/// `campaign.rs:745 minimal_claim_headers` 的等价复刻（该 fn 是私有的）。
fn minimal_headers(access: &str) -> Vec<(String, String)> {
    vec![
        ("Authorization".into(), format!("Bearer {access}")),
        (
            "Cosy-ClientType".into(),
            campaign::DESKTOP_CLIENT_TYPE.into(),
        ),
        ("User-Agent".into(), "Qoder".into()),
        ("Content-Type".into(), "application/json".into()),
    ]
}

fn header_map(headers: &[(String, String)]) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            map.insert(n, v);
        }
    }
    map
}

/// 解析 claim 响应的业务码（409 必须按 body 分开：`CAMPAIGN_NOT_ACTIVE` ≠ 已领）。
fn claim_code(status: u16, body: &str) -> Value {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let payload = parsed.as_ref();
    let dig = |path: &str| -> Option<String> {
        payload
            .and_then(|v| v.pointer(path))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let code = dig("/errorCode")
        .or_else(|| dig("/code"))
        .or_else(|| dig("/code/code"))
        .or_else(|| dig("/failureCode"))
        .or_else(|| dig("/data/failureCode"))
        .unwrap_or_default();
    let status_word = dig("/data/status")
        .or_else(|| dig("/status"))
        .unwrap_or_default();
    let replayed = payload
        .and_then(|v| {
            v.pointer("/data/replayed")
                .or_else(|| v.pointer("/replayed"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false);
    json!({
        "httpStatus": status,
        "businessCode": code,
        "statusWord": status_word,
        "replayed": replayed,
        "bodyHead": body.chars().take(400).collect::<String>(),
    })
}

/// 把 `claim_code` 归到报告用的 outcome 词表。
fn outcome_of(code: &Value) -> &'static str {
    let http = code["httpStatus"].as_u64().unwrap_or(0) as u16;
    let business = code["businessCode"].as_str().unwrap_or("");
    let word = code["statusWord"].as_str().unwrap_or("");
    let replayed = code["replayed"].as_bool().unwrap_or(false);
    if http == 0 {
        return "network";
    }
    if http == 401 {
        return "auth-expired";
    }
    if business == "CAMPAIGN_NOT_ACTIVE" {
        return "stale-id";
    }
    if business == "SAME_PERSON_ALREADY_CLAIMED" || word == "BLOCKED" {
        return "risk-blocked";
    }
    if business == "REDEMPTION_CODE_OUT_OF_STOCK" {
        return "quota-exhausted";
    }
    if business == "RISK_BLOCKED" || business == "RISK_DEPENDENCY_UNAVAILABLE" {
        return "risk-blocked";
    }
    if business == "ALREADY_CLAIMED" || business == "REPLAYED" || replayed {
        return "already";
    }
    if (200..300).contains(&http) && matches!(word, "CLAIMED" | "GRANTED" | "SUCCESS") {
        return "claimed";
    }
    "unknown"
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let region_arg = match std::env::var("QODER_LIVE_REGION").as_deref() {
        Ok("global") => region::QoderRegion::Global,
        _ => region::QoderRegion::Cn,
    };
    let uid = std::env::var("QODER_LIVE_UID").expect("需 QODER_LIVE_UID");
    let home = std::env::var("BUDDY_SWITCH_HOME").unwrap_or_else(|_| "<unset>".into());

    // 生产库安全闸：本探针对 claim 端点发请求，绝不碰 ~/.buddy-switch。
    let accounts_path = paths::accounts_file_for(region_arg);
    let resolved = std::fs::canonicalize(&accounts_path).unwrap_or_else(|_| accounts_path.clone());
    let production =
        std::fs::canonicalize(std::env::var("USERPROFILE").unwrap_or_default() + "/.buddy-switch")
            .ok();
    if production.as_ref().is_some_and(|p| resolved.starts_with(p)) {
        println!(
            "!! REFUSING: accounts file is the production one ({}).",
            resolved.display()
        );
        println!("!! Set BUDDY_SWITCH_HOME to the preview home. Nothing was sent.");
        std::process::exit(3);
    }

    let base = region::realm_spec(region_arg).openapi_base;
    let access = account::find_account_for(region_arg, &uid)
        .and_then(|a| a.access_token.filter(|t| !t.is_empty()))
        .expect("账号库中找不到该 uid 的 access token");
    let identity = native::cached_identity(region_arg, &uid, false);

    println!("== qoder header A/B probe ==");
    println!("BUDDY_SWITCH_HOME = {home}");
    println!("accounts file     = {}", resolved.display());
    println!("region            = {}  base = {base}", region_arg.as_str());
    println!("uid               = {}", mask(&uid));
    println!(
        "native identity   = {} (runtime_info_exe = {})",
        if identity.is_some() {
            "PRESENT"
        } else {
            "ABSENT"
        },
        native::runtime_info_exe(region_arg)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<none>".into())
    );

    // 两种形态
    let (desktop, machine_state) =
        campaign::desktop_headers(region_arg, &uid, &access, identity.as_ref());
    let minimal = minimal_headers(&access);
    println!("\n-- header shapes --");
    println!(
        "desktop: {} headers, machineHeaders={machine_state}",
        desktop.len()
    );
    println!("         {}", names(&desktop).join(", "));
    println!("minimal: {} headers", minimal.len());
    println!("         {}", names(&minimal).join(", "));

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("client");

    let mut report = json!({
        "generatedAt": chrono::Local::now().to_rfc3339(),
        "probe": "qoder_header_ab (examples/)",
        "env": {
            "buddySwitchHome": home,
            "accountsFile": resolved.display().to_string(),
            "region": region_arg.as_str(),
            "openapiBase": base,
            "uidMasked": mask(&uid),
            "nativeIdentityPresent": identity.is_some(),
            "runtimeInfoExe": native::runtime_info_exe(region_arg).map(|p| p.display().to_string()),
            "machineHeadersState": machine_state,
        },
        "shapes": {
            "desktop": { "count": desktop.len(), "names": names(&desktop) },
            "minimal": { "count": minimal.len(), "names": names(&minimal) },
        },
        "list": {},
        "claim": {},
    });

    // ---- 列表端点 A/B -----------------------------------------------------
    println!(
        "\n== LIST endpoint A/B  GET {base}{} ==",
        "/sash/api/v1/me/campaigns"
    );
    for (label, headers) in [("desktop", &desktop), ("minimal", &minimal)] {
        let url = format!("{base}/sash/api/v1/me/campaigns");
        let sent = client.get(&url).headers(header_map(headers)).send().await;
        match sent {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                let value: Option<Value> = serde_json::from_str(&body).ok();
                let rows = value
                    .as_ref()
                    .and_then(|v| v.get("campaigns"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let claimables: Vec<String> = rows
                    .iter()
                    .filter(|row| {
                        row.get("claimStatus")
                            .and_then(Value::as_str)
                            .is_some_and(|s| s.eq_ignore_ascii_case("CLAIMABLE"))
                    })
                    .filter_map(|row| {
                        row.get("campaignId")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect();
                println!(
                    "  [{label:<7}] HTTP {status}  showCampaign={}  campaigns={}  claimable={}",
                    value
                        .as_ref()
                        .and_then(|v| v.get("showCampaign"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    rows.len(),
                    claimables.len()
                );
                report["list"][label] = json!({
                    "httpStatus": status,
                    "showCampaign": value.as_ref().and_then(|v| v.get("showCampaign")).cloned(),
                    "campaignCount": rows.len(),
                    "claimableIds": claimables,
                    "campaigns": rows.iter().map(|row| json!({
                        "campaignId": row.get("campaignId"),
                        "campaignKey": row.get("campaignKey"),
                        "actionType": row.get("actionType"),
                        "claimStatus": row.get("claimStatus"),
                        "benefitKind": row.pointer("/benefit/kind"),
                        "benefitAmount": row.pointer("/benefit/amount"),
                        "startAt": row.get("startAt"),
                        "endAt": row.get("endAt"),
                    })).collect::<Vec<_>>(),
                });
            }
            Err(error) => {
                println!("  [{label:<7}] NETWORK ERROR {error}");
                report["list"][label] = json!({ "networkError": error.to_string() });
            }
        }
        tokio::time::sleep(GAP).await;
    }

    // ---- claim 端点 A/B ---------------------------------------------------
    // 只对「服务端已 CLAIMED」的活动做幂等重放：证明形态差异，且无任何副作用。
    let claimed_id = report["list"]["desktop"]["campaigns"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| {
                    row["claimStatus"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case("CLAIMED"))
                        && row["benefitKind"]
                            .as_str()
                            .is_some_and(|k| k.eq_ignore_ascii_case("CREDITS"))
                })
                .and_then(|row| row["campaignId"].as_str())
                .map(str::to_string)
        });

    match claimed_id {
        None => {
            println!("\n== CLAIM endpoint A/B skipped ==");
            println!("  no CLAIMED credit campaign in today's list — nothing safe to replay.");
            report["claim"]["skipped"] = json!("no CLAIMED credit campaign to replay idempotently");
        }
        Some(campaign_id) => {
            println!(
                "\n== CLAIM endpoint A/B (idempotent replay of an already-CLAIMED campaign) =="
            );
            println!("  campaignId = {campaign_id}");
            println!(
                "  legal: server documents replay -> ALREADY_CLAIMED/replayed, no side effect"
            );
            for (label, headers) in [("desktop", &desktop), ("minimal", &minimal)] {
                let url = format!("{base}/sash/api/v1/me/campaigns/{campaign_id}/claim");
                // 与 campaign.rs 一致：空 body（Content-Length: 0），不是 `{}`。
                let sent = client
                    .post(&url)
                    .headers(header_map(headers))
                    .body("")
                    .send()
                    .await;
                match sent {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        let body = resp.text().await.unwrap_or_default();
                        let code = claim_code(status, &body);
                        let outcome = outcome_of(&code);
                        println!(
                            "  [{label:<7}] HTTP {status}  code={:?}  status={:?}  replayed={}  => {outcome}",
                            code["businessCode"].as_str().unwrap_or(""),
                            code["statusWord"].as_str().unwrap_or(""),
                            code["replayed"].as_bool().unwrap_or(false)
                        );
                        report["claim"][label] = json!({
                            "httpStatus": status,
                            "businessCode": code["businessCode"],
                            "statusWord": code["statusWord"],
                            "replayed": code["replayed"],
                            "outcome": outcome,
                            "bodyHead": code["bodyHead"],
                        });
                    }
                    Err(error) => {
                        println!("  [{label:<7}] NETWORK ERROR {error}");
                        report["claim"][label] = json!({ "networkError": error.to_string() });
                    }
                }
                tokio::time::sleep(GAP).await;
            }
        }
    }

    let dir = std::path::PathBuf::from("D:/Project/buddy-Qoder-Switch/reports/qoder-live");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!(
        "header-ab-{}-{}.json",
        region_arg.as_str(),
        chrono::Local::now().format("%Y%m%dT%H%M%S")
    ));
    let _ = std::fs::write(
        &path,
        scrub(&serde_json::to_string_pretty(&report).unwrap_or_default()),
    );
    println!("\nsaved {}", path.display());
}
