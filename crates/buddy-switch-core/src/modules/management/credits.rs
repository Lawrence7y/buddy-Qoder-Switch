//! 积分数据聚合、到期日历与多平台投影引擎（FUS-W03 / W04）。
//!
//! 统一聚合 WorkBuddy、Trae 与 Qoder 三个平台的真实积分数据，
//! 计算当前余额、区间收支、逐包明细、可点击到期日历与来源/缓存状态。

use chrono::{Datelike, Local, TimeZone};
use std::collections::{BTreeMap, HashMap};

use crate::modules::config::now_ms;
use crate::modules::management::models::{
    AccountRef, CreditEvent, CreditObservation, CreditPackageItem, CreditSummaryProjection,
    CreditSummaryStats, ExpiryCalendarDateGroup, ExpiryCalendarItem, ManagementDataStatus,
};
use crate::modules::management::store;
use crate::modules::product::Scope;
use crate::modules::trae::region::TraeRegion;
use crate::modules::trae::variant::TraeVariant;

/// 解析并校验作用域，未知作用域 fail-closed。
pub fn validate_scope(scope_str: &str) -> Result<Scope, String> {
    Scope::parse(scope_str).map_err(|e| format!("未知或非法作用域 '{scope_str}': {e}"))
}

/// 格式化毫秒时间戳为本地日期字符串 YYYY-MM-DD。
pub fn format_local_date(ts_ms: i64) -> String {
    if let Some(dt) = Local.timestamp_millis_opt(ts_ms).single() {
        format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
    } else {
        "未知日期".to_string()
    }
}

/// 计算距离当前时刻的自然日天数差（同一自然日未来/过去均为 0，次日为 1，依此类推）。
pub fn days_until_ms(ts_ms: i64, now: i64) -> i64 {
    let ts_date = Local
        .timestamp_millis_opt(ts_ms)
        .single()
        .map(|dt| dt.date_naive());
    let now_date = Local
        .timestamp_millis_opt(now)
        .single()
        .map(|dt| dt.date_naive());
    match (ts_date, now_date) {
        (Some(td), Some(nd)) => {
            let diff = (td - nd).num_days();
            diff.max(0)
        }
        _ => 0,
    }
}

fn trae_variant_for_region(region: TraeRegion) -> TraeVariant {
    match region {
        TraeRegion::Cn => TraeVariant::TraeMerged,
        TraeRegion::Global => TraeVariant::GlobalTraeMerged,
    }
}

/// 将已有平台的本地数据（账号文件、既有快照、签到日志）同步导入 SQLite 库。
pub fn sync_from_existing_stores(conn: &rusqlite::Connection, scope: Scope) -> Result<(), String> {
    let scope_str = scope.as_str();
    let now = now_ms();

    match scope {
        Scope::WbCn | Scope::WbGlobal => {
            let region = scope
                .wb_region()
                .ok_or_else(|| "无法获取 WB 区域".to_string())?;
            let accounts = crate::modules::account::load_accounts_for(region);
            for acc in &accounts {
                let aid = acc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let name = crate::modules::account::account_display_name(acc);
                let acc_ref = AccountRef {
                    scope: scope_str.to_string(),
                    account_id: aid.clone(),
                    display_name: name.clone(),
                    is_active: true,
                    updated_at_ms: now,
                };
                let _ = store::upsert_account_ref(conn, &acc_ref);

                // 尝试从既有 snapshots 恢复历史余额观察
                let snapshots = crate::modules::credit_usage::load_snapshots();
                for s in snapshots {
                    if let (Some(s_aid), Some(ts), Some(rem), Some(tot)) = (
                        s.get("accountId").and_then(|v| v.as_str()),
                        s.get("ts").and_then(|v| v.as_i64()),
                        s.get("remaining").and_then(|v| v.as_f64()),
                        s.get("total").and_then(|v| v.as_f64()),
                    ) {
                        if s_aid == aid {
                            let obs = CreditObservation {
                                id: None,
                                scope: scope_str.to_string(),
                                account_id: aid.clone(),
                                source: "snapshot".to_string(),
                                remaining: rem,
                                total: tot,
                                unit: "credits".to_string(),
                                is_stale: false,
                                is_comparable: true,
                                observed_at_ms: ts,
                            };
                            let _ = store::insert_credit_observation(conn, &obs);
                        }
                    }
                }
            }

            // 同步签到日志流水（仅记录具备有效 accountId 的条目，不使用模糊昵称串联 ID）
            let checkins = crate::modules::config::load_checkin_logs();
            for item in checkins {
                if let (Some(ts), Some(aid), Some(res)) = (
                    item.get("timestamp").and_then(|v| v.as_i64()),
                    item.get("accountId").and_then(|v| v.as_str()),
                    item.get("result").and_then(|v| v.as_str()),
                ) {
                    let acc_name = item
                        .get("accountName")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let reward_amt = item.get("reward").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let evt = CreditEvent {
                        id: None,
                        scope: scope_str.to_string(),
                        account_id: aid.to_string(),
                        account_name: acc_name,
                        event_id: None,
                        event_kind: "checkin".to_string(),
                        event_type: "checkin".to_string(),
                        amount: reward_amt,
                        change_amount: reward_amt,
                        balance_after: 0.0,
                        unit: "credits".to_string(),
                        occurred_at_ms: ts,
                        recorded_at: ts / 1000,
                        evidence_level: "checkin_receipt".to_string(),
                        note: Some(format!("签到回执: {res}")),
                        reason: Some(format!("签到回执: {res}")),
                    };
                    let _ = store::insert_credit_event(conn, &evt);
                }
            }
        }
        Scope::TraeCn | Scope::TraeGlobal => {
            let region = scope
                .trae_region()
                .ok_or_else(|| "无法获取 Trae 区域".to_string())?;
            let accs_file = crate::modules::trae::account::load_accounts_for_region(region);
            for acc in &accs_file.accounts {
                let uid = acc.user_id.clone().unwrap_or_else(|| acc.name.clone());
                let acc_ref = AccountRef {
                    scope: scope_str.to_string(),
                    account_id: uid.clone(),
                    display_name: acc.name.clone(),
                    is_active: true,
                    updated_at_ms: now,
                };
                let _ = store::upsert_account_ref(conn, &acc_ref);
            }

            // 读取 Trae 侧已有的 remaining_credits 与 packages
            let variant = trae_variant_for_region(region);
            let rem_file = crate::modules::trae::credits::load_remaining_for(variant);
            for (uid, &rem_credits) in &rem_file.credits {
                let packages = rem_file.packages.get(uid).cloned().unwrap_or_default();
                let total_credits = packages
                    .iter()
                    .map(|p| p.total)
                    .sum::<f64>()
                    .max(rem_credits);
                let obs = CreditObservation {
                    id: None,
                    scope: scope_str.to_string(),
                    account_id: uid.clone(),
                    source: "official".to_string(),
                    remaining: rem_credits,
                    total: total_credits,
                    unit: "credits".to_string(),
                    is_stale: false,
                    is_comparable: true,
                    observed_at_ms: now,
                };
                let _ = store::insert_credit_observation(conn, &obs);

                for (idx, pkg) in packages.iter().enumerate() {
                    let pkg_name = pkg
                        .package_name
                        .clone()
                        .or_else(|| pkg.package_code.clone())
                        .unwrap_or_else(|| format!("积分包 #{}", idx + 1));
                    let expire_at_ms = pkg.expire_at.map(|sec| sec * 1000);
                    let pkg_id = pkg
                        .package_code
                        .clone()
                        .unwrap_or_else(|| format!("trae_pkg_{idx}"));

                    let expire_time = expire_at_ms.map(|ms| ms / 1000);
                    let expire_time_formatted = expire_at_ms.map(|ms| format_local_date(ms));

                    let pkg_item = CreditPackageItem {
                        id: None,
                        scope: scope_str.to_string(),
                        account_id: uid.clone(),
                        account_name: uid.clone(),
                        package_id: pkg_id,
                        name: pkg_name.clone(),
                        package_name: pkg_name,
                        package_type: "addon".to_string(),
                        remaining: pkg.remaining,
                        remaining_balance: pkg.remaining,
                        total: pkg.total,
                        original_capacity: Some(pkg.total),
                        unit: "credits".to_string(),
                        expire_at_ms,
                        expire_time,
                        expire_time_formatted,
                        is_inferred: false,
                        is_stale: false,
                        source: "official".to_string(),
                        source_type: "official".to_string(),
                        observed_at_ms: now,
                        observed_at: now / 1000,
                        status: "active".to_string(),
                    };
                    let _ = store::upsert_credit_package(conn, &pkg_item);
                }
            }
        }
        Scope::QoderCn | Scope::QoderGlobal => {
            let region = scope
                .qoder_region()
                .ok_or_else(|| "无法获取 Qoder 区域".to_string())?;
            let accs_file = crate::modules::qoder::account::load_accounts_for(region);
            for acc in &accs_file.accounts {
                let name = crate::modules::qoder::account::display_name(acc);
                let acc_ref = AccountRef {
                    scope: scope_str.to_string(),
                    account_id: acc.uid.clone(),
                    display_name: name,
                    is_active: true,
                    updated_at_ms: now,
                };
                let _ = store::upsert_account_ref(conn, &acc_ref);
            }
        }
    }

    Ok(())
}

/// 查询并投影积分工作台（FUS-R02 / R03 / R04）。
pub fn project_credit_summary(
    scope_str: &str,
    filter_account: Option<&str>,
    window_days: Option<i64>,
) -> Result<CreditSummaryProjection, String> {
    let scope = validate_scope(scope_str)?;
    let conn = store::open_db()?;
    let now = now_ms();

    // 若库内没有本 scope 的任何记录，尝试执行首次种子同步
    let existing_accounts = store::query_account_refs(&conn, scope.as_str())?;
    let existing_obs = store::query_latest_observations(&conn, scope.as_str(), None)?;
    if existing_accounts.is_empty() || existing_obs.is_empty() {
        let _ = sync_from_existing_stores(&conn, scope);
    }

    project_credit_summary_in_conn(&conn, scope_str, filter_account, window_days, now)
}

/// 在指定数据库连接上执行积分工作台投影（支持内存库隔离测试与确定性离线夹具生成）。
pub fn project_credit_summary_in_conn(
    conn: &rusqlite::Connection,
    scope_str: &str,
    filter_account: Option<&str>,
    window_days: Option<i64>,
    now: i64,
) -> Result<CreditSummaryProjection, String> {
    let scope = validate_scope(scope_str)?;
    let days = window_days.unwrap_or(30).max(1);
    let window_start_ms = now - days * 86_400_000;

    let accounts = store::query_account_refs(conn, scope.as_str())?;
    let account_name_map: HashMap<String, String> = accounts
        .iter()
        .map(|a| (a.account_id.clone(), a.display_name.clone()))
        .collect();

    let observations = store::query_latest_observations(conn, scope.as_str(), filter_account)?;
    let mut packages = store::query_credit_packages(conn, scope.as_str(), filter_account)?;

    // 数据库全量聚合（确保 151+ 条事件不被截断）
    let (period_in, period_out) = store::aggregate_credit_events_summary(
        conn,
        scope.as_str(),
        filter_account,
        Some(window_start_ms),
    )?;

    // 流水展示明细单独限制 100 条
    let events = store::query_credit_events(
        conn,
        scope.as_str(),
        filter_account,
        Some(window_start_ms),
        100,
    )?;

    // 补齐 package 中的 accountName
    for pkg in &mut packages {
        if pkg.account_name.is_empty() {
            if let Some(name) = account_name_map.get(&pkg.account_id) {
                pkg.account_name = name.clone();
            } else {
                pkg.account_name = pkg.account_id.clone();
            }
        }
    }

    // 统计可用余额与账号
    let mut current_remaining = 0.0;
    let mut total_original_capacity = 0.0;
    let mut total_accounts = 0;
    let mut active_accounts = 0;
    let mut latest_obs_time: Option<i64> = None;
    let mut has_stale = false;
    let mut sources = Vec::new();

    for obs in &observations {
        current_remaining += obs.remaining;
        total_original_capacity += obs.total;
        total_accounts += 1;
        if obs.remaining > 0.0 {
            active_accounts += 1;
        }
        if obs.is_stale || (now - obs.observed_at_ms > 24 * 3600 * 1000) {
            has_stale = true;
        }
        latest_obs_time = match latest_obs_time {
            Some(t) => Some(t.max(obs.observed_at_ms)),
            None => Some(obs.observed_at_ms),
        };
        if !sources.contains(&obs.source) {
            sources.push(obs.source.clone());
        }
    }

    // 统计即将过期积分（7 天以内且未过期）并构建到期日历
    let mut expiring_soon = 0.0;
    let mut expiring_within7_count = 0usize;
    let seven_days_ms = now + 7 * 86_400_000;

    let mut calendar_map: BTreeMap<String, Vec<ExpiryCalendarItem>> = BTreeMap::new();
    let mut date_timestamp_map: HashMap<String, i64> = HashMap::new();

    for pkg in &packages {
        if pkg.remaining <= 0.0 {
            continue; // 已用完的包不展示在到期日历中（PRD §5.2）
        }
        if let Some(exp_ms) = pkg.expire_at_ms {
            if exp_ms >= now && exp_ms <= seven_days_ms {
                expiring_soon += pkg.remaining;
                expiring_within7_count += 1;
            }
            let date_str = format_local_date(exp_ms);
            let days_until = days_until_ms(exp_ms, now);

            let cal_item = ExpiryCalendarItem {
                key: format!("{}-{}-{}", pkg.account_id, pkg.package_id, exp_ms),
                scope: pkg.scope.clone(),
                account_id: pkg.account_id.clone(),
                account_name: pkg.account_name.clone(),
                package_id: pkg.package_id.clone(),
                package_name: pkg.name.clone(),
                package_type: pkg.package_type.clone(),
                remaining: pkg.remaining,
                amount: pkg.remaining,
                total: pkg.total,
                unit: pkg.unit.clone(),
                expire_at_ms: exp_ms,
                expire_time: exp_ms / 1000,
                days_remaining: days_until,
            };
            calendar_map
                .entry(date_str.clone())
                .or_default()
                .push(cal_item);
            date_timestamp_map.entry(date_str).or_insert(exp_ms);
        }
    }

    let mut calendar = Vec::new();
    for (date_str, items) in calendar_map {
        let ts_ms = *date_timestamp_map.get(&date_str).unwrap_or(&now);
        let days_until = days_until_ms(ts_ms, now);
        let total_exp: f64 = items.iter().map(|it| it.remaining).sum();
        let count = items.len();
        calendar.push(ExpiryCalendarDateGroup {
            date: date_str,
            timestamp_ms: ts_ms,
            days_until,
            days_remaining: days_until,
            total_expiring: total_exp,
            count,
            package_count: count,
            items,
        });
    }

    // 按到期日期升序排列日历
    calendar.sort_by_key(|c| c.timestamp_ms);

    // 诚实状态派生：无观测数据时绝不伪装为 official 且零余额
    let is_known = !observations.is_empty();
    let (source_kind, code, message) = if observations.is_empty() {
        (
            "unknown".to_string(),
            "unobserved".to_string(),
            "尚未获取到积分观测数据".to_string(),
        )
    } else if sources.iter().all(|s| s == "official") {
        (
            "official".to_string(),
            if has_stale {
                "stale".to_string()
            } else {
                "live".to_string()
            },
            if has_stale {
                "官方积分快照已过期".to_string()
            } else {
                "官方积分实时同步".to_string()
            },
        )
    } else if sources.iter().all(|s| s == "snapshot") {
        (
            "snapshot".to_string(),
            "cached".to_string(),
            "从本地快照读取".to_string(),
        )
    } else {
        (
            "mixed".to_string(),
            "cached".to_string(),
            "多来源综合估算".to_string(),
        )
    };

    let summary = CreditSummaryStats {
        current_remaining,
        total_remaining_balance: current_remaining,
        total_original_capacity,
        period_in,
        period_out,
        expiring_soon,
        expiring_within7_days_count: expiring_within7_count,
        expiring_within7_days_amount: expiring_soon,
        currency: "credits".to_string(),
        total_accounts,
        active_accounts,
        total_packages: packages.len(),
        is_known,
    };

    let status = ManagementDataStatus {
        is_stale: has_stale,
        last_updated_ms: latest_obs_time,
        source_kind,
        code,
        message,
        observed_at: latest_obs_time.map(|t| t / 1000).unwrap_or(0),
        cached_at: if observations.is_empty() {
            None
        } else {
            Some(now / 1000)
        },
        ttl_remaining_seconds: if observations.is_empty() {
            None
        } else {
            Some(300)
        },
        error: None,
    };

    Ok(CreditSummaryProjection {
        scope: scope.as_str().to_string(),
        filter_account: filter_account.map(str::to_string),
        summary: summary.clone(),
        stats: summary,
        packages,
        calendar,
        observations,
        events: events.clone(),
        recent_events: events,
        status,
    })
}

/// 主动触发从各平台实时采集/刷新积分并更新数据库（FUS-W03 刷新闭环）。
pub async fn refresh_credits(
    scope_str: &str,
    _request_key: Option<&str>,
) -> Result<CreditSummaryProjection, String> {
    let scope = validate_scope(scope_str)?;
    let mut conn = store::open_db()?;
    let now = now_ms();
    let mut refresh_error: Option<String> = None;

    match scope {
        Scope::WbCn | Scope::WbGlobal => {
            let region = scope
                .wb_region()
                .ok_or_else(|| "无法获取 WB 区域".to_string())?;
            let accounts = crate::modules::account::load_accounts_for(region);

            for acc in &accounts {
                let aid = acc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let name = crate::modules::account::account_display_name(acc);
                let acc_ref = AccountRef {
                    scope: scope.as_str().to_string(),
                    account_id: aid.clone(),
                    display_name: name.clone(),
                    is_active: true,
                    updated_at_ms: now,
                };
                let _ = store::upsert_account_ref(&conn, &acc_ref);

                // 调用 WorkBuddy 已有的 credits 模块查询实时资源包与余额
                let expiry_res = crate::modules::credits::get_credit_expiry_for(region, acc).await;
                if expiry_res
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    if let Some(resources) = expiry_res.get("resources").and_then(|v| v.as_array())
                    {
                        let mut remaining_sum = 0.0;
                        let mut total_sum = 0.0;
                        let mut new_packages = Vec::new();

                        for (idx, r) in resources.iter().enumerate() {
                            let rem = r.get("remaining").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let tot = r.get("total").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            remaining_sum += rem;
                            total_sum += tot;

                            let pkg_name = r
                                .get("packageName")
                                .or_else(|| r.get("packageCode"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("官方资源包");

                            let exp_ms = r.get("expireAt").and_then(|v| v.as_i64()).map(|ts| {
                                if ts < 10_000_000_000 {
                                    ts * 1000
                                } else {
                                    ts
                                }
                            });

                            let pkg_id = r
                                .get("packageCode")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&format!("wb_res_{idx}"))
                                .to_string();

                            let expire_time = exp_ms.map(|ms| ms / 1000);
                            let expire_time_formatted = exp_ms.map(|ms| format_local_date(ms));

                            let pkg_item = CreditPackageItem {
                                id: None,
                                scope: scope.as_str().to_string(),
                                account_id: aid.clone(),
                                account_name: name.clone(),
                                package_id: pkg_id,
                                name: pkg_name.to_string(),
                                package_name: pkg_name.to_string(),
                                package_type: "addon".to_string(),
                                remaining: rem,
                                remaining_balance: rem,
                                total: tot,
                                original_capacity: Some(tot),
                                unit: "credits".to_string(),
                                expire_at_ms: exp_ms,
                                expire_time,
                                expire_time_formatted,
                                is_inferred: false,
                                is_stale: false,
                                source: "official".to_string(),
                                source_type: "official".to_string(),
                                observed_at_ms: now,
                                observed_at: now / 1000,
                                status: "active".to_string(),
                            };
                            new_packages.push(pkg_item);
                        }

                        // 成功全量采集：事务替换该账号的全部包，自动移除已用尽/消失的旧包
                        let _ = store::replace_account_credit_packages(
                            &mut conn,
                            scope.as_str(),
                            &aid,
                            &new_packages,
                        );

                        let obs = CreditObservation {
                            id: None,
                            scope: scope.as_str().to_string(),
                            account_id: aid.clone(),
                            source: "official".to_string(),
                            remaining: remaining_sum,
                            total: total_sum,
                            unit: "credits".to_string(),
                            is_stale: false,
                            is_comparable: true,
                            observed_at_ms: now,
                        };
                        let _ = store::insert_credit_observation(&conn, &obs);
                    }
                } else {
                    // 查询失败或受阻：标记原有积分包为 stale，绝对不删除旧数据
                    let _ = store::mark_account_packages_stale(&conn, scope.as_str(), &aid);
                    if let Some(err_msg) = expiry_res.get("error").and_then(|v| v.as_str()) {
                        refresh_error = Some(err_msg.to_string());
                    }
                }
            }
        }
        Scope::TraeCn | Scope::TraeGlobal => {
            let region = scope
                .trae_region()
                .ok_or_else(|| "无法获取 Trae 区域".to_string())?;
            let variant = trae_variant_for_region(region);
            let accs_file = crate::modules::trae::account::load_accounts_for_region(region);

            for acc in &accs_file.accounts {
                let uid = acc.user_id.clone().unwrap_or_else(|| acc.name.clone());
                let acc_ref = AccountRef {
                    scope: scope.as_str().to_string(),
                    account_id: uid,
                    display_name: acc.name.clone(),
                    is_active: true,
                    updated_at_ms: now,
                };
                let _ = store::upsert_account_ref(&conn, &acc_ref);
            }

            // 触发刷新 Trae 官方额度
            let refreshed_count =
                crate::modules::trae::credits::refresh_all_remaining_for(variant).await;
            if refreshed_count == 0 && !accs_file.accounts.is_empty() {
                refresh_error = Some("Trae 上游额度刷新未返回有效变更".to_string());
            }

            // 读回更新后的文件并入库
            let rem_file = crate::modules::trae::credits::load_remaining_for(variant);
            for (uid, &rem_credits) in &rem_file.credits {
                let packages = rem_file.packages.get(uid).cloned().unwrap_or_default();
                let total_credits = packages
                    .iter()
                    .map(|p| p.total)
                    .sum::<f64>()
                    .max(rem_credits);
                let obs = CreditObservation {
                    id: None,
                    scope: scope.as_str().to_string(),
                    account_id: uid.clone(),
                    source: "official".to_string(),
                    remaining: rem_credits,
                    total: total_credits,
                    unit: "credits".to_string(),
                    is_stale: false,
                    is_comparable: true,
                    observed_at_ms: now,
                };
                let _ = store::insert_credit_observation(&conn, &obs);

                let mut new_packages = Vec::new();
                for (idx, pkg) in packages.iter().enumerate() {
                    let pkg_name = pkg
                        .package_name
                        .clone()
                        .or_else(|| pkg.package_code.clone())
                        .unwrap_or_else(|| format!("积分包 #{}", idx + 1));
                    let expire_at_ms = pkg.expire_at.map(|sec| sec * 1000);
                    let pkg_id = pkg
                        .package_code
                        .clone()
                        .unwrap_or_else(|| format!("trae_pkg_{idx}"));

                    let expire_time = expire_at_ms.map(|ms| ms / 1000);
                    let expire_time_formatted = expire_at_ms.map(|ms| format_local_date(ms));

                    let pkg_item = CreditPackageItem {
                        id: None,
                        scope: scope.as_str().to_string(),
                        account_id: uid.clone(),
                        account_name: uid.clone(),
                        package_id: pkg_id,
                        name: pkg_name.clone(),
                        package_name: pkg_name,
                        package_type: "addon".to_string(),
                        remaining: pkg.remaining,
                        remaining_balance: pkg.remaining,
                        total: pkg.total,
                        original_capacity: Some(pkg.total),
                        unit: "credits".to_string(),
                        expire_at_ms,
                        expire_time,
                        expire_time_formatted,
                        is_inferred: false,
                        is_stale: false,
                        source: "official".to_string(),
                        source_type: "official".to_string(),
                        observed_at_ms: now,
                        observed_at: now / 1000,
                        status: "active".to_string(),
                    };
                    new_packages.push(pkg_item);
                }

                let _ = store::replace_account_credit_packages(
                    &mut conn,
                    scope.as_str(),
                    uid,
                    &new_packages,
                );
            }
        }
        Scope::QoderCn | Scope::QoderGlobal => {
            let region = scope
                .qoder_region()
                .ok_or_else(|| "无法获取 Qoder 区域".to_string())?;
            let accs_file = crate::modules::qoder::account::load_accounts_for(region);

            for acc in &accs_file.accounts {
                let name = crate::modules::qoder::account::display_name(acc);
                let acc_ref = AccountRef {
                    scope: scope.as_str().to_string(),
                    account_id: acc.uid.clone(),
                    display_name: name,
                    is_active: true,
                    updated_at_ms: now,
                };
                let _ = store::upsert_account_ref(&conn, &acc_ref);
            }

            // 触发 Qoder 官方额度与用量查询（qoder::credits::region_view）
            let qoder_view = crate::modules::qoder::credits::region_view(region).await;
            if let Some(accounts_arr) = qoder_view.get("accounts").and_then(|v| v.as_array()) {
                for item in accounts_arr {
                    let uid = item
                        .get("uid")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let ok = item.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);

                    if ok {
                        let remaining = item
                            .get("remaining")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let total = item
                            .get("total")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(remaining);

                        // 诚实记录官方观测，但由于 Qoder 上游无独立逐包清单，严禁伪造成假套餐包
                        let obs = CreditObservation {
                            id: None,
                            scope: scope.as_str().to_string(),
                            account_id: uid.to_string(),
                            source: "official".to_string(),
                            remaining,
                            total,
                            unit: "credits".to_string(),
                            is_stale: false,
                            is_comparable: true,
                            observed_at_ms: now,
                        };
                        let _ = store::insert_credit_observation(&conn, &obs);
                    } else if let Some(err) = item.get("error").and_then(|v| v.as_str()) {
                        refresh_error = Some(err.to_string());
                    }
                }
            }
        }
    }

    // 重新投影并带上可能的错误/缓存说明
    let mut projection = project_credit_summary(scope.as_str(), None, None)?;
    if let Some(err) = refresh_error {
        projection.status.is_stale = true;
        projection.status.error = Some(err);
    }
    Ok(projection)
}
