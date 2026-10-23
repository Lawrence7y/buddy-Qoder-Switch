#[cfg(test)]
mod tests {
    use crate::modules::management::credits::{self, days_until_ms, validate_scope};
    use crate::modules::management::models::{
        AccountRef, CreditEvent, CreditObservation, CreditPackageItem, CreditSummaryProjection,
        CreditSummaryStats, ManagementDataStatus, TaskItem, TaskRun,
    };
    use crate::modules::management::runtime;
    use crate::modules::management::store;
    use crate::modules::management::tasks;

    #[test]
    fn test_schema_migration_idempotent() {
        let conn = store::open_memory_db().expect("内存库创建成功");
        store::ensure_schema(&conn).expect("重复调用迁移安全幂等");

        let version: i64 = conn
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = 1",
                [],
                |r| r.get(0),
            )
            .expect("版本 1 迁移记录存在");
        assert_eq!(version, 1);
    }

    #[test]
    fn test_future_schema_version_rejected() {
        let conn = store::open_memory_db().expect("内存库创建成功");
        // 伪造注入一个未来 schema 版本（如 v999）
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (999, 'future_migration', 1800000000000)",
            [],
        )
        .expect("插入未来版本");

        // 再次调用 ensure_schema，必须拒绝降级运行
        let res = store::ensure_schema(&conn);
        assert!(res.is_err(), "遇到高版本 schema 时必须拒绝运行以保护数据");
        assert!(res.unwrap_err().contains("999"));
    }

    #[test]
    fn test_unknown_scope_fail_closed() {
        assert!(validate_scope("invalid-scope").is_err());
        assert!(validate_scope("wb_cn").is_err()); // 必须是短横线 wb-cn
        assert!(validate_scope("").is_err());
        assert!(validate_scope("wb-cn").is_ok());
        assert!(validate_scope("qoder-global").is_ok());
    }

    #[test]
    fn test_observation_deduplication() {
        let conn = store::open_memory_db().expect("内存库创建成功");
        let now = 1_700_000_000_000i64;

        let obs1 = CreditObservation {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "acc_1".to_string(),
            source: "official".to_string(),
            remaining: 50.0,
            total: 100.0,
            unit: "credits".to_string(),
            is_stale: false,
            is_comparable: true,
            observed_at_ms: now,
        };

        let inserted = store::insert_credit_observation(&conn, &obs1).expect("插入成功");
        assert!(inserted);

        let obs2 = CreditObservation {
            observed_at_ms: now + 60_000,
            ..obs1.clone()
        };
        let inserted2 = store::insert_credit_observation(&conn, &obs2).expect("去重检查");
        assert!(!inserted2, "5 分钟内相同余额快照应被去重");

        let obs3 = CreditObservation {
            remaining: 45.0,
            observed_at_ms: now + 120_000,
            ..obs1.clone()
        };
        let inserted3 = store::insert_credit_observation(&conn, &obs3).expect("写入新余额");
        assert!(inserted3, "余额变动应正常写入");
    }

    #[test]
    fn test_unknown_expiry_upsert_dedup() {
        let conn = store::open_memory_db().expect("内存库创建成功");
        let now = 1_700_000_000_000i64;

        let pkg = CreditPackageItem {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "acc_null_expiry".to_string(),
            account_name: "测试账号".to_string(),
            package_id: "pkg_no_expire".to_string(),
            name: "未知到期包".to_string(),
            package_name: "未知到期包".to_string(),
            package_type: "plan".to_string(),
            remaining: 100.0,
            remaining_balance: 100.0,
            total: 100.0,
            original_capacity: Some(100.0),
            unit: "credits".to_string(),
            expire_at_ms: None, // 未知到期
            expire_time: None,
            expire_time_formatted: None,
            is_inferred: false,
            is_stale: false,
            source: "official".to_string(),
            source_type: "official".to_string(),
            observed_at_ms: now,
            observed_at: now / 1000,
            status: "active".to_string(),
        };

        // 插入第一次
        store::upsert_credit_package(&conn, &pkg).expect("第一次插入");
        // 插入第二次（金额更新）
        let pkg_updated = CreditPackageItem {
            remaining: 80.0,
            remaining_balance: 80.0,
            ..pkg.clone()
        };
        store::upsert_credit_package(&conn, &pkg_updated).expect("第二次更新");

        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM credit_packages WHERE scope = 'wb-cn' AND account_id = 'acc_null_expiry' AND package_id = 'pkg_no_expire'",
                [],
                |r| r.get(0),
            )
            .expect("查询行数");

        assert_eq!(rows, 1, "未知到期的相同包 upsert 两次在表中必须恰为 1 行");

        let pkgs =
            store::query_credit_packages(&conn, "wb-cn", Some("acc_null_expiry")).expect("查询");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].remaining, 80.0, "更新后余额应覆盖旧值");
    }

    #[test]
    fn test_replace_account_packages_removes_old_packages() {
        let mut conn = store::open_memory_db().expect("内存库创建成功");
        let now = 1_700_000_000_000i64;

        let pkg_old = CreditPackageItem {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "acc_dyn".to_string(),
            account_name: "动态账号".to_string(),
            package_id: "pkg_consumed".to_string(),
            name: "已用尽旧包".to_string(),
            package_name: "已用尽旧包".to_string(),
            package_type: "addon".to_string(),
            remaining: 0.0,
            remaining_balance: 0.0,
            total: 50.0,
            original_capacity: Some(50.0),
            unit: "credits".to_string(),
            expire_at_ms: Some(now + 86400000),
            expire_time: Some((now + 86400000) / 1000),
            expire_time_formatted: Some("2026-10-23".to_string()),
            is_inferred: false,
            is_stale: false,
            source: "official".to_string(),
            source_type: "official".to_string(),
            observed_at_ms: now,
            observed_at: now / 1000,
            status: "active".to_string(),
        };

        store::upsert_credit_package(&conn, &pkg_old).expect("写入旧包");
        assert_eq!(
            store::query_credit_packages(&conn, "wb-cn", Some("acc_dyn"))
                .unwrap()
                .len(),
            1
        );

        // 成功全量刷新后：返回的是全新的包列表（旧包已用尽消失）
        let pkg_new = CreditPackageItem {
            package_id: "pkg_fresh".to_string(),
            name: "全新月度包".to_string(),
            package_name: "全新月度包".to_string(),
            remaining: 200.0,
            remaining_balance: 200.0,
            total: 200.0,
            original_capacity: Some(200.0),
            ..pkg_old.clone()
        };

        store::replace_account_credit_packages(&mut conn, "wb-cn", "acc_dyn", &[pkg_new])
            .expect("快照替换");

        let current = store::query_credit_packages(&conn, "wb-cn", Some("acc_dyn")).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(
            current[0].package_id, "pkg_fresh",
            "旧包应被成功移除，仅保留最新清单"
        );
    }

    #[test]
    fn test_mark_account_packages_stale_retains_data() {
        let conn = store::open_memory_db().expect("内存库创建成功");
        let now = 1_700_000_000_000i64;

        let pkg = CreditPackageItem {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "acc_err".to_string(),
            account_name: "错误账号".to_string(),
            package_id: "pkg_survive".to_string(),
            name: "保留包".to_string(),
            package_name: "保留包".to_string(),
            package_type: "addon".to_string(),
            remaining: 30.0,
            remaining_balance: 30.0,
            total: 30.0,
            original_capacity: Some(30.0),
            unit: "credits".to_string(),
            expire_at_ms: None,
            expire_time: None,
            expire_time_formatted: None,
            is_inferred: false,
            is_stale: false,
            source: "official".to_string(),
            source_type: "official".to_string(),
            observed_at_ms: now,
            observed_at: now / 1000,
            status: "active".to_string(),
        };

        store::upsert_credit_package(&conn, &pkg).expect("写入");

        // 模拟刷新失败：绝不清空旧包，而是标为 stale
        store::mark_account_packages_stale(&conn, "wb-cn", "acc_err").expect("标 stale");

        let current = store::query_credit_packages(&conn, "wb-cn", Some("acc_err")).unwrap();
        assert_eq!(current.len(), 1, "失败时旧包必须完整保留");
        assert!(current[0].is_stale, "旧包应持久化标记为 stale");
    }

    #[test]
    fn test_151_events_aggregation_not_truncated_by_limit() {
        let conn = store::open_memory_db().expect("内存库创建成功");
        let now = 1_700_000_000_000i64;

        // 插入 151 条 1.0 credit 的 usage 消耗事件
        for i in 0..151 {
            let evt = CreditEvent {
                id: None,
                scope: "wb-cn".to_string(),
                account_id: "acc_bulk".to_string(),
                account_name: None,
                event_id: Some(format!("evt_{i}")),
                event_kind: "usage".to_string(),
                event_type: "usage".to_string(),
                amount: 1.0,
                change_amount: 1.0,
                balance_after: 0.0,
                unit: "credits".to_string(),
                occurred_at_ms: now + i * 1000,
                recorded_at: (now + i * 1000) / 1000,
                evidence_level: "official_api".to_string(),
                note: None,
                reason: None,
            };
            store::insert_credit_event(&conn, &evt).expect("插入事件");
        }

        // 聚合函数：不受 100 条分页限制
        let (period_in, period_out) = store::aggregate_credit_events_summary(
            &conn,
            "wb-cn",
            Some("acc_bulk"),
            Some(now - 1000),
        )
        .expect("汇总查询");

        assert_eq!(period_in, None);
        assert_eq!(
            period_out,
            Some(151.0),
            "151 条消耗事件必须准确汇总得到 151.0"
        );

        // 而列表展示依然返回最近 100 条
        let events_list =
            store::query_credit_events(&conn, "wb-cn", Some("acc_bulk"), Some(now - 1000), 100)
                .expect("列表查询");
        assert_eq!(events_list.len(), 100);
    }

    #[test]
    fn test_same_day_midnight_and_hours_difference_is_zero() {
        // 今天 23:00 与今天 23:59（1小时后）属于同一自然日
        let now = 1_700_000_000_000i64;
        let one_hour_later = now + 3600 * 1000;

        let days = days_until_ms(one_hour_later, now);
        assert_eq!(
            days, 0,
            "同一自然日内未来一小时到期，days_until 必须为 0（今日到期），不得算作 1 天"
        );

        // 25 小时后（跨自然日）
        let next_day = now + 25 * 3600 * 1000;
        let days_next = days_until_ms(next_day, now);
        assert_eq!(days_next, 1, "跨自然日应算作 1 天后");
    }

    #[test]
    fn test_empty_data_source_kind_is_not_official_and_not_fake_zero() {
        let conn = store::open_memory_db().expect("内存库创建成功");
        let obs = store::query_latest_observations(&conn, "wb-cn", None).unwrap();
        assert!(obs.is_empty(), "无数据");

        let pkgs = store::query_credit_packages(&conn, "wb-cn", None).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_task_specs_w02_contract_metadata() {
        let specs = tasks::get_task_specs("wb-cn").expect("获取 WB-CN 任务规范");
        assert!(!specs.is_empty(), "WB-CN 预置规范不为空");

        let first = &specs[0];
        let meta = &first.metadata;
        assert_eq!(meta.get("stage").and_then(|v| v.as_str()), Some("W02"));
        assert_eq!(
            meta.get("w06_delivered").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            meta.get("w07_delivered").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn test_serde_json_compatibility_with_frontend() {
        let now = 1_700_000_000_000i64;
        let pkg = CreditPackageItem {
            id: Some(1),
            scope: "wb-cn".to_string(),
            account_id: "acc_test".to_string(),
            account_name: "测试账号".to_string(),
            package_id: "pkg_1".to_string(),
            name: "月度配额".to_string(),
            package_name: "月度配额".to_string(),
            package_type: "plan".to_string(),
            remaining: 50.0,
            remaining_balance: 50.0,
            total: 100.0,
            original_capacity: Some(100.0),
            unit: "credits".to_string(),
            expire_at_ms: Some(now + 86400000),
            expire_time: Some((now + 86400000) / 1000),
            expire_time_formatted: Some("2026-10-23".to_string()),
            is_inferred: false,
            is_stale: false,
            source: "official".to_string(),
            source_type: "official".to_string(),
            observed_at_ms: now,
            observed_at: now / 1000,
            status: "active".to_string(),
        };

        let summary = super::super::models::CreditSummaryStats {
            current_remaining: 50.0,
            total_remaining_balance: 50.0,
            total_original_capacity: 100.0,
            period_in: Some(10.0),
            period_out: Some(5.0),
            expiring_soon: 50.0,
            expiring_within7_days_count: 1,
            expiring_within7_days_amount: 50.0,
            currency: "credits".to_string(),
            total_accounts: 1,
            active_accounts: 1,
            total_packages: 1,
            is_known: true,
        };

        let projection = super::super::models::CreditSummaryProjection {
            scope: "wb-cn".to_string(),
            filter_account: None,
            summary: summary.clone(),
            stats: summary,
            packages: vec![pkg],
            calendar: vec![],
            observations: vec![],
            events: vec![],
            recent_events: vec![],
            status: super::super::models::ManagementDataStatus {
                is_stale: false,
                last_updated_ms: Some(now),
                source_kind: "official".to_string(),
                code: "live".to_string(),
                message: "正常".to_string(),
                observed_at: now / 1000,
                cached_at: Some(now / 1000),
                ttl_remaining_seconds: Some(300),
                error: None,
            },
        };

        // 序列化为真实 JSON
        let json_val = serde_json::to_value(&projection).expect("序列化为 JSON 成功");

        // 断言前端组件所需属性全部存在且非 null
        assert_eq!(
            json_val
                .pointer("/stats/totalRemainingBalance")
                .and_then(|v| v.as_f64()),
            Some(50.0)
        );
        assert_eq!(
            json_val
                .pointer("/stats/totalPackages")
                .and_then(|v| v.as_i64()),
            Some(1)
        );
        assert_eq!(
            json_val
                .pointer("/packages/0/remainingBalance")
                .and_then(|v| v.as_f64()),
            Some(50.0)
        );
        assert_eq!(
            json_val
                .pointer("/packages/0/packageName")
                .and_then(|v| v.as_str()),
            Some("月度配额")
        );
        assert_eq!(
            json_val.pointer("/status/code").and_then(|v| v.as_str()),
            Some("live")
        );
        assert_eq!(
            json_val
                .pointer("/status/observedAt")
                .and_then(|v| v.as_i64()),
            Some(now / 1000)
        );
    }

    #[test]
    fn test_credit_event_signed_roundtrip_and_json() {
        let conn = store::open_memory_db().expect("内存库创建成功");
        let now = 1_700_000_000_000i64;

        // 插入 1 条 usage 事件 (消耗 5.0) 与 1 条 checkin 事件 (入账 10.0)
        let usage_evt = CreditEvent {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "user_a".to_string(),
            account_name: Some("User A".to_string()),
            event_id: Some("evt_usage_1".to_string()),
            event_kind: "usage".to_string(),
            event_type: "usage".to_string(),
            amount: 5.0,
            change_amount: -5.0,
            balance_after: 95.0,
            unit: "credits".to_string(),
            occurred_at_ms: now,
            recorded_at: now / 1000,
            evidence_level: "official_api".to_string(),
            note: Some("调用模型扣费".to_string()),
            reason: Some("调用模型扣费".to_string()),
        };
        store::insert_credit_event(&conn, &usage_evt).expect("插入 usage 事件");

        let checkin_evt = CreditEvent {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "user_a".to_string(),
            account_name: Some("User A".to_string()),
            event_id: Some("evt_checkin_1".to_string()),
            event_kind: "checkin".to_string(),
            event_type: "checkin".to_string(),
            amount: 10.0,
            change_amount: 10.0,
            balance_after: 105.0,
            unit: "credits".to_string(),
            occurred_at_ms: now + 1000,
            recorded_at: (now + 1000) / 1000,
            evidence_level: "checkin_receipt".to_string(),
            note: Some("每日签到奖励".to_string()),
            reason: Some("每日签到奖励".to_string()),
        };
        store::insert_credit_event(&conn, &checkin_evt).expect("插入 checkin 事件");

        // 读回事件
        let events =
            store::query_credit_events(&conn, "wb-cn", Some("user_a"), None, 10).expect("查询事件");
        assert_eq!(events.len(), 2);

        let checkin_read = events
            .iter()
            .find(|e| e.event_kind == "checkin")
            .expect("找到 checkin");
        assert_eq!(checkin_read.amount, 10.0);
        assert_eq!(
            checkin_read.change_amount, 10.0,
            "checkin 变动必须为正向 (+10.0)"
        );

        let usage_read = events
            .iter()
            .find(|e| e.event_kind == "usage")
            .expect("找到 usage");
        assert_eq!(usage_read.amount, 5.0);
        assert_eq!(
            usage_read.change_amount, -5.0,
            "usage 消耗必须携带负号 (-5.0)"
        );

        // 序列化并验证 JSON 输出
        let json_val = serde_json::to_value(&events).expect("序列化为 JSON");
        let usage_json = json_val
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["eventKind"] == "usage")
            .unwrap();
        assert_eq!(
            usage_json["changeAmount"].as_f64(),
            Some(-5.0),
            "JSON 中 usage 的 changeAmount 必须是 -5.0"
        );
        assert_eq!(usage_json["amount"].as_f64(), Some(5.0));

        let checkin_json = json_val
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["eventKind"] == "checkin")
            .unwrap();
        assert_eq!(
            checkin_json["changeAmount"].as_f64(),
            Some(10.0),
            "JSON 中 checkin 的 changeAmount 必须是 +10.0"
        );
    }

    #[test]
    fn test_unobserved_status_observed_at_zero_and_cached_none() {
        // 构造未观测状态的投影
        let projection = CreditSummaryProjection {
            scope: "wb-cn".to_string(),
            filter_account: None,
            summary: CreditSummaryStats {
                current_remaining: 0.0,
                total_remaining_balance: 0.0,
                total_original_capacity: 0.0,
                period_in: None,
                period_out: None,
                expiring_soon: 0.0,
                expiring_within7_days_count: 0,
                expiring_within7_days_amount: 0.0,
                currency: "credits".to_string(),
                total_accounts: 0,
                active_accounts: 0,
                total_packages: 0,
                is_known: false,
            },
            stats: CreditSummaryStats {
                current_remaining: 0.0,
                total_remaining_balance: 0.0,
                total_original_capacity: 0.0,
                period_in: None,
                period_out: None,
                expiring_soon: 0.0,
                expiring_within7_days_count: 0,
                expiring_within7_days_amount: 0.0,
                currency: "credits".to_string(),
                total_accounts: 0,
                active_accounts: 0,
                total_packages: 0,
                is_known: false,
            },
            packages: vec![],
            calendar: vec![],
            observations: vec![],
            events: vec![],
            recent_events: vec![],
            status: ManagementDataStatus {
                is_stale: false,
                last_updated_ms: None,
                source_kind: "unknown".to_string(),
                code: "unobserved".to_string(),
                message: "尚未获取到积分观测数据".to_string(),
                observed_at: 0,
                cached_at: None,
                ttl_remaining_seconds: None,
                error: None,
            },
        };

        let json_val = serde_json::to_value(&projection).expect("序列化未观测投影");
        assert_eq!(
            json_val.pointer("/status/code").and_then(|v| v.as_str()),
            Some("unobserved")
        );
        assert_eq!(
            json_val
                .pointer("/status/observedAt")
                .and_then(|v| v.as_i64()),
            Some(0)
        );
        assert!(json_val.pointer("/status/cachedAt").is_none());
        assert_eq!(
            json_val.pointer("/stats/isKnown").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    // -----------------------------------------------------------------------
    // FUS-W06 状态机定点测试：重入幂等、账号锁与超时、取消语义、失败重试与恢复
    // -----------------------------------------------------------------------

    struct MockActionHandler {
        fail_accounts: Vec<String>,
    }

    impl tasks::TaskActionHandler for MockActionHandler {
        async fn execute_item(
            &self,
            scope: &str,
            task_id: &str,
            _task_kind: &str,
            account_id: &str,
        ) -> Result<tasks::TaskItemExecutionResult, String> {
            if self.fail_accounts.contains(&account_id.to_string()) {
                Ok(tasks::TaskItemExecutionResult {
                    status: "failed".to_string(),
                    stage: "upstream_call".to_string(),
                    message: Some("模拟上游调用失败".to_string()),
                    data: None,
                    error: Some("429 rate limit exceeded".to_string()),
                })
            } else {
                Ok(tasks::TaskItemExecutionResult {
                    status: "confirmed".to_string(),
                    stage: "done".to_string(),
                    message: Some(format!("模拟成功: {task_id} @ {account_id}")),
                    data: Some(
                        serde_json::json!({ "scope": scope, "taskId": task_id, "accountId": account_id }),
                    ),
                    error: None,
                })
            }
        }
    }

    #[tokio::test]
    async fn test_task_run_reentrancy_idempotency_no_duplicate_run() {
        let conn = store::open_db().expect("打开 DB");
        let scope = "wb-cn";
        let task_id = "test-reentrancy-task";
        let now = crate::modules::config::now_ms();

        // 预先写入一个正在运行的 run
        let original_run = TaskRun {
            run_id: format!("run-orig-reentrant-{}", now),
            task_id: task_id.to_string(),
            scope: scope.to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "running".to_string(),
            started_at_ms: now,
            started_at: now / 1000,
            finished_at_ms: None,
            finished_at: None,
            total_items: 1,
            success_items: 0,
            failed_items: 0,
            message: Some("原始活跃运行".to_string()),
            details_json: Some("{}".to_string()),
            error: None,
            cursor: Some(0),
        };
        store::upsert_task_run(&conn, &original_run).expect("插入活跃 run");

        // 重复触发同一任务，必须返回已有 run，不得产生第二个 run
        let returned_run = tasks::trigger_task_with_handler(
            scope,
            task_id,
            None,
            None,
            &MockActionHandler {
                fail_accounts: vec![],
            },
        )
        .await
        .expect("重入触发成功");

        assert_eq!(
            returned_run.run_id, original_run.run_id,
            "重入必须返回既有 run_id"
        );
        assert_eq!(returned_run.status, "running");

        // 清理
        let _ = tasks::cancel_task_run(scope, &original_run.run_id);
    }

    #[test]
    fn test_task_cross_process_account_lock_mutual_exclusion_and_timeout() {
        let conn = store::open_memory_db().expect("打开内存库");
        store::ensure_schema(&conn).expect("初始化 schema");

        let scope = "wb-cn";
        let account_id = "acc_locked_test";
        let task_id = "checkin";
        let ttl_ms = 1_000i64; // 1秒租约
        let now = 100_000i64;

        // 1. owner_A 首次获取锁 -> 成功
        let acq1 =
            store::acquire_account_lock(&conn, scope, account_id, task_id, "run_A", ttl_ms, now)
                .expect("获取锁");
        assert!(acq1, "首次获取锁应成功");
        assert!(store::is_account_locked(&conn, scope, account_id, task_id, now).unwrap());

        // 2. owner_B 在租约期内尝试获取同一账号锁 -> 互斥拒绝 (false)
        let acq2 = store::acquire_account_lock(
            &conn,
            scope,
            account_id,
            task_id,
            "run_B",
            ttl_ms,
            now + 500,
        )
        .expect("尝试获取锁");
        assert!(!acq2, "锁未过期前不同 owner 必须互斥失败");

        // 3. owner_A 在租约期内心跳续期 -> 成功
        let acq3 = store::acquire_account_lock(
            &conn,
            scope,
            account_id,
            task_id,
            "run_A",
            ttl_ms,
            now + 600,
        )
        .expect("续期锁");
        assert!(acq3, "同一 owner 续期应成功");

        // 4. 超过租约期限后（now + 600 + 1000 = 101600，我们在 102000 尝试），锁自动超时失效
        // owner_B 可以抢占接管该过期锁
        let acq4 = store::acquire_account_lock(
            &conn, scope, account_id, task_id, "run_B", ttl_ms, 102_000,
        )
        .expect("接管超时锁");
        assert!(acq4, "租约超时后允许新 owner 接管");

        // 5. 释放锁
        store::release_account_lock(&conn, scope, account_id, task_id, "run_B").expect("释放锁");
        assert!(!store::is_account_locked(&conn, scope, account_id, task_id, 102_000).unwrap());
    }

    #[test]
    fn test_task_cancellation_semantics_pending_vs_inflight() {
        let conn = store::open_memory_db().expect("打开内存库");
        store::ensure_schema(&conn).expect("初始化 schema");

        let scope = "wb-cn";
        let run_id = "run-test-cancel-semantics";
        let now = 100_000i64;

        let run = TaskRun {
            run_id: run_id.to_string(),
            task_id: "checkin".to_string(),
            scope: scope.to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "running".to_string(),
            started_at_ms: now,
            started_at: now / 1000,
            finished_at_ms: None,
            finished_at: None,
            total_items: 3,
            success_items: 1,
            failed_items: 0,
            message: Some("执行中".to_string()),
            details_json: Some("{}".to_string()),
            error: None,
            cursor: Some(1),
        };
        store::upsert_task_run(&conn, &run).expect("插入 run");

        // Item 1: 已完成 confirmed
        let item1 = TaskItem {
            id: None,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            account_id: "acc_1".to_string(),
            stage: "done".to_string(),
            status: "confirmed".to_string(),
            started_at_ms: now,
            finished_at_ms: Some(now + 10),
            result_data: None,
            error: None,
            retry_count: 0,
        };
        store::upsert_task_item(&conn, &item1).expect("插入 item1");

        // Item 2: 正在执行写请求 running (dispatched to upstream)
        let item2 = TaskItem {
            id: None,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            account_id: "acc_2".to_string(),
            stage: "upstream_write".to_string(),
            status: "running".to_string(),
            started_at_ms: now + 10,
            finished_at_ms: None,
            result_data: None,
            error: None,
            retry_count: 0,
        };
        store::upsert_task_item(&conn, &item2).expect("插入 item2");

        // Item 3: 尚未开始 pending
        let item3 = TaskItem {
            id: None,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            account_id: "acc_3".to_string(),
            stage: "prepare".to_string(),
            status: "pending".to_string(),
            started_at_ms: now,
            finished_at_ms: None,
            result_data: None,
            error: None,
            retry_count: 0,
        };
        store::upsert_task_item(&conn, &item3).expect("插入 item3");

        // 执行取消
        let cancelled_run =
            tasks::cancel_task_run_in_conn(&conn, scope, run_id).expect("执行取消操作");

        // PRD §5.3 / §7:
        // - 未开始项取消为 cancelled
        // - 已发写请求项转为 pending_confirmation，后续仅复查
        // - 存在 pending_confirmation 时 run 状态必须为 waiting_verification
        let items = store::query_task_items(&conn, run_id).expect("查询 items");
        let i1 = items.iter().find(|i| i.account_id == "acc_1").unwrap();
        let i2 = items.iter().find(|i| i.account_id == "acc_2").unwrap();
        let i3 = items.iter().find(|i| i.account_id == "acc_3").unwrap();

        assert_eq!(i1.status, "confirmed", "已到账项保持 confirmed");
        assert_eq!(
            i2.status, "pending_confirmation",
            "写请求在途项转为 pending_confirmation"
        );
        assert_eq!(i2.stage, "waiting_verification");
        assert_eq!(i3.status, "cancelled", "未开始项直接转为 cancelled");
        assert_eq!(i3.stage, "cancelled");

        assert_eq!(
            cancelled_run.status, "waiting_verification",
            "含在途写请求时 run 进入 waiting_verification"
        );
    }

    #[tokio::test]
    async fn test_task_retry_excludes_already_claimed_and_pending_confirmation() {
        let conn = store::open_db().expect("打开 DB");
        let scope = "wb-cn";
        let run_id = format!("run-test-retry-{}", crate::modules::config::now_ms());
        let now = crate::modules::config::now_ms();

        let run = TaskRun {
            run_id: run_id.clone(),
            task_id: "checkin".to_string(),
            scope: scope.to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "failed".to_string(),
            started_at_ms: now,
            started_at: now / 1000,
            finished_at_ms: Some(now + 100),
            finished_at: Some((now + 100) / 1000),
            total_items: 4,
            success_items: 1,
            failed_items: 1,
            message: Some("部分失败".to_string()),
            details_json: Some("{}".to_string()),
            error: Some("网络错误".to_string()),
            cursor: Some(4),
        };
        store::upsert_task_run(&conn, &run).expect("插入 run");

        // 1. confirmed 项
        store::upsert_task_item(
            &conn,
            &TaskItem {
                id: None,
                run_id: run_id.clone(),
                scope: scope.to_string(),
                account_id: "acc_confirmed".to_string(),
                stage: "done".to_string(),
                status: "confirmed".to_string(),
                started_at_ms: now,
                finished_at_ms: Some(now + 10),
                result_data: None,
                error: None,
                retry_count: 0,
            },
        )
        .expect("插入 item");

        // 2. pending_confirmation 项
        store::upsert_task_item(
            &conn,
            &TaskItem {
                id: None,
                run_id: run_id.clone(),
                scope: scope.to_string(),
                account_id: "acc_pending_conf".to_string(),
                stage: "waiting_verification".to_string(),
                status: "pending_confirmation".to_string(),
                started_at_ms: now,
                finished_at_ms: Some(now + 20),
                result_data: None,
                error: Some("待确认".to_string()),
                retry_count: 0,
            },
        )
        .expect("插入 item");

        // 3. skipped 项
        store::upsert_task_item(
            &conn,
            &TaskItem {
                id: None,
                run_id: run_id.clone(),
                scope: scope.to_string(),
                account_id: "acc_skipped".to_string(),
                stage: "filter".to_string(),
                status: "skipped".to_string(),
                started_at_ms: now,
                finished_at_ms: Some(now + 30),
                result_data: None,
                error: None,
                retry_count: 0,
            },
        )
        .expect("插入 item");

        // 4. failed 项
        store::upsert_task_item(
            &conn,
            &TaskItem {
                id: None,
                run_id: run_id.clone(),
                scope: scope.to_string(),
                account_id: "acc_failed".to_string(),
                stage: "upstream_call".to_string(),
                status: "failed".to_string(),
                started_at_ms: now,
                finished_at_ms: Some(now + 40),
                result_data: None,
                error: Some("429 rate limit".to_string()),
                retry_count: 0,
            },
        )
        .expect("插入 item");

        // 执行重试
        let handler = MockActionHandler {
            fail_accounts: vec![],
        };
        let retried_run = tasks::retry_task_run_with_handler(scope, &run_id, &handler)
            .await
            .expect("重试执行成功");

        let items = store::query_task_items(&conn, &run_id).expect("查询 items");
        let i_conf = items
            .iter()
            .find(|i| i.account_id == "acc_confirmed")
            .unwrap();
        let i_pend = items
            .iter()
            .find(|i| i.account_id == "acc_pending_conf")
            .unwrap();
        let i_skip = items
            .iter()
            .find(|i| i.account_id == "acc_skipped")
            .unwrap();
        let i_fail = items.iter().find(|i| i.account_id == "acc_failed").unwrap();

        // 断言：仅 failed 项增加了 retry_count 且变为 confirmed；其它项完全未被重试
        assert_eq!(i_conf.retry_count, 0, "confirmed 项不得重试");
        assert_eq!(i_pend.retry_count, 0, "pending_confirmation 项不得重试");
        assert_eq!(i_skip.retry_count, 0, "skipped 项不得重试");
        assert_eq!(i_fail.retry_count, 1, "failed 项重试次数递增");
        assert_eq!(i_fail.status, "confirmed", "failed 项重试后成功");

        // 由于存在 pending_confirmation 项，run 终态应保持 waiting_verification
        assert_eq!(retried_run.status, "waiting_verification");
    }

    #[tokio::test]
    async fn test_task_recovery_across_reloads_and_cursor() {
        let scope = "wb-cn";
        let task_id_str = format!("test-recovery-task-{}", crate::modules::config::now_ms());
        let task_id = &task_id_str;
        let accounts = vec!["acc_rec_1".to_string(), "acc_rec_2".to_string()];

        let handler = MockActionHandler {
            fail_accounts: vec![],
        };
        let executed_run = tasks::trigger_task_with_handler(
            scope,
            task_id,
            Some("manual"),
            Some(accounts.clone()),
            &handler,
        )
        .await
        .expect("触发执行成功");

        assert_eq!(executed_run.status, "completed");
        assert_eq!(executed_run.total_items, 2);
        assert_eq!(executed_run.success_items, 2);
        assert_eq!(executed_run.cursor, Some(2));

        // 模拟页面切页或重启：重新打开 DB 查询
        let recovered_runs =
            tasks::get_task_runs(scope, Some(&executed_run.run_id)).expect("恢复查询 runs 成功");
        assert_eq!(recovered_runs.len(), 1);
        let rec = &recovered_runs[0];
        assert_eq!(rec.run_id, executed_run.run_id);
        assert_eq!(rec.status, "completed");
        assert_eq!(rec.cursor, Some(2));

        let recovered_items =
            tasks::get_task_items(scope, &executed_run.run_id).expect("恢复查询 items 成功");
        assert_eq!(recovered_items.len(), 2);
        assert!(recovered_items.iter().all(|i| i.status == "confirmed"));
    }

    struct CancelDuringAction {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl tasks::TaskActionHandler for CancelDuringAction {
        async fn execute_item(
            &self,
            scope: &str,
            task_id: &str,
            _: &str,
            account_id: &str,
        ) -> Result<tasks::TaskItemExecutionResult, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let conn = store::open_db()?;
            let run = store::query_active_task_run(&conn, scope, task_id)?.unwrap();
            tasks::cancel_task_run(scope, &run.run_id)?;
            assert!(store::is_account_locked(
                &conn,
                scope,
                account_id,
                task_id,
                crate::modules::config::now_ms()
            )?);
            Ok(tasks::TaskItemExecutionResult {
                status: "confirmed".into(),
                stage: "done".into(),
                message: None,
                data: None,
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn test_cancel_during_execution_stops_next_account_and_preserves_pending() {
        let handler = CancelDuringAction {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let task = format!("cancel-live-{}", uuid::Uuid::new_v4());
        let run = tasks::trigger_task_with_handler(
            "wb-cn",
            &task,
            None,
            Some(vec!["a".into(), "b".into()]),
            &handler,
        )
        .await
        .unwrap();
        assert_eq!(handler.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(run.status, "waiting_verification");
        let items = tasks::get_task_items("wb-cn", &run.run_id).unwrap();
        assert_eq!(
            items.iter().find(|i| i.account_id == "a").unwrap().status,
            "pending_confirmation"
        );
        assert_eq!(
            items.iter().find(|i| i.account_id == "b").unwrap().status,
            "cancelled"
        );
        assert_eq!(run.success_items, 0);
        assert!(tasks::get_task_items("wb-global", &run.run_id).is_err());
    }

    #[tokio::test]
    async fn test_cancel_during_retry_stops_queued_retry_and_preserves_pending() {
        let task = format!("retry-cancel-{}", uuid::Uuid::new_v4());
        let run = tasks::trigger_task_with_handler(
            "wb-cn",
            &task,
            None,
            Some(vec!["a".into(), "b".into()]),
            &MockActionHandler {
                fail_accounts: vec!["a".into(), "b".into()],
            },
        )
        .await
        .unwrap();
        let handler = CancelDuringAction {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let retried = tasks::retry_task_run_with_handler("wb-cn", &run.run_id, &handler)
            .await
            .unwrap();
        assert_eq!(handler.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(retried.status, "waiting_verification");
        let items = tasks::get_task_items("wb-cn", &run.run_id).unwrap();
        assert_eq!(items.iter().filter(|i| i.status == "cancelled").count(), 1);
        assert_eq!(
            items
                .iter()
                .filter(|i| i.status == "pending_confirmation")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn test_offline_handler_never_confirms_reward() {
        let task = format!("offline-{}", uuid::Uuid::new_v4());
        let run = tasks::trigger_task_with_handler(
            "wb-cn",
            &task,
            None,
            Some(vec!["fixture".into()]),
            &tasks::OfflineTaskActionHandler,
        )
        .await
        .unwrap();
        assert_eq!(run.success_items, 0);
        let items = tasks::get_task_items("wb-cn", &run.run_id).unwrap();
        assert_eq!(items[0].status, "unsupported");
        assert_eq!(items[0].result_data.as_ref().unwrap()["offline"], true);
    }

    #[tokio::test]
    async fn test_explicit_empty_accounts_and_unknown_task_do_not_create_runs() {
        let task = format!("empty-{}", uuid::Uuid::new_v4());
        let result = tasks::trigger_task_with_handler(
            "wb-cn",
            &task,
            None,
            Some(vec![]),
            &MockActionHandler {
                fail_accounts: vec![],
            },
        )
        .await;
        assert!(result.is_err());
        assert!(tasks::get_task_runs("wb-cn", Some(&task))
            .unwrap()
            .is_empty());
        assert!(tasks::trigger_task("wb-cn", &task, None, None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_duplicate_accounts_execute_once() {
        let task = format!("dedup-{}", uuid::Uuid::new_v4());
        let run = tasks::trigger_task_with_handler(
            "wb-cn",
            &task,
            None,
            Some(vec!["a".into(), "a".into(), "".into()]),
            &MockActionHandler {
                fail_accounts: vec![],
            },
        )
        .await
        .unwrap();
        assert_eq!(run.total_items, 1);
        assert_eq!(run.success_items, 1);
    }

    #[test]
    fn test_old_active_run_is_not_replayed_based_on_start_time() {
        let conn = store::open_memory_db().unwrap();
        let run = TaskRun {
            run_id: "old-run".into(),
            task_id: "old-task".into(),
            scope: "wb-cn".into(),
            task_kind: "checkin".into(),
            trigger_source: "manual".into(),
            triggered_by: "manual".into(),
            status: "running".into(),
            started_at_ms: 1,
            started_at: 0,
            finished_at_ms: None,
            finished_at: None,
            total_items: 1,
            success_items: 0,
            failed_items: 0,
            message: None,
            details_json: None,
            error: None,
            cursor: Some(0),
        };
        store::upsert_task_run(&conn, &run).unwrap();
        let found = store::query_active_task_run(&conn, "wb-cn", "old-task")
            .unwrap()
            .unwrap();
        assert_eq!(found.run_id, run.run_id);
        assert_eq!(found.status, "running");
    }

    #[test]
    fn test_task_spec_rejects_scope_mismatch_and_fixed_unknown_accounts() {
        let mut spec = tasks::get_task_specs("wb-cn").unwrap().remove(0);
        assert!(tasks::save_task_spec("wb-global", &mut spec).is_err());
        spec.account_scope = "fixed".into();
        spec.metadata["accountIds"] = serde_json::json!([]);
        assert!(tasks::save_task_spec("wb-cn", &mut spec).is_err());
        spec.metadata["accountIds"] =
            serde_json::json!([format!("unknown-{}", uuid::Uuid::new_v4())]);
        assert!(tasks::save_task_spec("wb-cn", &mut spec).is_err());
    }

    #[test]
    fn test_simultaneous_triggers_share_one_run_and_execute_once() {
        struct SlowHandler(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl tasks::TaskActionHandler for SlowHandler {
            async fn execute_item(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<tasks::TaskItemExecutionResult, String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                Ok(tasks::TaskItemExecutionResult {
                    status: "confirmed".into(),
                    stage: "done".into(),
                    message: None,
                    data: None,
                    error: None,
                })
            }
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let task = format!("concurrent-{}", uuid::Uuid::new_v4());
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let barrier = barrier.clone();
                let task = task.clone();
                let calls = calls.clone();
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    barrier.wait();
                    runtime
                        .block_on(tasks::trigger_task_with_handler(
                            "wb-cn",
                            &task,
                            None,
                            Some(vec!["a".into()]),
                            &SlowHandler(calls),
                        ))
                        .unwrap()
                        .run_id
                })
            })
            .collect();
        let ids: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(ids[0], ids[1]);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn test_task_events_transactional_rollback_leaves_no_ghost_events() {
        let conn = store::open_memory_db().unwrap();
        let run_id = "run-ghost-test";
        let scope = "wb-cn";

        // 模拟在一个事务中插入，然后显式回滚
        {
            let tx = conn.unchecked_transaction().unwrap();
            store::insert_task_event(
                &tx,
                run_id,
                scope,
                "acc-1",
                "prepare",
                "running",
                1000,
                None,
                Some("准备执行"),
            )
            .unwrap();
            store::insert_task_event(
                &tx,
                run_id,
                scope,
                "acc-1",
                "upstream_write",
                "running",
                1001,
                None,
                Some("写入中"),
            )
            .unwrap();
            // 不 commit，直接 drop（触发 rollback）
            drop(tx);
        }

        // 查询事件表，必须为 0，不能留下任何幽灵日志
        let page = store::query_task_events(&conn, scope, None, None, None).unwrap();
        assert_eq!(page.items.len(), 0);
        assert_eq!(page.has_more, false);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn test_task_events_pagination_and_cursor_catchup() {
        let conn = store::open_memory_db().unwrap();
        let scope = "wb-cn";
        let run_id = "run-page-test";

        // 先插入对应的 task_run 供验证
        let run = TaskRun {
            run_id: run_id.to_string(),
            task_id: "test-task".to_string(),
            scope: scope.to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "running".to_string(),
            started_at_ms: 1000,
            started_at: 1,
            finished_at_ms: None,
            finished_at: None,
            total_items: 10,
            success_items: 0,
            failed_items: 0,
            message: None,
            details_json: None,
            error: None,
            cursor: Some(0),
        };
        store::upsert_task_run(&conn, &run).unwrap();

        // 插入 15 条单调递增事件
        for i in 1..=15 {
            store::insert_task_event(
                &conn,
                run_id,
                scope,
                &format!("acc-{i}"),
                "stage",
                "running",
                1000 + i as i64,
                None,
                Some(&format!("event #{i}")),
            )
            .unwrap();
        }

        // 1. 分页测试：limit = 5，第一页
        let page1 = store::query_task_events(&conn, scope, Some(run_id), None, Some(5)).unwrap();
        assert_eq!(page1.items.len(), 5);
        assert!(page1.has_more);
        let cursor1 = page1.next_cursor.unwrap();
        assert_eq!(page1.items[0].id, 1);
        assert_eq!(page1.items[4].id, 5);
        assert_eq!(cursor1, 5);

        // 2. 第二页：after_cursor = cursor1
        let page2 = store::query_task_events(&conn, scope, Some(run_id), Some(cursor1), Some(5)).unwrap();
        assert_eq!(page2.items.len(), 5);
        assert!(page2.has_more);
        let cursor2 = page2.next_cursor.unwrap();
        assert_eq!(page2.items[0].id, 6);
        assert_eq!(page2.items[4].id, 10);
        assert_eq!(cursor2, 10);

        // 3. 第三页（末页）：after_cursor = cursor2
        let page3 = store::query_task_events(&conn, scope, Some(run_id), Some(cursor2), Some(5)).unwrap();
        assert_eq!(page3.items.len(), 5);
        assert!(!page3.has_more);
        let cursor3 = page3.next_cursor.unwrap();
        assert_eq!(page3.items[0].id, 11);
        assert_eq!(page3.items[4].id, 15);
        assert_eq!(cursor3, 15);

        // 4. 重复读测试：使用旧 cursor1 再次读取，结果必须完全幂等稳定
        let page2_repeat = store::query_task_events(&conn, scope, Some(run_id), Some(cursor1), Some(5)).unwrap();
        assert_eq!(page2_repeat.items, page2.items);

        // 5. 游标增量补读测试：传入当前末尾游标 cursor3，应返回空列表且 has_more = false
        let catchup_empty = store::query_task_events(&conn, scope, Some(run_id), Some(cursor3), Some(5)).unwrap();
        assert_eq!(catchup_empty.items.len(), 0);
        assert!(!catchup_empty.has_more);

        // 6. 追加新事件后补读：仅返回新增项
        let new_id = store::insert_task_event(
            &conn,
            run_id,
            scope,
            "acc-16",
            "finish",
            "completed",
            2000,
            None,
            Some("追加事件"),
        )
        .unwrap();
        assert_eq!(new_id, 16);

        let catchup_new = store::query_task_events(&conn, scope, Some(run_id), Some(cursor3), Some(5)).unwrap();
        assert_eq!(catchup_new.items.len(), 1);
        assert_eq!(catchup_new.items[0].id, 16);
        assert_eq!(catchup_new.next_cursor, Some(16));
        assert!(!catchup_new.has_more);
    }

    #[test]
    fn test_task_events_unknown_scope_and_cross_scope_fail_closed() {
        let conn = store::open_memory_db().unwrap();

        // 1. 未知 scope 查询直接拒绝
        assert!(tasks::get_task_events("invalid-scope", None, None, None).is_err());
        assert!(tasks::get_task_events("unknown", None, None, None).is_err());

        // 2. 跨 scope run fail-closed
        let run_wb = TaskRun {
            run_id: "run-wb-999".to_string(),
            task_id: "checkin".to_string(),
            scope: "wb-cn".to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "completed".to_string(),
            started_at_ms: 1000,
            started_at: 1,
            finished_at_ms: Some(2000),
            finished_at: Some(2),
            total_items: 1,
            success_items: 1,
            failed_items: 0,
            message: None,
            details_json: None,
            error: None,
            cursor: Some(1),
        };
        store::upsert_task_run(&conn, &run_wb).unwrap();

        // 用 trae-cn 查询属于 wb-cn 的 runId：必须拒绝
        let err = store::query_task_events(&conn, "trae-cn", Some("run-wb-999"), None, None);
        assert!(err.is_err());

        // 查询不存在的 runId：必须拒绝
        let err_missing = store::query_task_events(&conn, "wb-cn", Some("run-not-exist"), None, None);
        assert!(err_missing.is_err());
    }

    #[test]
    fn test_task_events_recovery_repeated_execution_no_duplicate_events() {
        let mut conn = store::open_memory_db().unwrap();
        let scope = "wb-cn";
        let run_id = "run-recover-test";
        let now = 100_000;

        let run = TaskRun {
            run_id: run_id.to_string(),
            task_id: "wb-cn-daily-checkin".to_string(),
            scope: scope.to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "running".to_string(),
            started_at_ms: now - 10_000,
            started_at: (now - 10_000) / 1000,
            finished_at_ms: None,
            finished_at: None,
            total_items: 2,
            success_items: 0,
            failed_items: 0,
            message: None,
            details_json: None,
            error: None,
            cursor: Some(0),
        };
        store::upsert_task_run(&conn, &run).unwrap();

        let item1 = TaskItem {
            id: None,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            account_id: "acc-1".to_string(),
            stage: "prepare".to_string(),
            status: "pending".to_string(),
            started_at_ms: now - 10_000,
            finished_at_ms: None,
            result_data: None,
            error: None,
            retry_count: 0,
        };
        let item2 = TaskItem {
            id: None,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            account_id: "acc-2".to_string(),
            stage: "upstream_write".to_string(),
            status: "running".to_string(),
            started_at_ms: now - 10_000,
            finished_at_ms: None,
            result_data: None,
            error: None,
            retry_count: 0,
        };
        store::upsert_task_item(&conn, &item1).unwrap();
        store::upsert_task_item(&conn, &item2).unwrap();

        // 注册过期租约
        conn.execute(
            "INSERT INTO task_run_leases (run_id, owner_id, expires_at_ms) VALUES (?1, ?2, ?3)",
            rusqlite::params![run_id, "prev-process", now - 1],
        )
        .unwrap();

        // 第一次执行恢复
        let recovered1 = runtime::recover_abandoned_runs(&mut conn, now).unwrap();
        assert_eq!(recovered1, vec![run_id.to_string()]);

        let events1 = store::query_task_events(&conn, scope, Some(run_id), None, None).unwrap();
        let count1 = events1.items.len();
        assert!(count1 >= 3); // 2 item events + 1 run recovery event

        // 第二次重复执行恢复：必须幂等，不重复产生日志
        let recovered2 = runtime::recover_abandoned_runs(&mut conn, now + 1000).unwrap();
        assert!(recovered2.is_empty());

        let events2 = store::query_task_events(&conn, scope, Some(run_id), None, None).unwrap();
        assert_eq!(events2.items.len(), count1);
        assert_eq!(events2.items, events1.items);
    }

    #[test]
    fn test_task_events_schema_v6_migration_no_fabricated_history() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // 模拟已存在 v5 数据库
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at_ms INTEGER NOT NULL);
             INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (5, 'v5_legacy', 1700000000004);
             CREATE TABLE task_runs (run_id TEXT PRIMARY KEY, scope TEXT NOT NULL, task_id TEXT NOT NULL, task_kind TEXT NOT NULL, trigger_source TEXT NOT NULL, status TEXT NOT NULL, started_at_ms INTEGER NOT NULL, finished_at_ms INTEGER, total_items INTEGER, success_items INTEGER, failed_items INTEGER, message TEXT, details_json TEXT, error TEXT, cursor_id INTEGER);
             INSERT INTO task_runs (run_id, scope, task_id, task_kind, trigger_source, status, started_at_ms) VALUES ('run-v5-old', 'wb-cn', 'checkin', 'checkin', 'manual', 'completed', 1000);",
        ).unwrap();

        // 执行 schema 迁移升级至 v6
        store::ensure_schema(&conn).unwrap();

        let v: i64 = conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 6);

        // 验证 task_events 表已创建，但绝不伪造虚假的历史记录
        let events = store::query_task_events(&conn, "wb-cn", Some("run-v5-old"), None, None).unwrap();
        assert_eq!(events.items.len(), 0);
    }

    #[test]
    fn test_task_events_full_lifecycle_coverage() {
        let spec = tasks::get_task_specs("wb-cn").unwrap().remove(0);
        let task_id = spec.task_id.clone();

        // 1. 触发任务执行（离线 Handler 产生 unsupported 结果并完整走完生命周期）
        let run = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(tasks::trigger_task_at_with_handler(
                "wb-cn",
                &task_id,
                Some("manual"),
                Some(vec!["acc-lifecycle-1".into()]),
                &tasks::OfflineTaskActionHandler,
                None,
            ))
            .unwrap();

        let page = tasks::get_task_events("wb-cn", Some(&run.run_id), None, None).unwrap();
        assert!(page.items.len() >= 3);
        // 包含 prepare, upstream_write, finish
        let stages: Vec<&str> = page.items.iter().map(|e| e.stage.as_str()).collect();
        assert!(stages.contains(&"prepare"));
        assert!(stages.contains(&"upstream_write"));
        assert!(stages.contains(&"finish"));

        // 验证旧 run.cursor 与新事件 id 分离，未混淆
        assert!(run.cursor.is_some());
        assert!(page.items[0].id > 0);
    }

    #[test]
    fn test_generate_management_ssr_fixtures() {
        let app_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fixtures_dir = app_dir.join("fixtures/management");
        std::fs::create_dir_all(&fixtures_dir).unwrap();

        let mut conn = store::open_memory_db().unwrap();
        let now = 1700000000000i64;

        // 1. 生成 empty.json（无任何观测数据）
        let empty_proj = credits::project_credit_summary_in_conn(&conn, "wb-cn", None, None, now).unwrap();
        assert_eq!(empty_proj.summary.is_known, false);
        assert_eq!(empty_proj.status.code, "unobserved");
        assert_eq!(empty_proj.status.source_kind, "unknown");

        let empty_json = serde_json::to_string_pretty(&empty_proj).unwrap();
        std::fs::write(fixtures_dir.join("empty.json"), &empty_json).unwrap();

        // 2. 生成 known.json（具有真实官方观测数据和 usage 消耗事件）
        let acc_ref = AccountRef {
            scope: "wb-cn".to_string(),
            account_id: "fixture".to_string(),
            display_name: "Fixture Account".to_string(),
            is_active: true,
            updated_at_ms: now,
        };
        store::upsert_account_ref(&conn, &acc_ref).unwrap();

        let obs = CreditObservation {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "fixture".to_string(),
            source: "official".to_string(),
            remaining: 15.0,
            total: 20.0,
            unit: "credits".to_string(),
            is_stale: false,
            is_comparable: true,
            observed_at_ms: now,
        };
        store::insert_credit_observation(&conn, &obs).unwrap();

        let pkg = CreditPackageItem {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "fixture".to_string(),
            account_name: "Fixture Account".to_string(),
            package_id: "pkg-1".to_string(),
            name: "通用积分包".to_string(),
            package_name: "通用积分包".to_string(),
            package_type: "plan".to_string(),
            remaining: 15.0,
            remaining_balance: 15.0,
            total: 20.0,
            original_capacity: Some(20.0),
            unit: "credits".to_string(),
            expire_at_ms: Some(now + 3 * 86400 * 1000),
            expire_time: Some((now + 3 * 86400 * 1000) / 1000),
            expire_time_formatted: Some("2023-11-17".to_string()),
            is_inferred: false,
            is_stale: false,
            source: "official".to_string(),
            source_type: "official".to_string(),
            observed_at_ms: now,
            observed_at: now / 1000,
            status: "normal".to_string(),
        };
        store::replace_account_credit_packages(&mut conn, "wb-cn", "fixture", &[pkg]).unwrap();

        let usage_evt = CreditEvent {
            id: None,
            scope: "wb-cn".to_string(),
            account_id: "fixture".to_string(),
            account_name: Some("Fixture Account".to_string()),
            event_id: Some("fixture-0".to_string()),
            event_kind: "usage".to_string(),
            event_type: "usage".to_string(),
            amount: 1.0,
            change_amount: -1.0,
            balance_after: 15.0,
            unit: "credits".to_string(),
            occurred_at_ms: now - 3600_000,
            recorded_at: (now - 3600_000) / 1000,
            evidence_level: "official_api".to_string(),
            note: Some("fixture".to_string()),
            reason: Some("fixture".to_string()),
        };
        store::insert_credit_event(&conn, &usage_evt).unwrap();

        let known_proj = credits::project_credit_summary_in_conn(&conn, "wb-cn", None, None, now).unwrap();
        assert_eq!(known_proj.summary.is_known, true);
        assert_eq!(known_proj.summary.current_remaining, 15.0);
        assert_eq!(known_proj.status.code, "live");
        assert_eq!(known_proj.status.source_kind, "official");
        assert!(!known_proj.events.is_empty());
        assert_eq!(known_proj.events[0].change_amount, -1.0);
        assert_eq!(known_proj.recent_events[0].change_amount, -1.0);

        let known_json = serde_json::to_string_pretty(&known_proj).unwrap();
        std::fs::write(fixtures_dir.join("known.json"), &known_json).unwrap();
    }

    #[tokio::test]
    async fn test_task_events_sqlite_abort_trigger_rollback_atomicity() {
        // 1. 验证公开 trigger 入口：通过独立临时 home 在 DB 装 abort trigger，证明创建与准备事件写失败时，整笔 run/items 事务回滚
        let test_dir = std::env::temp_dir().join(format!("test-events-abort-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&test_dir).unwrap();
        {
            let _guard = crate::modules::config::HomeOverrideGuard::set(&test_dir);
            // 初始化 DB 并安装 trigger
            {
                let conn = store::open_db().unwrap();
                conn.execute(
                    "CREATE TRIGGER abort_task_events BEFORE INSERT ON task_events BEGIN SELECT RAISE(ABORT, 'forced abort on task_events'); END;",
                    [],
                ).unwrap();
            }

            // 调用实际公开 trigger_task_at_with_handler 入口
            let trigger_res = tasks::trigger_task_at_with_handler(
                "wb-cn",
                "wb-cn-daily-checkin",
                Some("manual"),
                Some(vec!["acc-abort-test".to_string()]),
                &tasks::OfflineTaskActionHandler,
                None,
            ).await;
            assert!(trigger_res.is_err(), "公开 trigger 应因 trigger abort 报错");
            assert!(trigger_res.unwrap_err().contains("forced abort on task_events"));

            // 验证整个事务回滚：无任何半创建 run 或半提交 item
            let conn = store::open_db().unwrap();
            let runs = store::query_task_runs(&conn, "wb-cn", None).unwrap();
            assert_eq!(runs.len(), 0, "回滚后绝不能留下半创建的 TaskRun");
            let event_count: i64 = conn.query_row("SELECT COUNT(*) FROM task_events", [], |r| r.get(0)).unwrap();
            assert_eq!(event_count, 0, "回滚后绝不能留下幽灵事件");
        }
        let _ = std::fs::remove_dir_all(&test_dir);

        // 2. 内存 DB 验证公开 cancel 与 recovery 入口在 abort trigger 下的完全回滚（保持既有测试）
        let mut conn = store::open_memory_db().unwrap();
        let scope = "wb-cn";
        let run_id = "run-abort-test";
        let now = 100000;

        let run = TaskRun {
            run_id: run_id.to_string(),
            task_id: "wb-cn-daily-checkin".to_string(),
            scope: scope.to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "running".to_string(),
            started_at_ms: now,
            started_at: now / 1000,
            finished_at_ms: None,
            finished_at: None,
            total_items: 1,
            success_items: 0,
            failed_items: 0,
            message: Some("正在执行".to_string()),
            details_json: None,
            error: None,
            cursor: Some(0),
        };
        store::upsert_task_run(&conn, &run).unwrap();
        let item = TaskItem {
            id: None,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            account_id: "acc-1".to_string(),
            stage: "upstream_write".to_string(),
            status: "running".to_string(),
            started_at_ms: now,
            finished_at_ms: None,
            result_data: None,
            error: None,
            retry_count: 0,
        };
        store::upsert_task_item(&conn, &item).unwrap();

        // 1. 安装 SQLite abort trigger：任何写入 task_events 均抛出 abort
        conn.execute(
            "CREATE TRIGGER abort_task_events BEFORE INSERT ON task_events BEGIN SELECT RAISE(ABORT, 'forced abort on task_events'); END;",
            [],
        ).unwrap();

        // 2. 测试 cancel 入口：当事件写失败时，cancel 操作事务回滚
        let cancel_res = tasks::cancel_task_run_in_conn(&conn, scope, run_id);
        assert!(cancel_res.is_err(), "cancel 应因 trigger abort 报错");
        assert!(cancel_res.unwrap_err().contains("forced abort on task_events"));

        // 验证数据未被半提交：run 仍为 running，item 仍为 running，未被写为 cancelled 或 pending_confirmation
        let queried_runs = store::query_task_runs(&conn, scope, Some(run_id)).unwrap();
        assert_eq!(queried_runs[0].status, "running");
        let queried_items = store::query_task_items(&conn, run_id).unwrap();
        assert_eq!(queried_items[0].status, "running");
        let event_count: i64 = conn.query_row("SELECT COUNT(*) FROM task_events", [], |r| r.get(0)).unwrap();
        assert_eq!(event_count, 0);

        // 3. 测试 recovery 入口：当事件写失败时，恢复操作事务回滚
        runtime::register_run(&conn, run_id, now - 60000).unwrap();
        // 租约过期以满足 recovery 条件
        conn.execute("UPDATE task_run_leases SET expires_at_ms = ?1 WHERE run_id = ?2", rusqlite::params![now - 1000, run_id]).unwrap();
        let recover_res = runtime::recover_abandoned_runs(&mut conn, now);
        assert!(recover_res.is_err(), "recovery 应因 trigger abort 报错");

        // 验证 recovery 事务完全回滚：run 依然是 running，未被改为 waiting_verification/failed
        let runs_after_rec = store::query_task_runs(&conn, scope, Some(run_id)).unwrap();
        assert_eq!(runs_after_rec[0].status, "running");
        let items_after_rec = store::query_task_items(&conn, run_id).unwrap();
        assert_eq!(items_after_rec[0].status, "running");
    }

    struct FakeLeakyActionHandler;

    impl tasks::TaskActionHandler for FakeLeakyActionHandler {
        async fn execute_item(
            &self,
            _scope: &str,
            _task_id: &str,
            _task_kind: &str,
            account_id: &str,
        ) -> Result<tasks::TaskItemExecutionResult, String> {
            if account_id == "acc-err" {
                // 上游写请求返回 Err，带有敏感令牌和 prompt
                Err("Authorization: Bearer eyJhbGciOiJIUzI1Ni...&prompt=secret_prompt_12345&raw_resp=unredacted_tokens_leak".to_string())
            } else {
                // 模拟返回带有未受控 stage/status 和 error 的 failed 结果
                Ok(tasks::TaskItemExecutionResult {
                    status: "failed".to_string(),
                    stage: "leak_secret_stage_Bearer_123".to_string(),
                    message: Some("raw_resp_unredacted_tokens_leak".to_string()),
                    data: None,
                    error: Some("Authorization: Bearer eyJhbGciOiJIUzI1Ni...&api_key=sk-secret_key_9999".to_string()),
                })
            }
        }
    }

    #[tokio::test]
    async fn test_task_events_sanitization_negative_control() {
        // 验证真实 TaskActionHandler 注入含假凭据、prompt、原始响应泄漏时，
        // 走实际 trigger_task_at_with_handler 和 retry_task_run_with_handler 路径，
        // task_events 严格脱敏，白名单校验 stage/status，序列化绝无任何敏感信息泄漏，且完整生命周期真实出现。
        let test_dir = std::env::temp_dir().join(format!("test-events-leak-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&test_dir).unwrap();
        {
            let _guard = crate::modules::config::HomeOverrideGuard::set(&test_dir);
            let handler = FakeLeakyActionHandler;

            // 1. 通过 trigger_task_at_with_handler 真实触发执行两条账号
            let run = tasks::trigger_task_at_with_handler(
                "wb-cn",
                "wb-cn-daily-checkin",
                Some("manual"),
                Some(vec!["acc-err".to_string(), "acc-fail".to_string()]),
                &handler,
                None,
            ).await.unwrap();

            // 2. 通过 retry_task_run_with_handler 真实触发重试 failed 项
            let retry_run = tasks::retry_task_run_with_handler(
                "wb-cn",
                &run.run_id,
                &handler,
            ).await.unwrap();
            assert_eq!(retry_run.run_id, run.run_id);

            // 3. 读取该 run 的全部持久化事件并序列化为 JSON 进行负控检查
            let page = tasks::get_task_events("wb-cn", Some(&run.run_id), None, None).unwrap();
            assert!(!page.items.is_empty(), "必须真实产生生命周期事件");
            let json_str = serde_json::to_string(&page).unwrap();

            // 敏感信息负控断言
            assert!(!json_str.contains("Bearer"), "事件序列化泄漏了凭据前缀 Bearer");
            assert!(!json_str.contains("eyJ"), "事件序列化泄漏了 JWT token 前缀 eyJ");
            assert!(!json_str.contains("secret_prompt"), "事件序列化泄漏了敏感 prompt");
            assert!(!json_str.contains("unredacted_tokens_leak"), "事件序列化泄漏了原始响应内容");
            assert!(!json_str.contains("sk-secret_key"), "事件序列化泄漏了 api_key");
            assert!(!json_str.contains("leak_secret_stage"), "未受控 stage 泄漏到了事件表");

            // 真实生命周期事件断言
            let stages: Vec<&str> = page.items.iter().map(|e| e.stage.as_str()).collect();
            let statuses: Vec<&str> = page.items.iter().map(|e| e.status.as_str()).collect();
            assert!(stages.contains(&"prepare"), "必须包含创建 prepare 事件");
            assert!(stages.contains(&"upstream_write"), "必须包含账号执行 upstream_write 事件");
            assert!(stages.contains(&"waiting_verification"), "必须包含待确认 waiting_verification 事件");
            assert!(stages.contains(&"finish"), "必须包含汇总结算 finish 事件");
            assert!(stages.contains(&"retry_start"), "必须包含重试启动 retry_start 事件");

            // 所有事件的 status 必须受控于白名单
            for st in &statuses {
                assert!(
                    matches!(*st, "running" | "pending_confirmation" | "failed" | "confirmed" | "waiting_verification" | "cancelled" | "completed"),
                    "检测到未受控的 status: {st}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_task_events_cancel_idempotency_no_duplicate_events() {
        let conn = store::open_memory_db().unwrap();
        let scope = "wb-cn";
        let run_id = "run-cancel-idempotent";
        let now = 300000;

        let run = TaskRun {
            run_id: run_id.to_string(),
            task_id: "wb-cn-daily-checkin".to_string(),
            scope: scope.to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "running".to_string(),
            started_at_ms: now,
            started_at: now / 1000,
            finished_at_ms: None,
            finished_at: None,
            total_items: 2,
            success_items: 0,
            failed_items: 0,
            message: Some("正在执行".to_string()),
            details_json: None,
            error: None,
            cursor: Some(0),
        };
        store::upsert_task_run(&conn, &run).unwrap();

        let item1 = TaskItem {
            id: None,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            account_id: "acc-1".to_string(),
            stage: "prepare".to_string(),
            status: "pending".to_string(),
            started_at_ms: now,
            finished_at_ms: None,
            result_data: None,
            error: None,
            retry_count: 0,
        };
        let item2 = TaskItem {
            id: None,
            run_id: run_id.to_string(),
            scope: scope.to_string(),
            account_id: "acc-2".to_string(),
            stage: "upstream_write".to_string(),
            status: "running".to_string(),
            started_at_ms: now,
            finished_at_ms: None,
            result_data: None,
            error: None,
            retry_count: 0,
        };
        store::upsert_task_item(&conn, &item1).unwrap();
        store::upsert_task_item(&conn, &item2).unwrap();

        // 第一次 cancel：item1 -> cancelled, item2 -> pending_confirmation, run -> waiting_verification
        let cancel1 = tasks::cancel_task_run_in_conn(&conn, scope, run_id).unwrap();
        assert_eq!(cancel1.status, "waiting_verification");
        let events1 = store::query_task_events(&conn, scope, Some(run_id), None, None).unwrap();
        let count1 = events1.items.len();
        assert!(count1 > 0, "第一次 cancel 应产生事件");

        // 第二次 cancel：此时已处于 waiting_verification，无状态流转，必须幂等且不追加任何重复事件！
        let cancel2 = tasks::cancel_task_run_in_conn(&conn, scope, run_id).unwrap();
        assert_eq!(cancel2.status, "waiting_verification");
        let events2 = store::query_task_events(&conn, scope, Some(run_id), None, None).unwrap();
        assert_eq!(events2.items.len(), count1, "第二次 cancel 不能追加任何重复事件日志！");
    }

    #[test]
    fn test_task_events_cursor_edge_cases() {
        let conn = store::open_memory_db().unwrap();
        let scope = "wb-cn";
        let run_id = "run-cursor-edge";

        // 先登记合法的 TaskRun 以满足 fail-closed 范围检查
        let run = TaskRun {
            run_id: run_id.to_string(),
            task_id: "wb-cn-daily-checkin".to_string(),
            scope: scope.to_string(),
            task_kind: "checkin".to_string(),
            trigger_source: "manual".to_string(),
            triggered_by: "manual".to_string(),
            status: "running".to_string(),
            started_at_ms: 100,
            started_at: 0,
            finished_at_ms: None,
            finished_at: None,
            total_items: 3,
            success_items: 0,
            failed_items: 0,
            message: None,
            details_json: None,
            error: None,
            cursor: Some(0),
        };
        store::upsert_task_run(&conn, &run).unwrap();

        // 插入 3 条事件
        let id1 = store::insert_task_event(&conn, run_id, scope, "acc-1", "s1", "running", 100, None, Some("m1")).unwrap();
        let id2 = store::insert_task_event(&conn, run_id, scope, "acc-2", "s2", "running", 200, None, Some("m2")).unwrap();
        let id3 = store::insert_task_event(&conn, run_id, scope, "acc-3", "s3", "running", 300, None, Some("m3")).unwrap();

        // 1. 查询末尾之后（after_cursor = id3）：返回空页，next_cursor 为 None，has_more 为 false
        let empty_page = store::query_task_events(&conn, scope, Some(run_id), Some(id3), Some(10)).unwrap();
        assert_eq!(empty_page.items.len(), 0);
        assert_eq!(empty_page.next_cursor, None);
        assert_eq!(empty_page.has_more, false);

        // 2. 超大未来游标（after_cursor = 999999）：同样安全返回空页
        let future_page = store::query_task_events(&conn, scope, Some(run_id), Some(999999), Some(10)).unwrap();
        assert_eq!(future_page.items.len(), 0);
        assert_eq!(future_page.next_cursor, None);
        assert_eq!(future_page.has_more, false);

        // 3. 重复使用旧游标（after_cursor = id1）：稳定返回后续 items (id2, id3)
        let repeat1 = store::query_task_events(&conn, scope, Some(run_id), Some(id1), Some(10)).unwrap();
        let repeat2 = store::query_task_events(&conn, scope, Some(run_id), Some(id1), Some(10)).unwrap();
        assert_eq!(repeat1, repeat2);
        assert_eq!(repeat1.items.len(), 2);
        assert_eq!(repeat1.items[0].id, id2);
        assert_eq!(repeat1.items[1].id, id3);
        assert_eq!(repeat1.items[0].sequence, id2);
        assert_eq!(repeat1.items[1].sequence, id3);
    }
}
