//! 管理数据库持久化（FUS-W03 / PRD §9.1）。
//!
//! 基于 rusqlite 的 `management.sqlite` 长期管理数据库与事务性 schema 迁移。
//! 严格隔离在通过启动护栏的数据根下，采用版本化事务升级。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::TimeZone;
use rusqlite::{params, Connection, OptionalExtension};

use crate::modules::config::store_dir;
use crate::modules::management::models::{
    AccountRef, CreditEvent, CreditObservation, CreditPackageItem, TaskEvent, TaskEventPage,
    TaskItem, TaskRun, TaskSpec,
};

static DB_MUTEX: Mutex<()> = Mutex::new(());

/// 当前已知支持的最大 schema 迁移版本。
pub const CURRENT_SCHEMA_VERSION: i64 = 6;

/// 默认管理数据库文件路径。
pub fn db_path() -> PathBuf {
    store_dir().join("management.sqlite")
}

/// 打开持久化管理数据库并保证迁移执行完毕。
pub fn open_db() -> Result<Connection, String> {
    if let Some(error) = crate::modules::config::env_home_override_refusal() {
        return Err(error);
    }
    let _lock = DB_MUTEX.lock().map_err(|e| format!("DB lock error: {e}"))?;
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(&path).map_err(|e| format!("打开 management.sqlite 失败: {e}"))?;
    ensure_schema(&conn)?;
    Ok(conn)
}

/// 打开指定路径的数据库（用于测试与隔离环境）。
pub fn open_db_at(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path).map_err(|e| format!("打开数据库 {path:?} 失败: {e}"))?;
    ensure_schema(&conn)?;
    Ok(conn)
}

/// 打开纯内存数据库（用于单元测试）。
pub fn open_memory_db() -> Result<Connection, String> {
    let conn = Connection::open_in_memory().map_err(|e| format!("创建内存 DB 失败: {e}"))?;
    ensure_schema(&conn)?;
    Ok(conn)
}

/// 事务性版本化数据库迁移。
pub fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );",
    )
    .map_err(|e| format!("初始化 schema_migrations 失败: {e}"))?;

    let current_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // 拒绝未知的高版本（未来版本）数据库，避免降级破坏数据
    if current_version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "检测到未知的高版本数据库 schema (v{current_version} > v{CURRENT_SCHEMA_VERSION})，拒绝降级运行以保护数据"
        ));
    }

    if current_version < 1 {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("开启迁移事务失败: {e}"))?;

        tx.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS account_refs (
                scope TEXT NOT NULL,
                account_id TEXT NOT NULL,
                display_name TEXT NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 1,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(scope, account_id)
            );

            CREATE TABLE IF NOT EXISTS credit_observations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                account_id TEXT NOT NULL,
                source TEXT NOT NULL,
                remaining REAL NOT NULL,
                total REAL NOT NULL,
                unit TEXT NOT NULL,
                is_stale INTEGER NOT NULL DEFAULT 0,
                is_comparable INTEGER NOT NULL DEFAULT 1,
                observed_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_obs_scope_acc ON credit_observations(scope, account_id, observed_at_ms DESC);

            CREATE TABLE IF NOT EXISTS credit_packages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                account_id TEXT NOT NULL,
                account_name TEXT NOT NULL DEFAULT '',
                package_id TEXT NOT NULL,
                name TEXT NOT NULL,
                package_type TEXT NOT NULL,
                remaining REAL NOT NULL,
                total REAL NOT NULL,
                expire_at_ms INTEGER,
                is_inferred INTEGER NOT NULL DEFAULT 0,
                is_stale INTEGER NOT NULL DEFAULT 0,
                source TEXT NOT NULL DEFAULT 'official',
                observed_at_ms INTEGER NOT NULL
            );
            -- 使用 ifnull 将 NULL expire_at_ms 转为 -1 进行稳定唯一性约束，确保 NULL 到期时间也能幂等去重
            CREATE UNIQUE INDEX IF NOT EXISTS idx_pkg_unique ON credit_packages(scope, account_id, package_id, ifnull(expire_at_ms, -1));
            CREATE INDEX IF NOT EXISTS idx_pkg_scope_expire ON credit_packages(scope, expire_at_ms);

            CREATE TABLE IF NOT EXISTS credit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                account_id TEXT NOT NULL,
                account_name TEXT,
                event_id TEXT,
                event_kind TEXT NOT NULL,
                amount REAL NOT NULL,
                unit TEXT NOT NULL,
                occurred_at_ms INTEGER NOT NULL,
                evidence_level TEXT NOT NULL,
                note TEXT,
                UNIQUE(scope, account_id, event_kind, occurred_at_ms, amount)
            );
            CREATE INDEX IF NOT EXISTS idx_evt_scope_time ON credit_events(scope, occurred_at_ms DESC);

            CREATE TABLE IF NOT EXISTS task_specs (
                id TEXT PRIMARY KEY,
                scope TEXT NOT NULL,
                task_kind TEXT NOT NULL,
                account_scope TEXT NOT NULL,
                schedule_cron TEXT,
                timezone TEXT NOT NULL DEFAULT 'UTC',
                enabled INTEGER NOT NULL DEFAULT 1,
                retry_policy TEXT,
                metadata TEXT NOT NULL DEFAULT '{}',
                updated_at_ms INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS task_runs (
                run_id TEXT PRIMARY KEY,
                scope TEXT NOT NULL,
                task_kind TEXT NOT NULL,
                trigger_source TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER,
                error TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_task_runs_scope_time ON task_runs(scope, started_at_ms DESC);

            CREATE TABLE IF NOT EXISTS task_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                account_id TEXT NOT NULL,
                stage TEXT NOT NULL,
                status TEXT NOT NULL,
                started_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER,
                result_data_json TEXT,
                error TEXT,
                UNIQUE(run_id, account_id)
            );

            INSERT INTO schema_migrations (version, name, applied_at_ms)
            VALUES (1, 'initial_management_schema', 1700000000000);
            ",
        )
        .map_err(|e| format!("执行 v1 迁移 SQL 失败: {e}"))?;

        tx.commit().map_err(|e| format!("提交 v1 迁移失败: {e}"))?;
    }

    if current_version < 2 {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("开启 v2 迁移事务失败: {e}"))?;

        tx.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS task_account_locks (
                scope TEXT NOT NULL,
                account_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                owner_id TEXT NOT NULL,
                acquired_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER NOT NULL,
                PRIMARY KEY(scope, account_id, task_id)
            );
            CREATE INDEX IF NOT EXISTS idx_task_account_locks_owner ON task_account_locks(owner_id);
            ",
        )
        .map_err(|e| format!("创建 task_account_locks 表失败: {e}"))?;

        // 检查并扩展 task_runs 字段
        let add_column_if_missing = |tx: &rusqlite::Transaction,
                                     table: &str,
                                     col: &str,
                                     ty: &str|
         -> Result<(), rusqlite::Error> {
            let pragma_sql = format!("PRAGMA table_info({})", table);
            let mut stmt = tx.prepare(&pragma_sql)?;
            let mut exists = false;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let name: String = row.get(1)?;
                if name == col {
                    exists = true;
                    break;
                }
            }
            if !exists {
                tx.execute(
                    &format!("ALTER TABLE {} ADD COLUMN {} {}", table, col, ty),
                    [],
                )?;
            }
            Ok(())
        };

        add_column_if_missing(&tx, "task_runs", "task_id", "TEXT NOT NULL DEFAULT ''")
            .map_err(|e| format!("扩展 task_runs.task_id 失败: {e}"))?;
        add_column_if_missing(
            &tx,
            "task_runs",
            "total_items",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .map_err(|e| format!("扩展 task_runs.total_items 失败: {e}"))?;
        add_column_if_missing(
            &tx,
            "task_runs",
            "success_items",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .map_err(|e| format!("扩展 task_runs.success_items 失败: {e}"))?;
        add_column_if_missing(
            &tx,
            "task_runs",
            "failed_items",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .map_err(|e| format!("扩展 task_runs.failed_items 失败: {e}"))?;
        add_column_if_missing(&tx, "task_runs", "message", "TEXT")
            .map_err(|e| format!("扩展 task_runs.message 失败: {e}"))?;
        add_column_if_missing(&tx, "task_runs", "details_json", "TEXT")
            .map_err(|e| format!("扩展 task_runs.details_json 失败: {e}"))?;
        add_column_if_missing(&tx, "task_runs", "cursor_id", "INTEGER")
            .map_err(|e| format!("扩展 task_runs.cursor_id 失败: {e}"))?;

        add_column_if_missing(&tx, "task_items", "result_data_json", "TEXT")
            .map_err(|e| format!("扩展 task_items.result_data_json 失败: {e}"))?;
        add_column_if_missing(
            &tx,
            "task_items",
            "retry_count",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .map_err(|e| format!("扩展 task_items.retry_count 失败: {e}"))?;

        tx.execute_batch(
            "
            CREATE INDEX IF NOT EXISTS idx_task_items_run ON task_items(run_id);
            CREATE INDEX IF NOT EXISTS idx_task_items_status ON task_items(run_id, status);
            INSERT INTO schema_migrations (version, name, applied_at_ms)
            VALUES (2, 'task_locks_and_execution_records', 1700000000001);
            ",
        )
        .map_err(|e| format!("执行 v2 迁移索引与版本更新失败: {e}"))?;

        tx.commit().map_err(|e| format!("提交 v2 迁移失败: {e}"))?;
    }

    if current_version < 3 {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("开启 v3 迁移事务失败: {e}"))?;

        let add_column_if_missing = |tx: &rusqlite::Transaction,
                                     table: &str,
                                     col: &str,
                                     ty: &str|
         -> Result<(), rusqlite::Error> {
            let pragma_sql = format!("PRAGMA table_info({})", table);
            let mut stmt = tx.prepare(&pragma_sql)?;
            let mut exists = false;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let name: String = row.get(1)?;
                if name == col {
                    exists = true;
                    break;
                }
            }
            if !exists {
                tx.execute(
                    &format!("ALTER TABLE {} ADD COLUMN {} {}", table, col, ty),
                    [],
                )?;
            }
            Ok(())
        };

        add_column_if_missing(&tx, "task_items", "result_data_json", "TEXT")
            .map_err(|e| format!("扩展 task_items.result_data_json 失败: {e}"))?;

        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (3, 'task_items_result_data_json', 1700000000002)",
            [],
        )
        .map_err(|e| format!("写入 v3 迁移记录失败: {e}"))?;

        tx.commit().map_err(|e| format!("提交 v3 迁移失败: {e}"))?;
    }

    if current_version < 4 {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("开启 v4 迁移事务失败: {e}"))?;

        tx.execute_batch(
            "
            CREATE UNIQUE INDEX IF NOT EXISTS idx_task_items_run_acc ON task_items(run_id, account_id);
            INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (4, 'task_items_unique_index', 1700000000003);
            ",
        )
        .map_err(|e| format!("写入 v4 迁移记录失败: {e}"))?;

        tx.commit().map_err(|e| format!("提交 v4 迁移失败: {e}"))?;
    }

    if current_version < 5 {
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_run_leases (
                run_id TEXT PRIMARY KEY, owner_id TEXT NOT NULL, expires_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS task_schedule_slots (
                scope TEXT NOT NULL, task_id TEXT NOT NULL, due_at_ms INTEGER NOT NULL,
                run_id TEXT NOT NULL, PRIMARY KEY(scope, task_id, due_at_ms)
             );
             INSERT OR IGNORE INTO schema_migrations (version, name, applied_at_ms)
             VALUES (5, 'management_schedule_slots_and_run_leases', 1700000000004);",
        )
        .map_err(|e| format!("执行 v5 迁移失败: {e}"))?;
        tx.commit().map_err(|e| e.to_string())?;
    }

    if current_version < 6 {
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                account_id TEXT NOT NULL DEFAULT '',
                stage TEXT NOT NULL,
                status TEXT NOT NULL,
                occurred_at_ms INTEGER NOT NULL,
                error_category TEXT,
                message TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_task_events_scope_id ON task_events(scope, id ASC);
            CREATE INDEX IF NOT EXISTS idx_task_events_run_id ON task_events(run_id, id ASC);
            INSERT OR IGNORE INTO schema_migrations (version, name, applied_at_ms)
            VALUES (6, 'management_task_events_v6', 1700000000005);",
        )
        .map_err(|e| format!("执行 v6 迁移失败: {e}"))?;
        tx.commit().map_err(|e| e.to_string())?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 账号引用快照 (Account Refs)
// ---------------------------------------------------------------------------

pub fn upsert_account_ref(conn: &Connection, acc: &AccountRef) -> Result<(), String> {
    conn.execute(
        "INSERT INTO account_refs (scope, account_id, display_name, is_active, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(scope, account_id) DO UPDATE SET
            display_name = excluded.display_name,
            is_active = excluded.is_active,
            updated_at_ms = excluded.updated_at_ms",
        params![
            acc.scope,
            acc.account_id,
            acc.display_name,
            if acc.is_active { 1 } else { 0 },
            acc.updated_at_ms
        ],
    )
    .map_err(|e| format!("upsert_account_ref 失败: {e}"))?;
    Ok(())
}

pub fn query_account_refs(conn: &Connection, scope: &str) -> Result<Vec<AccountRef>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT scope, account_id, display_name, is_active, updated_at_ms
             FROM account_refs WHERE scope = ?1 AND is_active = 1",
        )
        .map_err(|e| format!("prepare query_account_refs 失败: {e}"))?;

    let rows = stmt
        .query_map(params![scope], |row| {
            Ok(AccountRef {
                scope: row.get(0)?,
                account_id: row.get(1)?,
                display_name: row.get(2)?,
                is_active: row.get::<_, i64>(3)? != 0,
                updated_at_ms: row.get(4)?,
            })
        })
        .map_err(|e| format!("query_account_refs 执行失败: {e}"))?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("读取 account_ref 行失败: {e}"))?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 观测记录 (Observations)
// ---------------------------------------------------------------------------

pub fn insert_credit_observation(
    conn: &Connection,
    obs: &CreditObservation,
) -> Result<bool, String> {
    // 5 分钟去重窗口：若该账号在 5 分钟内已有相同 remaining 和 total 的记录，则跳过避免冗余落盘
    let window_start = obs.observed_at_ms - 5 * 60 * 1000;
    let existing_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM credit_observations
             WHERE scope = ?1 AND account_id = ?2 AND remaining = ?3 AND total = ?4 AND observed_at_ms >= ?5",
            params![obs.scope, obs.account_id, obs.remaining, obs.total, window_start],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if existing_count > 0 {
        return Ok(false);
    }

    conn.execute(
        "INSERT INTO credit_observations
         (scope, account_id, source, remaining, total, unit, is_stale, is_comparable, observed_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            obs.scope,
            obs.account_id,
            obs.source,
            obs.remaining,
            obs.total,
            obs.unit,
            if obs.is_stale { 1 } else { 0 },
            if obs.is_comparable { 1 } else { 0 },
            obs.observed_at_ms
        ],
    )
    .map_err(|e| format!("insert_credit_observation 失败: {e}"))?;
    Ok(true)
}

pub fn query_latest_observations(
    conn: &Connection,
    scope: &str,
    filter_account: Option<&str>,
) -> Result<Vec<CreditObservation>, String> {
    let sql = if filter_account.is_some() {
        "SELECT id, scope, account_id, source, remaining, total, unit, is_stale, is_comparable, observed_at_ms
         FROM credit_observations
         WHERE scope = ?1 AND account_id = ?2
         ORDER BY observed_at_ms DESC LIMIT 1"
    } else {
        "SELECT id, scope, account_id, source, remaining, total, unit, is_stale, is_comparable, observed_at_ms
         FROM credit_observations
         WHERE (scope, account_id, observed_at_ms) IN (
             SELECT scope, account_id, MAX(observed_at_ms)
             FROM credit_observations
             WHERE scope = ?1
             GROUP BY scope, account_id
         )"
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("prepare query_latest_observations 失败: {e}"))?;

    let mapper = |row: &rusqlite::Row| {
        Ok(CreditObservation {
            id: row.get(0)?,
            scope: row.get(1)?,
            account_id: row.get(2)?,
            source: row.get(3)?,
            remaining: row.get(4)?,
            total: row.get(5)?,
            unit: row.get(6)?,
            is_stale: row.get::<_, i64>(7)? != 0,
            is_comparable: row.get::<_, i64>(8)? != 0,
            observed_at_ms: row.get(9)?,
        })
    };

    let mut out = Vec::new();
    if let Some(acc) = filter_account {
        let rows = stmt
            .query_map(params![scope, acc], mapper)
            .map_err(|e| format!("query latest obs by account 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    } else {
        let rows = stmt
            .query_map(params![scope], mapper)
            .map_err(|e| format!("query latest obs 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 积分包明细 (Credit Packages)
// ---------------------------------------------------------------------------

/// 单个积分包的稳定幂等 upsert（针对 NULL expire_at_ms 也保证唯一与幂等）。
pub fn upsert_credit_package(conn: &Connection, pkg: &CreditPackageItem) -> Result<(), String> {
    let existing_id: Option<i64> = match pkg.expire_at_ms {
        Some(exp) => conn
            .query_row(
                "SELECT id FROM credit_packages 
                 WHERE scope = ?1 AND account_id = ?2 AND package_id = ?3 AND expire_at_ms = ?4",
                params![pkg.scope, pkg.account_id, pkg.package_id, exp],
                |r| r.get(0),
            )
            .ok(),
        None => conn
            .query_row(
                "SELECT id FROM credit_packages 
                 WHERE scope = ?1 AND account_id = ?2 AND package_id = ?3 AND expire_at_ms IS NULL",
                params![pkg.scope, pkg.account_id, pkg.package_id],
                |r| r.get(0),
            )
            .ok(),
    };

    if let Some(id) = existing_id {
        conn.execute(
            "UPDATE credit_packages SET
                account_name = ?1,
                name = ?2,
                package_type = ?3,
                remaining = ?4,
                total = ?5,
                is_inferred = ?6,
                is_stale = ?7,
                source = ?8,
                observed_at_ms = ?9
             WHERE id = ?10",
            params![
                pkg.account_name,
                pkg.name,
                pkg.package_type,
                pkg.remaining,
                pkg.total,
                if pkg.is_inferred { 1 } else { 0 },
                if pkg.is_stale { 1 } else { 0 },
                pkg.source,
                pkg.observed_at_ms,
                id
            ],
        )
        .map_err(|e| format!("update credit_package 失败: {e}"))?;
    } else {
        conn.execute(
            "INSERT INTO credit_packages
             (scope, account_id, account_name, package_id, name, package_type, remaining, total, expire_at_ms, is_inferred, is_stale, source, observed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                pkg.scope,
                pkg.account_id,
                pkg.account_name,
                pkg.package_id,
                pkg.name,
                pkg.package_type,
                pkg.remaining,
                pkg.total,
                pkg.expire_at_ms,
                if pkg.is_inferred { 1 } else { 0 },
                if pkg.is_stale { 1 } else { 0 },
                pkg.source,
                pkg.observed_at_ms
            ],
        )
        .map_err(|e| format!("insert credit_package 失败: {e}"))?;
    }

    Ok(())
}

/// 成功全量采集后的事务快照替换：移除该账号已消失/用尽的旧包，插入最新全量包。
pub fn replace_account_credit_packages(
    conn: &mut Connection,
    scope: &str,
    account_id: &str,
    packages: &[CreditPackageItem],
) -> Result<(), String> {
    let tx = conn
        .transaction()
        .map_err(|e| format!("开启 replace_account_credit_packages 事务失败: {e}"))?;

    tx.execute(
        "DELETE FROM credit_packages WHERE scope = ?1 AND account_id = ?2",
        params![scope, account_id],
    )
    .map_err(|e| format!("清除旧积分包失败: {e}"))?;

    for pkg in packages {
        tx.execute(
            "INSERT INTO credit_packages
             (scope, account_id, account_name, package_id, name, package_type, remaining, total, expire_at_ms, is_inferred, is_stale, source, observed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                pkg.scope,
                pkg.account_id,
                pkg.account_name,
                pkg.package_id,
                pkg.name,
                pkg.package_type,
                pkg.remaining,
                pkg.total,
                pkg.expire_at_ms,
                if pkg.is_inferred { 1 } else { 0 },
                if pkg.is_stale { 1 } else { 0 },
                pkg.source,
                pkg.observed_at_ms
            ],
        )
        .map_err(|e| format!("写入新积分包失败: {e}"))?;
    }

    tx.commit()
        .map_err(|e| format!("提交快照替换事务失败: {e}"))?;
    Ok(())
}

/// 采集失败/部分失败时：保留旧数据，但持久化标记为 stale。
pub fn mark_account_packages_stale(
    conn: &Connection,
    scope: &str,
    account_id: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE credit_packages SET is_stale = 1 WHERE scope = ?1 AND account_id = ?2",
        params![scope, account_id],
    )
    .map_err(|e| format!("mark_account_packages_stale 失败: {e}"))?;
    Ok(())
}

pub fn query_credit_packages(
    conn: &Connection,
    scope: &str,
    filter_account: Option<&str>,
) -> Result<Vec<CreditPackageItem>, String> {
    let sql = if filter_account.is_some() {
        "SELECT id, scope, account_id, account_name, package_id, name, package_type, remaining, total, expire_at_ms, is_inferred, is_stale, source, observed_at_ms
         FROM credit_packages
         WHERE scope = ?1 AND account_id = ?2
         ORDER BY (CASE WHEN expire_at_ms IS NULL THEN 1 ELSE 0 END), expire_at_ms ASC"
    } else {
        "SELECT id, scope, account_id, account_name, package_id, name, package_type, remaining, total, expire_at_ms, is_inferred, is_stale, source, observed_at_ms
         FROM credit_packages
         WHERE scope = ?1
         ORDER BY (CASE WHEN expire_at_ms IS NULL THEN 1 ELSE 0 END), expire_at_ms ASC"
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("prepare query_credit_packages 失败: {e}"))?;

    let mapper = |row: &rusqlite::Row| {
        let expire_at_ms: Option<i64> = row.get(9)?;
        let is_stale = row.get::<_, i64>(11)? != 0;
        let remaining: f64 = row.get(7)?;
        let total: f64 = row.get(8)?;
        let name: String = row.get(5)?;
        let source: String = row.get(12)?;
        let observed_at_ms: i64 = row.get(13)?;

        let expire_time = expire_at_ms.map(|ms| ms / 1000);
        let expire_time_formatted = expire_at_ms.and_then(|ms| {
            chrono::Local.timestamp_millis_opt(ms).single().map(|dt| {
                format!(
                    "{:04}-{:02}-{:02}",
                    chrono::Datelike::year(&dt),
                    chrono::Datelike::month(&dt),
                    chrono::Datelike::day(&dt)
                )
            })
        });

        Ok(CreditPackageItem {
            id: row.get(0)?,
            scope: row.get(1)?,
            account_id: row.get(2)?,
            account_name: row.get(3)?,
            package_id: row.get(4)?,
            name: name.clone(),
            package_name: name,
            package_type: row.get(6)?,
            remaining,
            remaining_balance: remaining,
            total,
            original_capacity: Some(total),
            unit: "credits".to_string(),
            expire_at_ms,
            expire_time,
            expire_time_formatted,
            is_inferred: row.get::<_, i64>(10)? != 0,
            is_stale,
            source: source.clone(),
            source_type: source,
            observed_at_ms,
            observed_at: observed_at_ms / 1000,
            status: if is_stale {
                "stale".to_string()
            } else {
                "active".to_string()
            },
        })
    };

    let mut out = Vec::new();
    if let Some(acc) = filter_account {
        let rows = stmt
            .query_map(params![scope, acc], mapper)
            .map_err(|e| format!("query_credit_packages by account 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    } else {
        let rows = stmt
            .query_map(params![scope], mapper)
            .map_err(|e| format!("query_credit_packages 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 积分变更事件流水 (Credit Events)
// ---------------------------------------------------------------------------

pub fn insert_credit_event(conn: &Connection, evt: &CreditEvent) -> Result<(), String> {
    conn.execute(
        "INSERT INTO credit_events
         (scope, account_id, account_name, event_id, event_kind, amount, unit, occurred_at_ms, evidence_level, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(scope, account_id, event_kind, occurred_at_ms, amount) DO UPDATE SET
            note = excluded.note",
        params![
            evt.scope,
            evt.account_id,
            evt.account_name,
            evt.event_id,
            evt.event_kind,
            evt.amount,
            evt.unit,
            evt.occurred_at_ms,
            evt.evidence_level,
            evt.note
        ],
    )
    .map_err(|e| format!("insert_credit_event 失败: {e}"))?;
    Ok(())
}

/// 数据库级全量聚合区间收支（不受展示分页 limit 截断影响）。
pub fn aggregate_credit_events_summary(
    conn: &Connection,
    scope: &str,
    filter_account: Option<&str>,
    since_ms: Option<i64>,
) -> Result<(Option<f64>, Option<f64>), String> {
    let mut sql = "SELECT event_kind, SUM(amount) FROM credit_events WHERE scope = ?1".to_string();
    let mut param_idx = 2;
    if filter_account.is_some() {
        sql.push_str(&format!(" AND account_id = ?{param_idx}"));
        param_idx += 1;
    }
    if since_ms.is_some() {
        sql.push_str(&format!(" AND occurred_at_ms >= ?{param_idx}"));
    }
    sql.push_str(" GROUP BY event_kind");

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("prepare aggregate_credit_events 失败: {e}"))?;

    let rows: Vec<(String, f64)> = if let (Some(acc), Some(since)) = (filter_account, since_ms) {
        stmt.query_map(params![scope, acc, since], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect()
    } else if let Some(acc) = filter_account {
        stmt.query_map(params![scope, acc], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect()
    } else if let Some(since) = since_ms {
        stmt.query_map(params![scope, since], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect()
    } else {
        stmt.query_map(params![scope], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect()
    };

    let mut period_in: Option<f64> = None;
    let mut period_out: Option<f64> = None;

    for (kind, total_amt) in rows {
        if total_amt > 0.0 && (kind == "checkin" || kind == "reward" || kind == "recharge") {
            period_in = Some(period_in.unwrap_or(0.0) + total_amt);
        } else if kind == "usage" || total_amt < 0.0 {
            period_out = Some(period_out.unwrap_or(0.0) + total_amt.abs());
        }
    }

    Ok((period_in, period_out))
}

pub fn query_credit_events(
    conn: &Connection,
    scope: &str,
    filter_account: Option<&str>,
    since_ms: Option<i64>,
    limit: usize,
) -> Result<Vec<CreditEvent>, String> {
    let mut sql = "SELECT id, scope, account_id, account_name, event_id, event_kind, amount, unit, occurred_at_ms, evidence_level, note
                   FROM credit_events WHERE scope = ?1".to_string();

    let mut param_idx = 2;
    if filter_account.is_some() {
        sql.push_str(&format!(" AND account_id = ?{param_idx}"));
        param_idx += 1;
    }
    if since_ms.is_some() {
        sql.push_str(&format!(" AND occurred_at_ms >= ?{param_idx}"));
        param_idx += 1;
    }
    sql.push_str(&format!(" ORDER BY occurred_at_ms DESC LIMIT ?{param_idx}"));

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("prepare query_credit_events 失败: {e}"))?;

    let mapper = |row: &rusqlite::Row| {
        let kind: String = row.get(5)?;
        let amount: f64 = row.get(6)?;
        let occurred_at_ms: i64 = row.get(8)?;
        let note: Option<String> = row.get(10)?;

        // 消耗/过期事件符号为负向，签到/充值/奖励事件为正向流入
        let change_amount = match kind.as_str() {
            "usage" | "expired" => -amount.abs(),
            _ => amount.abs(),
        };

        Ok(CreditEvent {
            id: row.get(0)?,
            scope: row.get(1)?,
            account_id: row.get(2)?,
            account_name: row.get(3)?,
            event_id: row.get(4)?,
            event_kind: kind.clone(),
            event_type: kind,
            amount: amount.abs(),
            change_amount,
            balance_after: 0.0,
            unit: row.get(7)?,
            occurred_at_ms,
            recorded_at: occurred_at_ms / 1000,
            evidence_level: row.get(9)?,
            note: note.clone(),
            reason: note,
        })
    };

    let mut out = Vec::new();
    if let (Some(acc), Some(since)) = (filter_account, since_ms) {
        let rows = stmt
            .query_map(params![scope, acc, since, limit as i64], mapper)
            .map_err(|e| format!("query events 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    } else if let Some(acc) = filter_account {
        let rows = stmt
            .query_map(params![scope, acc, limit as i64], mapper)
            .map_err(|e| format!("query events 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    } else if let Some(since) = since_ms {
        let rows = stmt
            .query_map(params![scope, since, limit as i64], mapper)
            .map_err(|e| format!("query events 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    } else {
        let rows = stmt
            .query_map(params![scope, limit as i64], mapper)
            .map_err(|e| format!("query events 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// 任务规范与运行态 (Task Specs & Runs)
// ---------------------------------------------------------------------------

pub fn upsert_task_spec(conn: &Connection, spec: &TaskSpec) -> Result<(), String> {
    let metadata_str = spec.metadata.to_string();
    conn.execute(
        "INSERT INTO task_specs
         (id, scope, task_kind, account_scope, schedule_cron, timezone, enabled, retry_policy, metadata, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
            task_kind = excluded.task_kind,
            account_scope = excluded.account_scope,
            schedule_cron = excluded.schedule_cron,
            timezone = excluded.timezone,
            enabled = excluded.enabled,
            retry_policy = excluded.retry_policy,
            metadata = excluded.metadata,
            updated_at_ms = excluded.updated_at_ms",
        params![
            spec.id,
            spec.scope,
            spec.task_kind,
            spec.account_scope,
            spec.schedule_cron,
            spec.timezone,
            if spec.enabled { 1 } else { 0 },
            spec.retry_policy,
            metadata_str,
            spec.updated_at_ms
        ],
    )
    .map_err(|e| format!("upsert_task_spec 失败: {e}"))?;
    Ok(())
}

pub fn query_task_specs(conn: &Connection, scope: &str) -> Result<Vec<TaskSpec>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, scope, task_kind, account_scope, schedule_cron, timezone, enabled, retry_policy, metadata, updated_at_ms
             FROM task_specs WHERE scope = ?1",
        )
        .map_err(|e| format!("prepare query_task_specs 失败: {e}"))?;

    let rows = stmt
        .query_map(params![scope], |row| {
            let metadata_str: String = row.get(8)?;
            let metadata = serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);
            let id: String = row.get(0)?;
            let schedule_cron: Option<String> = row.get(4)?;
            let task_kind: String = row.get(2)?;

            let (name, desc, trigger_type, def_schedule) = match task_kind.as_str() {
                "checkin" => (
                    "每日自动签到",
                    "定时检查并执行各账号自动签到与领奖",
                    "cron",
                    schedule_cron
                        .clone()
                        .unwrap_or_else(|| "0 9 * * *".to_string()),
                ),
                "credits_sync" => (
                    "额度数据同步",
                    "定期从上游刷新积分、到期包与配额状态",
                    "cron",
                    schedule_cron
                        .clone()
                        .unwrap_or_else(|| "0 */4 * * *".to_string()),
                ),
                _ => (
                    "活动奖励观察",
                    "观察活动积分到账与额度变动",
                    if schedule_cron.is_some() {
                        "cron"
                    } else {
                        "manual"
                    },
                    schedule_cron
                        .clone()
                        .unwrap_or_else(|| "manual".to_string()),
                ),
            };

            Ok(TaskSpec {
                id: id.clone(),
                task_id: id,
                scope: row.get(1)?,
                name: name.to_string(),
                description: desc.to_string(),
                task_kind,
                trigger_type: trigger_type.to_string(),
                account_scope: row.get(3)?,
                schedule_cron,
                default_schedule: def_schedule,
                timezone: row.get(5)?,
                enabled: row.get::<_, i64>(6)? != 0,
                status: "idle".to_string(),
                last_run_at: None,
                next_run_at: None,
                retry_policy: row.get(7)?,
                metadata,
                updated_at_ms: row.get(9)?,
            })
        })
        .map_err(|e| format!("query_task_specs 失败: {e}"))?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

pub fn upsert_task_run(conn: &Connection, run: &TaskRun) -> Result<(), String> {
    conn.execute(
        "INSERT INTO task_runs
         (run_id, task_id, scope, task_kind, trigger_source, status, started_at_ms, finished_at_ms, total_items, success_items, failed_items, message, details_json, error, cursor_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(run_id) DO UPDATE SET
            task_id = excluded.task_id,
            status = excluded.status,
            finished_at_ms = excluded.finished_at_ms,
            total_items = excluded.total_items,
            success_items = excluded.success_items,
            failed_items = excluded.failed_items,
            message = excluded.message,
            details_json = excluded.details_json,
            error = excluded.error,
            cursor_id = excluded.cursor_id",
        params![
            run.run_id,
            run.task_id,
            run.scope,
            run.task_kind,
            run.trigger_source,
            run.status,
            run.started_at_ms,
            run.finished_at_ms,
            run.total_items as i64,
            run.success_items as i64,
            run.failed_items as i64,
            run.message,
            run.details_json,
            run.error,
            run.cursor,
        ],
    )
    .map_err(|e| format!("upsert_task_run 失败: {e}"))?;
    Ok(())
}

pub fn insert_task_run(conn: &Connection, run: &TaskRun) -> Result<(), String> {
    upsert_task_run(conn, run)
}

fn map_task_run_row(row: &rusqlite::Row) -> rusqlite::Result<TaskRun> {
    let run_id: String = row.get(0)?;
    let task_id: String = row
        .get::<_, Option<String>>(1)?
        .unwrap_or_else(|| "checkin_daily".to_string());
    let scope: String = row.get(2)?;
    let task_kind: String = row.get(3)?;
    let trigger_source: String = row.get(4)?;
    let status: String = row.get(5)?;
    let started_at_ms: i64 = row.get(6)?;
    let finished_at_ms: Option<i64> = row.get(7)?;
    let total_items: i64 = row.get::<_, Option<i64>>(8)?.unwrap_or(0);
    let success_items: i64 = row.get::<_, Option<i64>>(9)?.unwrap_or(0);
    let failed_items: i64 = row.get::<_, Option<i64>>(10)?.unwrap_or(0);
    let message: Option<String> = row.get(11)?;
    let details_json: Option<String> = row.get(12)?;
    let error: Option<String> = row.get(13)?;
    let cursor_id: Option<i64> = row.get(14)?;

    Ok(TaskRun {
        run_id,
        task_id,
        scope,
        task_kind,
        trigger_source: trigger_source.clone(),
        triggered_by: trigger_source,
        status,
        started_at_ms,
        started_at: started_at_ms / 1000,
        finished_at_ms,
        finished_at: finished_at_ms.map(|ms| ms / 1000),
        total_items: total_items as usize,
        success_items: success_items as usize,
        failed_items: failed_items as usize,
        message,
        details_json,
        error,
        cursor: cursor_id,
    })
}

pub fn query_task_runs(
    conn: &Connection,
    scope: &str,
    run_id: Option<&str>,
) -> Result<Vec<TaskRun>, String> {
    let sql = if run_id.is_some() {
        "SELECT run_id, task_id, scope, task_kind, trigger_source, status, started_at_ms, finished_at_ms, total_items, success_items, failed_items, message, details_json, error, cursor_id
         FROM task_runs WHERE scope = ?1 AND run_id = ?2
         ORDER BY started_at_ms DESC"
    } else {
        "SELECT run_id, task_id, scope, task_kind, trigger_source, status, started_at_ms, finished_at_ms, total_items, success_items, failed_items, message, details_json, error, cursor_id
         FROM task_runs WHERE scope = ?1
         ORDER BY started_at_ms DESC LIMIT 50"
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("prepare query_task_runs 失败: {e}"))?;

    let mut out = Vec::new();
    if let Some(rid) = run_id {
        let rows = stmt
            .query_map(params![scope, rid], map_task_run_row)
            .map_err(|e| format!("query task runs by id 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    } else {
        let rows = stmt
            .query_map(params![scope], map_task_run_row)
            .map_err(|e| format!("query task runs 失败: {e}"))?;
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
    }

    Ok(out)
}

pub fn query_active_task_run(
    conn: &Connection,
    scope: &str,
    task_id: &str,
) -> Result<Option<TaskRun>, String> {
    let sql = "SELECT run_id, task_id, scope, task_kind, trigger_source, status, started_at_ms, finished_at_ms, total_items, success_items, failed_items, message, details_json, error, cursor_id
         FROM task_runs
         WHERE scope = ?1 AND task_id = ?2 AND status IN ('queued', 'running', 'waiting_upstream', 'waiting_verification')
         ORDER BY started_at_ms DESC LIMIT 1";

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("prepare query_active_task_run 失败: {e}"))?;

    let mut rows = stmt
        .query_map(params![scope, task_id], map_task_run_row)
        .map_err(|e| format!("query active task run 失败: {e}"))?;

    if let Some(first) = rows.next() {
        let run = first.map_err(|e| e.to_string())?;
        // 开始时间不能证明工作进程已退出，更不能证明上游没有收到写请求。
        // 在恢复/只读复查实现前保留运行，避免重放结果未知的领取。
        Ok(Some(run))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// 逐账号任务执行项 (Task Items)
// ---------------------------------------------------------------------------

pub fn upsert_task_item(conn: &Connection, item: &TaskItem) -> Result<(), String> {
    let result_json = item.result_data.as_ref().map(|v| v.to_string());
    conn.execute(
        "INSERT INTO task_items
         (run_id, scope, account_id, stage, status, started_at_ms, finished_at_ms, result_data_json, error, retry_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(run_id, account_id) DO UPDATE SET
            stage = excluded.stage,
            status = excluded.status,
            finished_at_ms = excluded.finished_at_ms,
            result_data_json = excluded.result_data_json,
            error = excluded.error,
            retry_count = excluded.retry_count",
        params![
            item.run_id,
            item.scope,
            item.account_id,
            item.stage,
            item.status,
            item.started_at_ms,
            item.finished_at_ms,
            result_json,
            item.error,
            item.retry_count,
        ],
    )
    .map_err(|e| format!("upsert_task_item 失败: {e}"))?;
    Ok(())
}

pub fn query_task_items(conn: &Connection, run_id: &str) -> Result<Vec<TaskItem>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, run_id, scope, account_id, stage, status, started_at_ms, finished_at_ms, result_data_json, error, retry_count
             FROM task_items WHERE run_id = ?1 ORDER BY id ASC",
        )
        .map_err(|e| format!("prepare query_task_items 失败: {e}"))?;

    let rows = stmt
        .query_map(params![run_id], |row| {
            let result_json_str: Option<String> = row.get(8)?;
            let result_data = result_json_str.and_then(|s| serde_json::from_str(&s).ok());

            Ok(TaskItem {
                id: Some(row.get(0)?),
                run_id: row.get(1)?,
                scope: row.get(2)?,
                account_id: row.get(3)?,
                stage: row.get(4)?,
                status: row.get(5)?,
                started_at_ms: row.get(6)?,
                finished_at_ms: row.get(7)?,
                result_data,
                error: row.get(9)?,
                retry_count: row.get::<_, Option<i32>>(10)?.unwrap_or(0),
            })
        })
        .map_err(|e| format!("query_task_items 失败: {e}"))?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 跨进程账号锁 (Task Account Locks - Lease / Timeout / Owner)
// ---------------------------------------------------------------------------

pub fn acquire_account_lock(
    conn: &Connection,
    scope: &str,
    account_id: &str,
    task_id: &str,
    owner_id: &str,
    ttl_ms: i64,
    now_ms: i64,
) -> Result<bool, String> {
    let changed = conn.execute(
        "INSERT INTO task_account_locks (scope, account_id, task_id, owner_id, acquired_at_ms, expires_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(scope, account_id, task_id) DO UPDATE SET
           owner_id = excluded.owner_id, acquired_at_ms = excluded.acquired_at_ms,
           expires_at_ms = excluded.expires_at_ms
         WHERE task_account_locks.owner_id = excluded.owner_id OR task_account_locks.expires_at_ms <= ?5",
        params![scope, account_id, task_id, owner_id, now_ms, now_ms + ttl_ms],
    ).map_err(|e| format!("获取账号锁失败: {e}"))?;
    Ok(changed == 1)
}

pub fn release_account_lock(
    conn: &Connection,
    scope: &str,
    account_id: &str,
    task_id: &str,
    owner_id: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM task_account_locks WHERE scope = ?1 AND account_id = ?2 AND task_id = ?3 AND owner_id = ?4",
        params![scope, account_id, task_id, owner_id],
    )
    .map_err(|e| format!("释放账号锁失败: {e}"))?;
    Ok(())
}

pub fn release_owner_locks(conn: &Connection, owner_id: &str) -> Result<usize, String> {
    let count = conn
        .execute(
            "DELETE FROM task_account_locks WHERE owner_id = ?1",
            params![owner_id],
        )
        .map_err(|e| format!("批量释放所有者锁失败: {e}"))?;
    Ok(count)
}

pub fn is_account_locked(
    conn: &Connection,
    scope: &str,
    account_id: &str,
    task_id: &str,
    now_ms: i64,
) -> Result<bool, String> {
    let locked: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM task_account_locks WHERE scope = ?1 AND account_id = ?2 AND task_id = ?3 AND expires_at_ms > ?4",
            params![scope, account_id, task_id, now_ms],
            |row| row.get(0),
        )
        .unwrap_or(false);
    Ok(locked)
}

// ---------------------------------------------------------------------------
// 任务持久化事件 (Task Events - FUS-W06)
// ---------------------------------------------------------------------------

pub fn insert_task_event(
    conn: &Connection,
    run_id: &str,
    scope: &str,
    account_id: &str,
    stage: &str,
    status: &str,
    occurred_at_ms: i64,
    error_category: Option<&str>,
    message: Option<&str>,
) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO task_events
         (run_id, scope, account_id, stage, status, occurred_at_ms, error_category, message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            run_id,
            scope,
            account_id,
            stage,
            status,
            occurred_at_ms,
            error_category,
            message,
        ],
    )
    .map_err(|e| format!("写入 task_event 失败: {e}"))?;
    Ok(conn.last_insert_rowid())
}

pub fn query_task_events(
    conn: &Connection,
    scope: &str,
    run_id: Option<&str>,
    after_cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<TaskEventPage, String> {
    let bounded_limit = limit.unwrap_or(50).clamp(1, 200);
    let cursor = after_cursor.unwrap_or(0);

    // 严格 fail-closed：如果指定了 run_id，必须验证该 run 属于该 scope
    if let Some(rid) = run_id {
        let run_scope: Option<String> = conn
            .query_row(
                "SELECT scope FROM task_runs WHERE run_id = ?1",
                params![rid],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;

        match run_scope {
            Some(s) if s == scope => {}
            _ => return Err(format!("任务运行 {rid} 不属于作用域 {scope} 或不存在")),
        }
    }

    let fetch_limit = (bounded_limit + 1) as i64;
    let mapper = |row: &rusqlite::Row| {
        let id: i64 = row.get(0)?;
        Ok(TaskEvent {
            id,
            sequence: id,
            run_id: row.get(1)?,
            scope: row.get(2)?,
            account_id: row.get(3)?,
            stage: row.get(4)?,
            status: row.get(5)?,
            occurred_at_ms: row.get(6)?,
            error_category: row.get(7)?,
            message: row.get(8)?,
        })
    };

    let mut raw_items = Vec::new();
    if let Some(rid) = run_id {
        let mut stmt = conn
            .prepare(
                "SELECT id, run_id, scope, account_id, stage, status, occurred_at_ms, error_category, message
                 FROM task_events
                 WHERE scope = ?1 AND run_id = ?2 AND id > ?3
                 ORDER BY id ASC LIMIT ?4",
            )
            .map_err(|e| format!("prepare query_task_events by run_id 失败: {e}"))?;
        let rows = stmt
            .query_map(params![scope, rid, cursor, fetch_limit], mapper)
            .map_err(|e| format!("query task events by run_id 失败: {e}"))?;
        for r in rows {
            raw_items.push(r.map_err(|e| e.to_string())?);
        }
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT id, run_id, scope, account_id, stage, status, occurred_at_ms, error_category, message
                 FROM task_events
                 WHERE scope = ?1 AND id > ?2
                 ORDER BY id ASC LIMIT ?3",
            )
            .map_err(|e| format!("prepare query_task_events 失败: {e}"))?;
        let rows = stmt
            .query_map(params![scope, cursor, fetch_limit], mapper)
            .map_err(|e| format!("query task events 失败: {e}"))?;
        for r in rows {
            raw_items.push(r.map_err(|e| e.to_string())?);
        }
    }

    let has_more = raw_items.len() > bounded_limit;
    if has_more {
        raw_items.truncate(bounded_limit);
    }
    let next_cursor = raw_items.last().map(|e| e.id);

    Ok(TaskEventPage {
        items: raw_items,
        next_cursor,
        has_more,
    })
}
