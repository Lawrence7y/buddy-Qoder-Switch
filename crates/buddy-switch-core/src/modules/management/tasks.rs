//! 任务中心执行器与调度整合（FUS-W06 / PRD §5.3、§7、§10）。
//!
//! 统一任务运行记录（run_id、逐账号 item 状态、事件游标）、跨进程账号锁、
//! 停止取消语义、失败重试过滤与恢复机制。
//! 遵循 PRD §7：
//! - 统一手动操作与定时执行路径；重入或重复触发返回已有 run，不产生重复 run；
//! - 跨进程账号锁（owner/lease/heartbeat）阻止同账号重复并发；
//! - 取消语义：未开始账号设为 cancelled，已发到上游写请求设为 pending_confirmation，后续仅复查；
//! - 批量失败重试自动排除待确认项、已到账项、此前已领项与不适用项；
//! - 遵循 W05 授权边界：离线先行，上游请求通过可注入的 TaskActionHandler trait 隔离，不发真实网络请求。

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::runtime;
use crate::modules::config::now_ms;
use crate::modules::management::credits::validate_scope;
use crate::modules::management::models::{TaskEventPage, TaskItem, TaskRun, TaskSpec};
use crate::modules::management::store;

/// 账号锁租约超时时间（毫秒）：1 分钟
pub const ACCOUNT_LOCK_TTL_MS: i64 = 60_000;

/// 逐账号执行项结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskItemExecutionResult {
    /// confirmed, already_claimed, failed, unsupported, skipped, pending_confirmation
    pub status: String,
    pub stage: String,
    pub message: Option<String>,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// 上游任务动作执行器抽象 trait（支持注入 Mock 用于离线测试与状态机验证）。
pub trait TaskActionHandler: Send + Sync {
    fn execute_item(
        &self,
        scope: &str,
        task_id: &str,
        task_kind: &str,
        account_id: &str,
    ) -> impl std::future::Future<Output = Result<TaskItemExecutionResult, String>> + Send;
}

/// 默认离线任务动作执行器（FUS-W06 离线安全先行：受 W05 门禁约束，未发真实上游网络请求）。
#[derive(Debug, Clone, Default)]
pub struct OfflineTaskActionHandler;

impl TaskActionHandler for OfflineTaskActionHandler {
    async fn execute_item(
        &self,
        scope: &str,
        task_id: &str,
        task_kind: &str,
        account_id: &str,
    ) -> Result<TaskItemExecutionResult, String> {
        Ok(TaskItemExecutionResult {
            status: "unsupported".to_string(),
            stage: "adapter_unavailable".to_string(),
            message: Some(format!(
                "任务 {task_id} 针对账号 {account_id} 在 {scope} 尚未接入真实适配器，未执行、未领取"
            )),
            data: Some(serde_json::json!({
                "offline": true,
                "scope": scope,
                "taskId": task_id,
                "taskKind": task_kind,
                "accountId": account_id,
                "stage": "adapter_unavailable"
            })),
            error: None,
        })
    }
}

async fn execute_with_lease<H: TaskActionHandler>(
    run: &TaskRun,
    account_id: &str,
    handler: &H,
) -> Result<TaskItemExecutionResult, String> {
    let action = handler.execute_item(&run.scope, &run.task_id, &run.task_kind, account_id);
    tokio::pin!(action);
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            result = &mut action => return result,
            _ = heartbeat.tick() => {
                let conn = store::open_db()?;
                runtime::renew_run(&conn, &run.run_id, now_ms())?;
                // 续期不允许重新抢占已被其他执行者接管的锁。
                let renewed = conn.execute(
                    "UPDATE task_account_locks SET expires_at_ms = ?1 WHERE scope = ?2 AND account_id = ?3 AND task_id = ?4 AND owner_id = ?5",
                    rusqlite::params![now_ms() + ACCOUNT_LOCK_TTL_MS, run.scope, account_id, run.task_id, run.run_id],
                ).map_err(|e| e.to_string())?;
                if renewed != 1 {
                    return Err("执行中账号锁丢失，结果待确认，禁止自动重放".to_string());
                }
            }
        }
    }
}

/// 白名单规范 stage，防止不可信 handler 原始泄漏到事件表与状态机。
fn sanitize_task_stage(raw: &str) -> &'static str {
    match raw {
        "prepare" => "prepare",
        "upstream_write" => "upstream_write",
        "waiting_verification" => "waiting_verification",
        "done" => "done",
        "lock_failed" => "lock_failed",
        "interrupted_before_start" => "interrupted_before_start",
        "interrupted_read" => "interrupted_read",
        "finish" => "finish",
        "cancel" => "cancel",
        "retry_start" => "retry_start",
        "recovery" => "recovery",
        _ => "upstream_write",
    }
}

/// 白名单规范 status，防止不可信 handler 原始泄漏到事件表与状态机。
fn sanitize_task_status(raw: &str) -> &'static str {
    match raw {
        "confirmed" => "confirmed",
        "already_claimed" => "already_claimed",
        "pending" => "pending",
        "running" => "running",
        "waiting_upstream" => "waiting_upstream",
        "waiting_verification" => "waiting_verification",
        "pending_confirmation" => "pending_confirmation",
        "failed" => "failed",
        "unsupported" => "unsupported",
        "skipped" => "skipped",
        "cancelled" => "cancelled",
        _ => "failed",
    }
}

/// 解析作用域下的目标执行账号集合。
fn resolve_target_accounts(
    conn: &rusqlite::Connection,
    scope_str: &str,
    account_ids: Option<Vec<String>>,
) -> Result<Vec<String>, String> {
    if let Some(ids) = account_ids {
        let mut unique = std::collections::HashSet::new();
        return Ok(ids
            .into_iter()
            .filter(|id| !id.trim().is_empty() && unique.insert(id.clone()))
            .collect());
    }

    // 优先读取已缓存的账号引用快照
    let refs = store::query_account_refs(conn, scope_str)?;
    if !refs.is_empty() {
        return Ok(refs.into_iter().map(|r| r.account_id).collect());
    }

    // 若持久化库中尚无快照，尝试从各平台的原生账号存储读取
    let scope = validate_scope(scope_str)?;
    let list: Vec<String> = match scope {
        crate::modules::product::Scope::WbCn | crate::modules::product::Scope::WbGlobal => {
            if let Some(reg) = scope.wb_region() {
                crate::modules::account::load_accounts_for(reg)
                    .iter()
                    .filter_map(|acc| acc.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            } else {
                vec![]
            }
        }
        crate::modules::product::Scope::QoderCn | crate::modules::product::Scope::QoderGlobal => {
            if let Some(reg) = scope.qoder_region() {
                crate::modules::qoder::account::load_accounts_for(reg)
                    .accounts
                    .iter()
                    .map(|acc| acc.uid.clone())
                    .collect()
            } else {
                vec![]
            }
        }
        crate::modules::product::Scope::TraeCn | crate::modules::product::Scope::TraeGlobal => {
            crate::modules::trae::account::entries_for_region(
                scope.trae_region().ok_or("无效 Trae 区域")?,
            )
            .into_iter()
            .map(|(uid, _)| uid)
            .collect()
        }
    };

    Ok(list)
}

/// 获取指定作用域的任务规范（若尚未初始化则写入该作用域的预设契约）。
pub fn get_task_specs(scope_str: &str) -> Result<Vec<TaskSpec>, String> {
    let scope = validate_scope(scope_str)?;
    let conn = store::open_db()?;
    let specs = store::query_task_specs(&conn, scope.as_str())?;

    if !specs.is_empty() {
        let mut specs = specs;
        runtime::project_specs(&conn, &mut specs, now_ms())?;
        return Ok(specs);
    }

    // 写入预设规范（保留 W02 契约元数据以保证测试兼容性）
    let now = now_ms();
    let meta = json!({
        "stage": "W02",
        "w06_delivered": false,
        "w07_delivered": false,
        "note": "已接入应用内排程与中断记录恢复；真实平台动作与只读到账复查尚未接入。",
        "supported_lifecycle": ["queued", "running", "waiting_upstream", "waiting_verification", "completed", "failed", "cancelled"],
        "reward_receipt_outcomes": ["confirmed", "already_claimed", "pending", "failed", "unsupported", "skipped"]
    });

    let defaults = match scope.as_str() {
        "wb-cn" => vec![
            TaskSpec {
                id: "wb-cn-daily-checkin".to_string(),
                task_id: "wb-cn-daily-checkin".to_string(),
                scope: "wb-cn".to_string(),
                name: "每日自动签到".to_string(),
                description: "定时检查并执行各账号自动签到与领奖".to_string(),
                task_kind: "checkin".to_string(),
                trigger_type: "cron".to_string(),
                account_scope: "all".to_string(),
                schedule_cron: Some("0 9 * * *".to_string()),
                default_schedule: "0 9 * * *".to_string(),
                timezone: "Asia/Shanghai".to_string(),
                enabled: false,
                status: "idle".to_string(),
                last_run_at: None,
                next_run_at: None,
                retry_policy: Some("linear_backoff_3".to_string()),
                metadata: meta.clone(),
                updated_at_ms: now,
            },
            TaskSpec {
                id: "wb-cn-growth-travel".to_string(),
                task_id: "wb-cn-growth-travel".to_string(),
                scope: "wb-cn".to_string(),
                name: "成长任务与奖励观察".to_string(),
                description: "定时检查成长任务进度与里程碑奖励".to_string(),
                task_kind: "reward_claim".to_string(),
                trigger_type: "cron".to_string(),
                account_scope: "all".to_string(),
                schedule_cron: Some("0 10 * * *".to_string()),
                default_schedule: "0 10 * * *".to_string(),
                timezone: "Asia/Shanghai".to_string(),
                enabled: false,
                status: "idle".to_string(),
                last_run_at: None,
                next_run_at: None,
                retry_policy: Some("linear_backoff_3".to_string()),
                metadata: meta.clone(),
                updated_at_ms: now,
            },
        ],
        "qoder-cn" => vec![TaskSpec {
            id: "qoder-cn-login-reward".to_string(),
            task_id: "qoder-cn-login-reward".to_string(),
            scope: "qoder-cn".to_string(),
            name: "Qoder 每日登录奖励".to_string(),
            description: "定时领取 Qoder 开发者社区签到额度".to_string(),
            task_kind: "reward_claim".to_string(),
            trigger_type: "cron".to_string(),
            account_scope: "all".to_string(),
            schedule_cron: Some("0 9 * * *".to_string()),
            default_schedule: "0 9 * * *".to_string(),
            timezone: "Asia/Shanghai".to_string(),
            enabled: false,
            status: "idle".to_string(),
            last_run_at: None,
            next_run_at: None,
            retry_policy: Some("no_retry_on_409".to_string()),
            metadata: meta.clone(),
            updated_at_ms: now,
        }],
        "trae-cn" => vec![TaskSpec {
            id: "trae-cn-daily-checkin".to_string(),
            task_id: "trae-cn-daily-checkin".to_string(),
            scope: "trae-cn".to_string(),
            name: "Trae 每日签到".to_string(),
            description: "定时执行 Trae 开发者账号自动签到".to_string(),
            task_kind: "checkin".to_string(),
            trigger_type: "cron".to_string(),
            account_scope: "all".to_string(),
            schedule_cron: Some("0 9 * * *".to_string()),
            default_schedule: "0 9 * * *".to_string(),
            timezone: "Asia/Shanghai".to_string(),
            enabled: false,
            status: "idle".to_string(),
            last_run_at: None,
            next_run_at: None,
            retry_policy: Some("exponential_backoff_3".to_string()),
            metadata: meta.clone(),
            updated_at_ms: now,
        }],
        _ => vec![TaskSpec {
            id: format!("{}-credits-sync", scope.as_str()),
            task_id: format!("{}-credits-sync", scope.as_str()),
            scope: scope.as_str().to_string(),
            name: "额度数据定期同步".to_string(),
            description: "定期从各平台上游同步最新额度与快照".to_string(),
            task_kind: "credits_sync".to_string(),
            trigger_type: "cron".to_string(),
            account_scope: "all".to_string(),
            schedule_cron: Some("0 */4 * * *".to_string()),
            default_schedule: "0 */4 * * *".to_string(),
            timezone: "Asia/Shanghai".to_string(),
            enabled: false,
            status: "idle".to_string(),
            last_run_at: None,
            next_run_at: None,
            retry_policy: Some("linear_backoff_2".to_string()),
            metadata: meta.clone(),
            updated_at_ms: now,
        }],
    };

    for spec in &defaults {
        let _ = store::upsert_task_spec(&conn, spec);
    }

    let mut specs = store::query_task_specs(&conn, scope.as_str())?;
    runtime::project_specs(&conn, &mut specs, now_ms())?;
    Ok(specs)
}

/// 保存任务规范（PRD §5.4：范围变更后明确固定账号集合或动态读取，保存时展示实际账号数量）。
pub fn save_task_spec(scope_str: &str, spec: &mut TaskSpec) -> Result<TaskSpec, String> {
    let scope = validate_scope(scope_str)?;
    let conn = store::open_db()?;
    let now = now_ms();

    if spec.scope != scope.as_str() || spec.id != spec.task_id {
        return Err("任务标识或作用域不匹配".to_string());
    }
    runtime::validate_spec(spec)?;
    if !matches!(spec.account_scope.as_str(), "fixed" | "all") || !spec.metadata.is_object() {
        return Err("账号范围必须为 fixed/all，metadata 必须为对象".to_string());
    }
    let existing: Option<String> = conn
        .query_row(
            "SELECT scope FROM task_specs WHERE id = ?1",
            [&spec.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if existing.as_deref().is_some_and(|s| s != scope.as_str()) {
        return Err("任务 ID 已属于其他作用域".to_string());
    }

    let accounts = resolve_target_accounts(&conn, scope.as_str(), None)?;
    let count = if spec.account_scope == "fixed" {
        let ids: Vec<String> = serde_json::from_value(
            spec.metadata
                .get("accountIds")
                .cloned()
                .ok_or("固定范围缺少 accountIds")?,
        )
        .map_err(|_| "accountIds 必须为字符串数组")?;
        let ids = resolve_target_accounts(&conn, scope.as_str(), Some(ids))?;
        if ids.is_empty() || ids.iter().any(|id| !accounts.contains(id)) {
            return Err("固定账号集合为空或包含其他作用域/未知账号".to_string());
        }
        let count = ids.len();
        spec.metadata["accountIds"] = json!(ids);
        count
    } else {
        accounts.len()
    };

    if let serde_json::Value::Object(ref mut map) = spec.metadata {
        map.insert("accountCount".to_string(), json!(count));
        map.insert("isFixed".to_string(), json!(spec.account_scope == "fixed"));
    }

    spec.updated_at_ms = now;
    store::upsert_task_spec(&conn, spec)?;
    Ok(spec.clone())
}

/// 查询任务运行记录。
pub fn get_task_runs(scope_str: &str, run_id: Option<&str>) -> Result<Vec<TaskRun>, String> {
    let scope = validate_scope(scope_str)?;
    let conn = store::open_db()?;
    store::query_task_runs(&conn, scope.as_str(), run_id)
}

/// 查询指定运行的逐账号执行项。
pub fn get_task_items(scope_str: &str, run_id: &str) -> Result<Vec<TaskItem>, String> {
    let scope = validate_scope(scope_str)?;
    let conn = store::open_db()?;
    if store::query_task_runs(&conn, scope.as_str(), Some(run_id))?.is_empty() {
        return Err("当前作用域中未找到指定运行".to_string());
    }
    store::query_task_items(&conn, run_id)
}

/// 查询任务生命周期持久化事件（FUS-W06：单调游标增量分页，严格隔离未知与跨 scope 运行）。
pub fn get_task_events(
    scope_str: &str,
    run_id: Option<&str>,
    after_cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<TaskEventPage, String> {
    let scope = validate_scope(scope_str)?;
    let conn = store::open_db()?;
    store::query_task_events(&conn, scope.as_str(), run_id, after_cursor, limit)
}

/// 触发任务执行（生产入口，使用默认离线安全执行器）。
pub async fn trigger_task(
    scope_str: &str,
    task_id: &str,
    trigger_type: Option<&str>,
    account_ids: Option<Vec<String>>,
) -> Result<TaskRun, String> {
    trigger_task_at(scope_str, task_id, trigger_type, account_ids, None).await
}

pub(crate) async fn trigger_scheduled_task(
    scope_str: &str,
    task_id: &str,
    due: i64,
) -> Result<TaskRun, String> {
    trigger_task_at(scope_str, task_id, Some("scheduler"), None, Some(due)).await
}

async fn trigger_task_at(
    scope_str: &str,
    task_id: &str,
    trigger_type: Option<&str>,
    account_ids: Option<Vec<String>>,
    due: Option<i64>,
) -> Result<TaskRun, String> {
    let spec = get_task_specs(scope_str)?
        .into_iter()
        .find(|s| s.id == task_id)
        .ok_or("当前作用域中未登记此任务")?;
    if trigger_type == Some("scheduler") && !spec.enabled {
        return Err("此任务计划已停用".to_string());
    }
    let conn = store::open_db()?;
    let known = resolve_target_accounts(&conn, scope_str, None)?;
    let selected = if let Some(ids) = account_ids {
        ids
    } else if spec.account_scope == "fixed" {
        serde_json::from_value(
            spec.metadata
                .get("accountIds")
                .cloned()
                .ok_or("固定范围缺少 accountIds")?,
        )
        .map_err(|_| "固定账号集合格式无效")?
    } else {
        known.clone()
    };
    if selected.iter().any(|id| !known.contains(id)) {
        return Err("所选账号不属于当前作用域".to_string());
    }
    drop(conn);
    trigger_task_at_with_handler(
        scope_str,
        task_id,
        trigger_type,
        Some(selected),
        &OfflineTaskActionHandler,
        due,
    )
    .await
}

/// 触发任务执行（支持注入 Handler，用于单测模拟各种上游与网络延迟）。
pub async fn trigger_task_with_handler<H: TaskActionHandler>(
    scope_str: &str,
    task_id: &str,
    trigger_type: Option<&str>,
    account_ids: Option<Vec<String>>,
    handler: &H,
) -> Result<TaskRun, String> {
    trigger_task_at_with_handler(scope_str, task_id, trigger_type, account_ids, handler, None).await
}

pub(super) async fn trigger_task_at_with_handler<H: TaskActionHandler>(
    scope_str: &str,
    task_id: &str,
    trigger_type: Option<&str>,
    account_ids: Option<Vec<String>>,
    handler: &H,
    due: Option<i64>,
) -> Result<TaskRun, String> {
    let scope = validate_scope(scope_str)?;
    let mut conn = store::open_db()?;
    let (mut run, accounts) = {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;

        if let Some(due) = due {
            if let Some(spec) = store::query_task_specs(&tx, scope.as_str())?
                .into_iter()
                .find(|spec| spec.id == task_id)
            {
                if !spec.enabled {
                    return Err("此任务计划已停用".into());
                }
                let schedule = runtime::DailySchedule::parse(
                    spec.schedule_cron.as_deref().ok_or("缺少排程表达式")?,
                    &spec.timezone,
                )?;
                if schedule.latest_due(due)? != Some(due) {
                    return Err("任务排程已变更，拒绝派发旧时点".into());
                }
            }
            if let Some(run_id) = runtime::scheduled_run(&tx, scope.as_str(), task_id, due)? {
                return store::query_task_runs(&tx, scope.as_str(), Some(&run_id))?
                    .into_iter()
                    .next()
                    .ok_or("排程时点指向的运行不存在，拒绝重复派发".into());
            }
        }

        // 1. 重入幂等性检查（PRD §7.1：同一按钮重入或重复请求返回已有运行状态，不产生第二个 run）
        if let Some(active_run) = store::query_active_task_run(&tx, scope.as_str(), task_id)? {
            if let Some(due) = due {
                runtime::record_slot(&tx, scope.as_str(), task_id, due, &active_run.run_id)?;
                tx.commit().map_err(|e| e.to_string())?;
            }
            return Ok(active_run);
        }

        // 2. 准备新运行
        let now = now_ms();
        let run_id = format!("run-{}-{}", scope.as_str(), uuid::Uuid::new_v4());
        let accounts = resolve_target_accounts(&tx, scope.as_str(), account_ids)?;
        if accounts.is_empty() {
            return Err("没有可执行账号，未创建任务运行".to_string());
        }
        let trigger_src = trigger_type.unwrap_or("manual").to_string();

        let task_kind = if let Some(spec) = store::query_task_specs(&tx, scope.as_str())?
            .into_iter()
            .find(|s| s.id == task_id)
        {
            spec.task_kind
        } else if task_id.contains("checkin") {
            "checkin".to_string()
        } else if task_id.contains("reward") {
            "reward_claim".to_string()
        } else {
            "credits_sync".to_string()
        };

        let run = TaskRun {
            run_id: run_id.clone(),
            task_id: task_id.to_string(),
            scope: scope.as_str().to_string(),
            task_kind: task_kind.clone(),
            trigger_source: trigger_src.clone(),
            triggered_by: trigger_src,
            status: "running".to_string(),
            started_at_ms: now,
            started_at: now / 1000,
            finished_at_ms: None,
            finished_at: None,
            total_items: accounts.len(),
            success_items: 0,
            failed_items: 0,
            message: Some("正在执行任务...".to_string()),
            details_json: Some("{}".to_string()),
            error: None,
            cursor: Some(0),
        };
        store::upsert_task_run(&tx, &run)?;
        runtime::register_run(&tx, &run_id, now)?;
        if let Some(due) = due {
            runtime::record_slot(&tx, scope.as_str(), task_id, due, &run_id)?;
        }

        // 3. 初始逐账号项登记为 pending 并记录创建事件
        for aid in &accounts {
            let item = TaskItem {
                id: None,
                run_id: run_id.clone(),
                scope: scope.as_str().to_string(),
                account_id: aid.clone(),
                stage: "prepare".to_string(),
                status: "pending".to_string(),
                started_at_ms: now,
                finished_at_ms: None,
                result_data: None,
                error: None,
                retry_count: 0,
            };
            store::upsert_task_item(&tx, &item)?;
        }
        store::insert_task_event(
            &tx,
            &run_id,
            scope.as_str(),
            "",
            "prepare",
            "running",
            now,
            None,
            Some("任务已创建并启动"),
        )?;
        tx.commit().map_err(|e| e.to_string())?;
        (run, accounts)
    };
    let run_id = run.run_id.clone();

    // 4. 逐账号执行（带账号锁、超时判定与游标更新）
    for (idx, aid) in accounts.iter().enumerate() {
        let (lock_acquired, mut item) = {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            let current = store::query_task_items(&tx, &run_id)?
                .into_iter()
                .find(|i| &i.account_id == aid)
                .ok_or("执行项丢失")?;
            if !matches!(current.status.as_str(), "pending" | "queued") {
                continue;
            }
            // 获取账号锁
            let lock_acquired = store::acquire_account_lock(
                &tx,
                scope.as_str(),
                aid,
                task_id,
                &run_id,
                ACCOUNT_LOCK_TTL_MS,
                now_ms(),
            )?;

            let item = TaskItem {
                id: None,
                run_id: run_id.clone(),
                scope: scope.as_str().to_string(),
                account_id: aid.clone(),
                stage: "upstream_write".to_string(),
                status: "running".to_string(),
                started_at_ms: now_ms(),
                finished_at_ms: None,
                result_data: None,
                error: None,
                retry_count: 0,
            };
            store::upsert_task_item(&tx, &item)?;
            store::insert_task_event(
                &tx,
                &run_id,
                scope.as_str(),
                aid,
                "upstream_write",
                "running",
                item.started_at_ms,
                None,
                Some("开始处理账号执行项"),
            )?;
            tx.commit().map_err(|e| e.to_string())?;
            (lock_acquired, item)
        };

        let exec_result = if lock_acquired {
            let res = execute_with_lease(&run, aid, handler).await;
            let _ = store::release_account_lock(&conn, scope.as_str(), aid, task_id, &run_id);
            res
        } else {
            Ok(TaskItemExecutionResult {
                status: "failed".to_string(),
                stage: "lock_failed".to_string(),
                message: None,
                data: None,
                error: Some("账号锁冲突：该账号正在被其它活跃任务执行".to_string()),
            })
        };

        match exec_result {
            Ok(res) => {
                item.status = sanitize_task_status(&res.status).to_string();
                item.stage = sanitize_task_stage(&res.stage).to_string();
                item.finished_at_ms = Some(now_ms());
                item.result_data = res.data;
                item.error = res.error;
            }
            Err(err) => {
                item.status = "pending_confirmation".to_string();
                item.stage = "waiting_verification".to_string();
                item.finished_at_ms = Some(now_ms());
                item.error = Some(err);
            }
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let current = store::query_task_items(&tx, &run_id)?
            .into_iter()
            .find(|i| &i.account_id == aid)
            .ok_or("执行项丢失")?;
        if current.status == "running" {
            store::upsert_task_item(&tx, &item)?;
            // 严格事件脱敏：事件只记录受控分类与固定短文案，绝不透传原始 error（防止泄漏凭据、prompt、原始响应）
            let (err_category, msg) = if item.status == "failed" {
                if item.stage == "lock_failed" {
                    (Some("lock_conflict"), Some("账号锁冲突：已被其它任务锁定"))
                } else {
                    (Some("upstream_error"), Some("上游调用失败，可稍后重试"))
                }
            } else if item.status == "pending_confirmation" {
                (Some("waiting_verification"), Some("写请求已发出，转入待确认状态"))
            } else {
                (None, Some("账号执行完成"))
            };
            store::insert_task_event(
                &tx,
                &run_id,
                scope.as_str(),
                aid,
                &item.stage,
                &item.status,
                item.finished_at_ms.unwrap_or_else(now_ms),
                err_category,
                msg,
            )?;
        }

        // 更新游标
        run = store::query_task_runs(&tx, scope.as_str(), Some(&run_id))?
            .into_iter()
            .next()
            .ok_or("运行丢失")?;
        run.cursor = Some(idx as i64 + 1);
        store::upsert_task_run(&tx, &run)?;
        tx.commit().map_err(|e| e.to_string())?;
    }

    // 释放该 run 占用的所有锁
    let _ = store::release_owner_locks(&conn, &run_id);

    // 5. 汇总结算
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let all_items = store::query_task_items(&tx, &run_id)?;
    let success_count = all_items
        .iter()
        .filter(|i| matches!(i.status.as_str(), "confirmed" | "already_claimed"))
        .count();
    let failed_count = all_items.iter().filter(|i| i.status == "failed").count();
    let has_pending_confirmation = all_items.iter().any(|i| i.status == "pending_confirmation");
    let has_cancelled = all_items.iter().any(|i| i.status == "cancelled");

    run.success_items = success_count;
    run.failed_items = failed_count;
    let finish_time = now_ms();
    run.finished_at_ms = Some(finish_time);
    run.finished_at = Some(finish_time / 1000);

    if has_pending_confirmation {
        run.status = "waiting_verification".to_string();
        run.message = Some("存在待确认项，已转入复查状态".to_string());
    } else if has_cancelled {
        run.status = "cancelled".to_string();
        run.message = Some("任务已取消".to_string());
    } else if failed_count > 0 && success_count == 0 {
        run.status = "failed".to_string();
        run.message = Some("任务执行失败".to_string());
    } else {
        run.status = "completed".to_string();
        run.message = Some(
            if all_items.iter().all(|i| i.status == "unsupported") {
                "尚未接入平台动作，所有账号均未执行"
            } else {
                "任务执行完成"
            }
            .to_string(),
        );
    }

    store::upsert_task_run(&tx, &run)?;
    let run_err_cat = if run.status == "failed" {
        Some("execution_failed")
    } else if run.status == "waiting_verification" {
        Some("waiting_verification")
    } else if run.status == "cancelled" {
        Some("user_cancelled")
    } else {
        None
    };
    store::insert_task_event(
        &tx,
        &run.run_id,
        scope.as_str(),
        "",
        "finish",
        &run.status,
        finish_time,
        run_err_cat,
        run.message.as_deref(),
    )?;
    runtime::finish_run(&tx, &run.run_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(run)
}

/// 停止取消任务运行（PRD §5.3：未开始账号设为 cancelled；已发写请求设为 pending_confirmation，后续仅复查）。
pub fn cancel_task_run(scope_str: &str, run_id: &str) -> Result<TaskRun, String> {
    let conn = store::open_db()?;
    cancel_task_run_in_conn(&conn, scope_str, run_id)
}

/// 停止取消任务运行（指定数据库连接，便于单元测试与隔离执行）。
pub fn cancel_task_run_in_conn(
    conn: &rusqlite::Connection,
    scope_str: &str,
    run_id: &str,
) -> Result<TaskRun, String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let run = cancel_task_run_transaction(&tx, scope_str, run_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(run)
}

fn cancel_task_run_transaction(
    conn: &rusqlite::Connection,
    scope_str: &str,
    run_id: &str,
) -> Result<TaskRun, String> {
    let scope = validate_scope(scope_str)?;
    let runs = store::query_task_runs(conn, scope.as_str(), Some(run_id))?;
    let mut run = runs
        .into_iter()
        .next()
        .ok_or_else(|| format!("未找到指定的任务运行: {run_id}"))?;

    // 已终态直接返回
    if matches!(run.status.as_str(), "completed" | "failed" | "cancelled") {
        return Ok(run);
    }

    let items = store::query_task_items(conn, run_id)?;
    let now = now_ms();
    let mut has_pending_confirmation = false;
    let mut items_transitioned = 0;

    for mut item in items {
        match item.status.as_str() {
            "pending" | "queued" => {
                items_transitioned += 1;
                // 尚未开始：直接取消
                item.status = "cancelled".to_string();
                item.stage = "cancelled".to_string();
                item.finished_at_ms = Some(now);
                item.error = Some("用户手动取消".to_string());
                store::upsert_task_item(conn, &item)?;
                store::insert_task_event(
                    conn,
                    run_id,
                    scope.as_str(),
                    &item.account_id,
                    "cancelled",
                    "cancelled",
                    now,
                    Some("user_cancelled"),
                    Some("用户手动取消未开始项"),
                )?;
            }
            "running" | "waiting_upstream" => {
                items_transitioned += 1;
                // 已发到上游写请求：转为待确认，后续只复查状态
                item.status = "pending_confirmation".to_string();
                item.stage = "waiting_verification".to_string();
                item.finished_at_ms = Some(now);
                item.error = Some("写请求已发出，取消操作转入待确认，后续仅复查状态".to_string());
                has_pending_confirmation = true;
                store::upsert_task_item(conn, &item)?;
                store::insert_task_event(
                    conn,
                    run_id,
                    scope.as_str(),
                    &item.account_id,
                    "waiting_verification",
                    "pending_confirmation",
                    now,
                    Some("waiting_verification"),
                    Some("写请求已发出，取消操作转入待确认"),
                )?;
            }
            "pending_confirmation" => {
                has_pending_confirmation = true;
            }
            _ => {
                // 已终态项（confirmed, already_claimed, failed, skipped）保留原样
            }
        }
    }

    // 幂等：若未发生任何状态流转，且当前 run 已处于终态或 waiting_verification，不重复写入 cancel 事件
    if items_transitioned == 0 && (run.status == "waiting_verification" || run.status == "cancelled") {
        return Ok(run);
    }

    // 在途 handler 仍可能发送/接收上游请求，由执行者返回后释放锁。

    // 重新统计 items 状态
    let all_items = store::query_task_items(conn, run_id)?;
    let success_count = all_items
        .iter()
        .filter(|i| matches!(i.status.as_str(), "confirmed" | "already_claimed"))
        .count();
    let failed_count = all_items.iter().filter(|i| i.status == "failed").count();

    run.success_items = success_count;
    run.failed_items = failed_count;
    run.finished_at_ms = Some(now);
    run.finished_at = Some(now / 1000);

    if has_pending_confirmation {
        run.status = "waiting_verification".to_string();
        run.message = Some(
            "任务已请求停止，未开始项已取消；已发出写请求的账号转为待确认，后续仅复查状态"
                .to_string(),
        );
    } else {
        run.status = "cancelled".to_string();
        run.message = Some("任务已由用户手动停止并取消".to_string());
    }

    store::upsert_task_run(conn, &run)?;
    let run_err_cat = if run.status == "waiting_verification" {
        Some("waiting_verification")
    } else {
        Some("user_cancelled")
    };
    store::insert_task_event(
        conn,
        run_id,
        scope.as_str(),
        "",
        "cancel",
        &run.status,
        now,
        run_err_cat,
        run.message.as_deref(),
    )?;
    Ok(run)
}

/// 重试失败项（生产入口）。
pub async fn retry_task_run(scope_str: &str, run_id: &str) -> Result<TaskRun, String> {
    retry_task_run_with_handler(scope_str, run_id, &OfflineTaskActionHandler).await
}

/// 重试失败项（支持注入 Handler）。
///
/// 遵循 PRD §5.3：批量「重试失败」自动排除待确认项、已领项和不适用项，仅对 failed 项进行重试。
pub async fn retry_task_run_with_handler<H: TaskActionHandler>(
    scope_str: &str,
    run_id: &str,
    handler: &H,
) -> Result<TaskRun, String> {
    let scope = validate_scope(scope_str)?;
    let mut conn = store::open_db()?;
    let (mut run, failed_items) = {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let runs = store::query_task_runs(&tx, scope.as_str(), Some(run_id))?;
        let mut run = runs
            .into_iter()
            .next()
            .ok_or_else(|| format!("未找到指定的任务运行: {run_id}"))?;

        if matches!(
            run.status.as_str(),
            "running" | "queued" | "waiting_upstream"
        ) {
            return Err("任务仍在执行中，不能重复重试".to_string());
        }
        let live_executor: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_run_leases WHERE run_id = ?1 AND expires_at_ms > ?2)",
            rusqlite::params![run_id, now_ms()], |row| row.get(0),
        ).map_err(|e| e.to_string())?;
        if live_executor {
            return Err("原运行仍有在途执行者，请待执行结束后重试失败项".into());
        }
        let items = store::query_task_items(&tx, run_id)?;
        // 严格过滤出仅 failed 的项！自动排除 confirmed, already_claimed, pending_confirmation, skipped, unsupported
        let failed_items: Vec<TaskItem> =
            items.into_iter().filter(|i| i.status == "failed").collect();

        if failed_items.is_empty() {
            return Err("无可重试的失败项（待确认、已领及跳过项不参与重试）".to_string());
        }

        run.status = "running".to_string();
        run.finished_at_ms = None;
        run.finished_at = None;
        run.error = None;
        run.message = Some("正在重试失败项...".to_string());
        store::upsert_task_run(&tx, &run)?;
        runtime::register_run(&tx, &run.run_id, now_ms())?;
        store::insert_task_event(
            &tx,
            &run.run_id,
            scope.as_str(),
            "",
            "retry_start",
            "running",
            now_ms(),
            None,
            Some("开始重试失败项"),
        )?;
        // 重试项先统一排队，取消入口才能阻止尚未开始的重试。
        for item in &failed_items {
            let mut queued = item.clone();
            queued.status = "queued".to_string();
            store::upsert_task_item(&tx, &queued)?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        (run, failed_items)
    };

    for mut item in failed_items {
        let lock_acquired = {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            let current = store::query_task_items(&tx, run_id)?
                .into_iter()
                .find(|i| i.account_id == item.account_id)
                .ok_or("执行项丢失")?;
            if current.status != "queued" {
                continue;
            }
            item.retry_count += 1;
            item.status = "running".to_string();
            item.stage = "upstream_write".to_string();
            item.started_at_ms = now_ms();
            item.finished_at_ms = None;
            item.error = None;
            store::upsert_task_item(&tx, &item)?;
            store::insert_task_event(
                &tx,
                &run.run_id,
                scope.as_str(),
                &item.account_id,
                "upstream_write",
                "running",
                item.started_at_ms,
                None,
                Some("重试开始处理账号执行项"),
            )?;

            let lock_acquired = store::acquire_account_lock(
                &tx,
                scope.as_str(),
                &item.account_id,
                &run.task_id,
                &run.run_id,
                ACCOUNT_LOCK_TTL_MS,
                now_ms(),
            )?;

            tx.commit().map_err(|e| e.to_string())?;
            lock_acquired
        };
        let res = if lock_acquired {
            let r = execute_with_lease(&run, &item.account_id, handler).await;
            let _ = store::release_account_lock(
                &conn,
                scope.as_str(),
                &item.account_id,
                &run.task_id,
                &run.run_id,
            );
            r
        } else {
            Ok(TaskItemExecutionResult {
                status: "failed".to_string(),
                stage: "lock_failed".to_string(),
                message: None,
                data: None,
                error: Some("账号锁冲突：该账号正在被其它活跃任务执行".to_string()),
            })
        };

        match res {
            Ok(exec_res) => {
                item.status = sanitize_task_status(&exec_res.status).to_string();
                item.stage = sanitize_task_stage(&exec_res.stage).to_string();
                item.finished_at_ms = Some(now_ms());
                item.result_data = exec_res.data;
                item.error = exec_res.error;
            }
            Err(err) => {
                item.status = "pending_confirmation".to_string();
                item.stage = "waiting_verification".to_string();
                item.finished_at_ms = Some(now_ms());
                item.error = Some(err);
            }
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let current = store::query_task_items(&tx, run_id)?
            .into_iter()
            .find(|i| i.account_id == item.account_id)
            .ok_or("执行项丢失")?;
        if current.status == "running" {
            store::upsert_task_item(&tx, &item)?;
            // 严格事件脱敏：事件只记录受控分类与固定短文案，绝不透传原始 error
            let (err_category, msg) = if item.status == "failed" {
                if item.stage == "lock_failed" {
                    (Some("lock_conflict"), Some("账号锁冲突：重试期间已被其它任务锁定"))
                } else {
                    (Some("upstream_error"), Some("重试失败，等待后续处理"))
                }
            } else if item.status == "pending_confirmation" {
                (Some("waiting_verification"), Some("写请求已发出，重试转入待确认状态"))
            } else {
                (None, Some("重试账号执行完成"))
            };
            store::insert_task_event(
                &tx,
                &run.run_id,
                scope.as_str(),
                &item.account_id,
                &item.stage,
                &item.status,
                item.finished_at_ms.unwrap_or_else(now_ms),
                err_category,
                msg,
            )?;
        }
        tx.commit().map_err(|e| e.to_string())?;
    }

    let _ = store::release_owner_locks(&conn, &run.run_id);

    // 重新聚合
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let all_items = store::query_task_items(&tx, run_id)?;
    let success_count = all_items
        .iter()
        .filter(|i| matches!(i.status.as_str(), "confirmed" | "already_claimed"))
        .count();
    let failed_count = all_items.iter().filter(|i| i.status == "failed").count();
    let has_pending_confirmation = all_items.iter().any(|i| i.status == "pending_confirmation");

    let has_cancelled = all_items.iter().any(|i| i.status == "cancelled");

    run.success_items = success_count;
    run.failed_items = failed_count;
    let finish_time = now_ms();
    run.finished_at_ms = Some(finish_time);
    run.finished_at = Some(finish_time / 1000);

    if has_pending_confirmation {
        run.status = "waiting_verification".to_string();
        run.message = Some("存在待确认项，已转入复查状态".to_string());
    } else if has_cancelled {
        run.status = "cancelled".to_string();
        run.message = Some("任务已取消".to_string());
    } else if failed_count > 0 && success_count == 0 {
        run.status = "failed".to_string();
        run.message = Some("重试完成，仍存在失败项".to_string());
    } else {
        run.status = "completed".to_string();
        run.message = Some("重试执行完成".to_string());
    }

    store::upsert_task_run(&tx, &run)?;
    let run_err_cat = if run.status == "failed" {
        Some("execution_failed")
    } else if run.status == "waiting_verification" {
        Some("waiting_verification")
    } else if run.status == "cancelled" {
        Some("user_cancelled")
    } else {
        None
    };
    store::insert_task_event(
        &tx,
        &run.run_id,
        scope.as_str(),
        "",
        "finish",
        &run.status,
        finish_time,
        run_err_cat,
        run.message.as_deref(),
    )?;
    runtime::finish_run(&tx, &run.run_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(run)
}
