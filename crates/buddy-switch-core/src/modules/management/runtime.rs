//! 管理任务排程与中断恢复。依据 docs/specs/FUSION-INTEGRATION.md §7。
//! 排程时点去重不等于供应商活动周期幂等；真实平台动作仍由适配器门禁控制。

use std::sync::OnceLock;

use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{models::TaskSpec, store, tasks};

pub const POLL_INTERVAL_SECONDS: u64 = 30;

fn process_owner() -> &'static str {
    static OWNER: OnceLock<String> = OnceLock::new();
    OWNER.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

pub(super) fn register_run(conn: &Connection, run_id: &str, now: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO task_run_leases(run_id, owner_id, expires_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(run_id) DO UPDATE SET owner_id = excluded.owner_id, expires_at_ms = excluded.expires_at_ms",
        params![run_id, process_owner(), now + tasks::ACCOUNT_LOCK_TTL_MS],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn renew_run(conn: &Connection, run_id: &str, now: i64) -> Result<(), String> {
    let changed = conn
        .execute(
            "UPDATE task_run_leases SET expires_at_ms = ?1 WHERE run_id = ?2 AND owner_id = ?3",
            params![now + tasks::ACCOUNT_LOCK_TTL_MS, run_id, process_owner()],
        )
        .map_err(|e| e.to_string())?;
    if changed != 1 {
        return Err("运行租约已丢失，结果待确认".into());
    }
    Ok(())
}

pub(super) fn finish_run(conn: &Connection, run_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM task_run_leases WHERE run_id = ?1 AND owner_id = ?2",
        params![run_id, process_owner()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 当前支持每日分钟/小时集合（整数、逗号列表、*、*/N）；日期三字段必须是 *。
/// 不把未支持的 cron 或时区悄悄按本地时间执行。
#[derive(Debug)]
pub struct DailySchedule {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    offset: FixedOffset,
}

impl DailySchedule {
    pub fn parse(expression: &str, timezone: &str) -> Result<Self, String> {
        let fields: Vec<_> = expression.split_whitespace().collect();
        if fields.len() != 5 || fields[2..] != ["*", "*", "*"] {
            return Err("当前排程仅支持每日规则，cron 的日期、月份、星期字段须为 *".into());
        }
        let offset = match timezone {
            "UTC" | "Etc/UTC" => FixedOffset::east_opt(0).unwrap(),
            "Asia/Shanghai" => FixedOffset::east_opt(8 * 3600).unwrap(),
            _ => return Err(format!("尚不支持此排程时区：{timezone}")),
        };
        Ok(Self {
            minutes: parse_field(fields[0], 60)?,
            hours: parse_field(fields[1], 24)?,
            offset,
        })
    }

    fn candidates(&self, now: i64, day_offset: u64) -> Result<Vec<i64>, String> {
        let local = DateTime::<Utc>::from_timestamp_millis(now)
            .ok_or("排程时间戳无效")?
            .with_timezone(&self.offset);
        let date = local
            .date_naive()
            .checked_add_days(chrono::Days::new(day_offset))
            .ok_or("日期溢出")?;
        let mut values = Vec::new();
        for hour in &self.hours {
            for minute in &self.minutes {
                let naive = date.and_hms_opt(*hour, *minute, 0).ok_or("排程时点无效")?;
                values.push(
                    self.offset
                        .from_local_datetime(&naive)
                        .single()
                        .ok_or("排程时点不唯一")?
                        .timestamp_millis(),
                );
            }
        }
        values.sort_unstable();
        Ok(values)
    }

    /// 启动仅补本地当天最近的一个时点，不积压重放前几天的领取。
    pub fn latest_due(&self, now: i64) -> Result<Option<i64>, String> {
        Ok(self
            .candidates(now, 0)?
            .into_iter()
            .filter(|at| *at <= now)
            .max())
    }

    pub fn next_due(&self, now: i64) -> Result<Option<i64>, String> {
        for day in 0..=1 {
            if let Some(at) = self.candidates(now, day)?.into_iter().find(|at| *at > now) {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }
}

fn parse_field(field: &str, maximum: u32) -> Result<Vec<u32>, String> {
    let mut values = Vec::new();
    for part in field.split(',') {
        if part == "*" {
            values.extend(0..maximum);
        } else if let Some(step) = part.strip_prefix("*/") {
            let step: u32 = step.parse().map_err(|_| "无效的排程步长")?;
            if step == 0 || step > maximum {
                return Err("排程步长超出范围".into());
            }
            values.extend((0..maximum).step_by(step as usize));
        } else {
            let value: u32 = part.parse().map_err(|_| "排程时点须为整数、* 或 */N")?;
            if value >= maximum {
                return Err("排程时点超出范围".into());
            }
            values.push(value);
        }
    }
    values.sort_unstable();
    values.dedup();
    if values.is_empty() {
        return Err("排程时点为空".into());
    }
    Ok(values)
}

pub fn validate_spec(spec: &TaskSpec) -> Result<(), String> {
    match spec.schedule_cron.as_deref() {
        Some(expression) => {
            DailySchedule::parse(expression, &spec.timezone)?;
        }
        None if spec.enabled => return Err("启用自动任务需要排程表达式".into()),
        None => {}
    }
    Ok(())
}

pub(super) fn project_specs(
    conn: &Connection,
    specs: &mut [TaskSpec],
    now: i64,
) -> Result<(), String> {
    for spec in specs {
        spec.last_run_at = conn
            .query_row(
                "SELECT MAX(started_at_ms) FROM task_runs WHERE scope = ?1 AND task_id = ?2",
                params![spec.scope, spec.id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(|e| e.to_string())?
            .map(|ms| ms / 1000);
        if spec.enabled {
            match spec
                .schedule_cron
                .as_deref()
                .ok_or("缺少排程表达式".into())
                .and_then(|expression| DailySchedule::parse(expression, &spec.timezone))
            {
                Ok(schedule) => spec.next_run_at = schedule.next_due(now)?.map(|ms| ms / 1000),
                Err(error) => {
                    spec.status = "invalid_schedule".into();
                    if !spec.metadata.is_object() {
                        spec.metadata = serde_json::json!({});
                    }
                    spec.metadata["scheduleError"] = serde_json::json!(error);
                }
            }
        }
    }
    Ok(())
}

/// 仅恢复有过期租约且无有效账号锁的活跃 run；无租约的旧版运行保留供手动核实。
/// 已发写请求只变为待确认；未开始项/只读中断项标失败，可由明确重试入口继续。
pub fn recover_abandoned_runs(conn: &mut Connection, now: i64) -> Result<Vec<String>, String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT r.scope, r.run_id FROM task_runs r JOIN task_run_leases l ON l.run_id = r.run_id
             WHERE r.status IN ('running','queued','waiting_upstream') AND l.expires_at_ms <= ?1
             AND NOT EXISTS(SELECT 1 FROM task_account_locks a WHERE a.owner_id = r.run_id AND a.expires_at_ms > ?1)"
        ).map_err(|e| e.to_string())?;
        let values = stmt
            .query_map([now], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?;
        values
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
    };
    let mut recovered = Vec::new();
    for (scope, run_id) in rows {
        let mut run = store::query_task_runs(&tx, &scope, Some(&run_id))?.remove(0);
        let mut items = store::query_task_items(&tx, &run_id)?;
        for item in &mut items {
            match item.status.as_str() {
                "pending" | "queued" => {
                    item.status = "failed".into();
                    item.stage = "interrupted_before_start".into();
                    item.error = Some("进程中断，尚未发送请求，可手动重试".into());
                }
                "running" | "waiting_upstream" if run.task_kind == "credits_sync" => {
                    item.status = "failed".into();
                    item.stage = "interrupted_read".into();
                    item.error = Some("只读同步中断，可手动重试".into());
                }
                "running" | "waiting_upstream" => {
                    item.status = "pending_confirmation".into();
                    item.stage = "waiting_verification".into();
                    item.error =
                        Some("进程中断，上游写请求结果未知，等待只读复查，禁止自动重放".into());
                }
                _ => continue,
            }
            item.finished_at_ms = Some(now);
            store::upsert_task_item(&tx, item)?;
            let (err_cat, msg) = if item.status == "pending_confirmation" {
                ("waiting_verification", "进程中断，写操作转待确认")
            } else if item.stage == "interrupted_read" {
                ("interrupted", "只读同步中断，可手动重试")
            } else {
                ("interrupted", "进程中断，尚未发送请求，可手动重试")
            };
            store::insert_task_event(
                &tx,
                &run_id,
                &scope,
                &item.account_id,
                &item.stage,
                &item.status,
                now,
                Some(err_cat),
                Some(msg),
            )?;
        }
        run.success_items = items
            .iter()
            .filter(|i| matches!(i.status.as_str(), "confirmed" | "already_claimed"))
            .count();
        run.failed_items = items.iter().filter(|i| i.status == "failed").count();
        run.status = if items.iter().any(|i| i.status == "pending_confirmation") {
            "waiting_verification"
        } else if run.failed_items > 0 || items.is_empty() {
            "failed"
        } else if items.iter().any(|i| i.status == "cancelled") {
            "cancelled"
        } else {
            "completed"
        }
        .into();
        run.finished_at_ms = Some(now);
        run.finished_at = Some(now / 1000);
        run.message = Some("已恢复中断记录；结果未知的写操作保留待确认，未自动重放".into());
        store::upsert_task_run(&tx, &run)?;
        let run_err_cat = if run.status == "waiting_verification" {
            "waiting_verification"
        } else if run.status == "failed" {
            "interrupted"
        } else {
            "recovery"
        };
        store::insert_task_event(
            &tx,
            &run_id,
            &scope,
            "",
            "recovery",
            &run.status,
            now,
            Some(run_err_cat),
            run.message.as_deref(),
        )?;
        store::release_owner_locks(&tx, &run_id)?;
        tx.execute("DELETE FROM task_run_leases WHERE run_id = ?1", [&run_id])
            .map_err(|e| e.to_string())?;
        recovered.push(run_id);
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(recovered)
}

pub(super) fn scheduled_run(
    conn: &Connection,
    scope: &str,
    task_id: &str,
    due: i64,
) -> Result<Option<String>, String> {
    conn.query_row("SELECT run_id FROM task_schedule_slots WHERE scope = ?1 AND task_id = ?2 AND due_at_ms = ?3",
        params![scope, task_id, due], |r| r.get(0)).optional().map_err(|e| e.to_string())
}

pub(super) fn record_slot(
    conn: &Connection,
    scope: &str,
    task_id: &str,
    due: i64,
    run_id: &str,
) -> Result<(), String> {
    conn.execute("INSERT INTO task_schedule_slots(scope, task_id, due_at_ms, run_id) VALUES (?1, ?2, ?3, ?4)",
        params![scope, task_id, due, run_id]).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct TickReport {
    pub recovered: Vec<String>,
    pub dispatched: Vec<String>,
    pub errors: Vec<String>,
}

pub async fn tick(now: i64) -> Result<TickReport, String> {
    let (mut report, specs) = {
        let mut conn = store::open_db()?;
        let recovered = recover_abandoned_runs(&mut conn, now)?;
        let scopes: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT scope FROM task_specs")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        let mut specs = Vec::new();
        for scope in scopes {
            specs.extend(store::query_task_specs(&conn, &scope)?);
        }
        (
            TickReport {
                recovered,
                ..Default::default()
            },
            specs,
        )
    };
    for spec in specs.into_iter().filter(|s| s.enabled) {
        let due = match spec
            .schedule_cron
            .as_deref()
            .ok_or("缺少排程表达式".to_string())
            .and_then(|expression| DailySchedule::parse(expression, &spec.timezone))
            .and_then(|schedule| schedule.latest_due(now))
        {
            Ok(Some(due)) => due,
            Ok(None) => continue,
            Err(error) => {
                report.errors.push(format!("{}: {error}", spec.id));
                continue;
            }
        };
        if scheduled_run(&store::open_db()?, &spec.scope, &spec.id, due)?.is_some() {
            continue;
        }
        match crate::modules::scheduler::run_scheduled_management_task(&spec.scope, &spec.id, due)
            .await
        {
            Ok(run) => report.dispatched.push(run.run_id),
            Err(error) => report.errors.push(format!("{}: {error}", spec.id)),
        }
    }
    Ok(report)
}

/// 由两宿主共同的 background 注册表启动；不另建系统计划任务或外部常驻服务。
pub async fn management_loop() {
    loop {
        match tick(crate::modules::config::now_ms()).await {
            Ok(report) if !report.errors.is_empty() => {
                eprintln!("[管理任务] {}", report.errors.join("; "))
            }
            Err(error) => eprintln!("[管理任务] {error}"),
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECONDS)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{AccountRef, TaskItem, TaskRun};
    use super::*;

    fn at(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    fn run_fixture(id: &str, kind: &str) -> TaskRun {
        TaskRun {
            run_id: id.into(),
            task_id: id.into(),
            scope: "wb-cn".into(),
            task_kind: kind.into(),
            trigger_source: "manual".into(),
            triggered_by: "manual".into(),
            status: "running".into(),
            started_at_ms: 1,
            started_at: 0,
            finished_at_ms: None,
            finished_at: None,
            total_items: 0,
            success_items: 0,
            failed_items: 0,
            message: None,
            details_json: None,
            error: None,
            cursor: Some(0),
        }
    }

    fn item_fixture(run_id: &str, account_id: &str, status: &str) -> TaskItem {
        TaskItem {
            id: None,
            run_id: run_id.into(),
            scope: "wb-cn".into(),
            account_id: account_id.into(),
            stage: "fixture".into(),
            status: status.into(),
            started_at_ms: 1,
            finished_at_ms: None,
            result_data: None,
            error: None,
            retry_count: 0,
        }
    }

    #[test]
    fn daily_schedule_uses_saved_timezone_and_does_not_replay_yesterday() {
        let now = at("2026-10-23T02:30:00Z");
        let shanghai = DailySchedule::parse("0 9 * * *", "Asia/Shanghai").unwrap();
        assert_eq!(
            shanghai.latest_due(now).unwrap(),
            Some(at("2026-10-23T01:00:00Z"))
        );
        assert_eq!(
            shanghai.next_due(now).unwrap(),
            Some(at("2026-10-24T01:00:00Z"))
        );
        let utc = DailySchedule::parse("0 9 * * *", "UTC").unwrap();
        assert_eq!(utc.latest_due(now).unwrap(), None);
        assert_eq!(utc.next_due(now).unwrap(), Some(at("2026-10-23T09:00:00Z")));
        assert_eq!(
            shanghai.latest_due(at("2026-10-23T16:01:00Z")).unwrap(),
            None
        );
    }

    #[test]
    fn schedule_intervals_lists_boundaries_and_invalid_values() {
        let schedule = DailySchedule::parse("0 */4 * * *", "UTC").unwrap();
        let now = at("2026-10-23T04:00:00Z");
        assert_eq!(schedule.latest_due(now).unwrap(), Some(now));
        assert_eq!(
            schedule.next_due(now).unwrap(),
            Some(at("2026-10-23T08:00:00Z"))
        );
        let schedule = DailySchedule::parse("15,45 8,12 * * *", "UTC").unwrap();
        assert_eq!(
            schedule.next_due(at("2026-10-23T08:16:00Z")).unwrap(),
            Some(at("2026-10-23T08:45:00Z"))
        );
        for expression in [
            "0 25 * * *",
            "60 0 * * *",
            "*/0 0 * * *",
            "0 0 * * 1",
            "0 0 1 * *",
            "bad",
            "0 0-5 * * *",
        ] {
            assert!(
                DailySchedule::parse(expression, "UTC").is_err(),
                "{expression}"
            );
        }
        assert!(DailySchedule::parse("0 9 * * *", "America/New_York").is_err());
        assert!(schedule.latest_due(i64::MAX).is_err());
    }

    #[test]
    fn management_database_refuses_missing_home_override_instead_of_falling_back() {
        let missing =
            std::env::temp_dir().join(format!("missing-management-{}", uuid::Uuid::new_v4()));
        assert!(!missing.exists());
        let _guard = crate::modules::config::HomeOverrideGuard::set(&missing);
        assert!(store::open_db().is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn recovery_preserves_unknown_writes_and_confirmed_items_and_is_idempotent() {
        let mut conn = store::open_memory_db().unwrap();
        let mut run = run_fixture("abandoned-write", "reward_claim");
        run.total_items = 3;
        store::upsert_task_run(&conn, &run).unwrap();
        for (id, status) in [
            ("done", "confirmed"),
            ("inflight", "running"),
            ("not-started", "queued"),
        ] {
            store::upsert_task_item(&conn, &item_fixture(&run.run_id, id, status)).unwrap();
        }
        register_run(&conn, &run.run_id, 0).unwrap();
        assert_eq!(
            recover_abandoned_runs(&mut conn, 120_000).unwrap(),
            vec![run.run_id.clone()]
        );
        let run = store::query_task_runs(&conn, "wb-cn", Some(&run.run_id))
            .unwrap()
            .remove(0);
        assert_eq!(run.status, "waiting_verification");
        assert_eq!(run.success_items, 1);
        assert_eq!(run.failed_items, 1);
        let items = store::query_task_items(&conn, &run.run_id).unwrap();
        assert_eq!(
            items
                .iter()
                .find(|i| i.account_id == "done")
                .unwrap()
                .status,
            "confirmed"
        );
        assert_eq!(
            items
                .iter()
                .find(|i| i.account_id == "inflight")
                .unwrap()
                .status,
            "pending_confirmation"
        );
        assert_eq!(
            items
                .iter()
                .find(|i| i.account_id == "not-started")
                .unwrap()
                .stage,
            "interrupted_before_start"
        );
        assert!(recover_abandoned_runs(&mut conn, 240_000)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn recovery_skips_live_leases_live_account_locks_and_legacy_runs() {
        let mut conn = store::open_memory_db().unwrap();
        for id in ["live-lease", "live-lock", "legacy"] {
            store::upsert_task_run(&conn, &run_fixture(id, "reward_claim")).unwrap();
            store::upsert_task_item(&conn, &item_fixture(id, "a", "running")).unwrap();
        }
        register_run(&conn, "live-lease", 100_000).unwrap();
        register_run(&conn, "live-lock", 0).unwrap();
        store::acquire_account_lock(
            &conn,
            "wb-cn",
            "a",
            "live-lock",
            "live-lock",
            100_000,
            100_000,
        )
        .unwrap();
        assert!(recover_abandoned_runs(&mut conn, 120_000)
            .unwrap()
            .is_empty());
        let recovered = recover_abandoned_runs(&mut conn, 300_000).unwrap();
        assert_eq!(recovered.len(), 2);
        assert!(!recovered.contains(&"legacy".to_string()));
    }

    #[test]
    fn read_only_recovery_survives_database_reopen_and_does_not_confirm() {
        let path = crate::modules::config::store_dir()
            .join(format!("recovery-{}.sqlite", uuid::Uuid::new_v4()));
        {
            let conn = store::open_db_at(&path).unwrap();
            store::upsert_task_run(&conn, &run_fixture("read", "credits_sync")).unwrap();
            store::upsert_task_item(&conn, &item_fixture("read", "a", "running")).unwrap();
            register_run(&conn, "read", 0).unwrap();
        }
        let mut conn = store::open_db_at(&path).unwrap();
        recover_abandoned_runs(&mut conn, 120_000).unwrap();
        let run = store::query_task_runs(&conn, "wb-cn", Some("read"))
            .unwrap()
            .remove(0);
        assert_eq!(run.status, "failed");
        assert_eq!(run.success_items, 0);
        assert_eq!(
            store::query_task_items(&conn, "read").unwrap()[0].stage,
            "interrupted_read"
        );
        drop(conn);
        let conn = store::open_db_at(&path).unwrap();
        assert_eq!(
            store::query_task_runs(&conn, "wb-cn", Some("read")).unwrap()[0],
            run
        );
    }

    #[tokio::test]
    async fn scheduled_slot_is_not_replayed_after_completion() {
        let task = format!("slot-{}", uuid::Uuid::new_v4());
        let first = tasks::trigger_task_at_with_handler(
            "wb-cn",
            &task,
            Some("scheduler"),
            Some(vec!["a".into()]),
            &tasks::OfflineTaskActionHandler,
            Some(1234),
        )
        .await
        .unwrap();
        let second = tasks::trigger_task_at_with_handler(
            "wb-cn",
            &task,
            Some("scheduler"),
            Some(vec!["a".into()]),
            &tasks::OfflineTaskActionHandler,
            Some(1234),
        )
        .await
        .unwrap();
        assert_eq!(first.run_id, second.run_id);
        let third = tasks::trigger_task_at_with_handler(
            "wb-cn",
            &task,
            Some("scheduler"),
            Some(vec!["a".into()]),
            &tasks::OfflineTaskActionHandler,
            Some(5678),
        )
        .await
        .unwrap();
        assert_ne!(first.run_id, third.run_id);
        assert_eq!(second.success_items, 0);
    }

    #[tokio::test]
    async fn tick_dispatches_saved_fixed_scope_once_and_respects_disabled_plan() {
        let id = format!("plan-{}", uuid::Uuid::new_v4());
        let mut spec = tasks::get_task_specs("trae-global").unwrap().remove(0);
        spec.id = id.clone();
        spec.task_id = id.clone();
        spec.enabled = true;
        spec.schedule_cron = Some("0 9 * * *".into());
        spec.timezone = "Asia/Shanghai".into();
        spec.account_scope = "fixed".into();
        spec.metadata["accountIds"] = serde_json::json!([id]);
        let conn = store::open_db().unwrap();
        store::upsert_account_ref(
            &conn,
            &AccountRef {
                scope: "trae-global".into(),
                account_id: id.clone(),
                display_name: "fixture".into(),
                is_active: true,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        tasks::save_task_spec("trae-global", &mut spec).unwrap();
        let now = at("2026-10-23T02:00:00Z");
        let first = tick(now).await.unwrap();
        assert_eq!(first.dispatched.len(), 1, "{:?}", first.errors);
        assert!(tick(now + 30_000).await.unwrap().dispatched.is_empty());
        let items = tasks::get_task_items("trae-global", &first.dispatched[0]).unwrap();
        assert_eq!(items[0].account_id, id);
        assert_eq!(items[0].status, "unsupported");
        let projected = tasks::get_task_specs("trae-global")
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap();
        assert!(projected.last_run_at.is_some());
        assert!(projected.next_run_at.is_some());
        spec.schedule_cron = Some("0 11 * * *".into());
        tasks::save_task_spec("trae-global", &mut spec).unwrap();
        assert!(
            tasks::trigger_scheduled_task("trae-global", &id, at("2026-10-23T01:00:00Z"))
                .await
                .is_err()
        );
        spec.enabled = false;
        tasks::save_task_spec("trae-global", &mut spec).unwrap();
        assert!(tick(now + 86_400_000).await.unwrap().dispatched.is_empty());
        assert!(tasks::trigger_scheduled_task("trae-global", &id, now)
            .await
            .is_err());
    }
}
