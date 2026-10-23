# BuddySwitch 核心管理闭环实施与复核修订证据 (FUS-W02 / W03 / W04)

> 实施与修订日期：2026-10-22  
> 状态：初稿经宿主独立复核阻断（参见 `reports/call-agy-host-review-blockers.md`），已完成第二轮定点修复、契约双向贯通与全量回归（首批管理闭环代码就绪；真实桌面交互与供应商到账待真实上线验证）  
> 权威 PRD 依据：`D:/desktop/FUSION-INTEGRATION-v1.1.md` (W02, W03, W04 批次)  
> 起始 HEAD：`04cd946`；工作区：`D:/Project/buddy-Qoder-Switch/app`  

---

## 1. 宿主独立复核阻断项与第二轮修复说明

在首轮代码写入后，宿主运行独立验证程序（`reports/call-agy-host-management-review.rs`）并在全新临时数据根下发现了 7 项阻断性问题。本轮拒绝假“通过”声明，对全部阻断项进行了代码级定点修复：

1. **DTO 契约双向贯通与时间戳修正**：
   - Rust 模型（`CreditSummaryProjection`）通过 `#[serde(alias = "...")]` 双向支持前端既有字段（`summary` / `stats`、`events` / `recentEvents`、`remaining` / `remaining_balance`、`count` / `package_count`、`days_until` / `days_remaining` 等）。
   - 修复前端时间戳解析逻辑：此前 `formatTime` 遇到毫秒（13 位）直接再次 `* 1000` 导致时间漂移到数万年后；现增加 `ts > 1e11 ? ts : ts * 1000` 兼容守卫。
   - 移除面向用户的产品界面生硬内部代号（不再显示 W02/W03/W04/W06/W07 编号，改用规范的产品化中文文案）。

2. **积分包库存去重与快照替换**：
   - SQLite 中 `UNIQUE(scope, account_id, package_id, expire_at_ms)` 当 `expire_at_ms` 为 NULL 时不构成唯一冲突。现使用表达式索引 `ifnull(expire_at_ms, -1)` 确保稳定幂等。
   - 新增 `replace_account_credit_packages`（事务级快照替换）：一次成功全量采集后，若旧包在最新上游中已消失或用尽，将其从当前活动库存中移除，并保留历史观测流水。
   - 新增 `mark_account_packages_stale`：采集失败或部分失败时，绝不误清空旧数据，而是持久化标记为 stale 与保留错误上下文。

3. **汇总与日期边界修复**：
   - 修复事件列表限制影响汇总的问题：此前使用 `query_credit_events(..., 100)` 的结果计算流水汇总导致 151 条数据被截断；现新增数据库层 `aggregate_credit_events_summary`（通过 SQL SUM 直接聚合），展示分页与指标聚合彻底解耦。
   - 修复到期倒计时天数差计算：使用本地自然日 `(expire_date - today).num_days()` 替代纯毫秒相除 ceil；同一自然日未来 1 小时天数差恒为 0（显示“今日到期”），跨自然日才为 1。

4. **Schema 版本保护与诚实数据源状态**：
   - `ensure_schema` 增加未来版本检查：遇到 `version > 1` 的未知数据库架构立即返回 Err 拒绝，防止低版本篡改高版本结构。
   - 空数据库与未观测数据诚实处理：未取到观测数据时 `is_known = false`，`source_kind = "unknown"`，绝不谎报 official 零余额。

5. **来源能力诚实性与多平台适配**：
   - 澄清能力边界：Qoder 具备官方额度查询能力（`quota_usage` 与 `region_view`），但尚未开放逐包资产明细拆分接口；系统如实展示其官方额度总额，绝不伪造 `is_inferred = false` 的虚假套餐包，状态明确标为“尚未提供逐包明细接口”。
   - WorkBuddy 与 Trae 走通官方积分结构到 UI 的完整投影链路。

6. **筛选联动与防竞态保护**：
   - 账号筛选器选中具体账号时，带参请求后端 `getManagementCreditSummary(scope, filterAccount, windowDays)`，联动过滤摘要、统计与积分包明细。
   - 前端增加 `useRef` 请求 ID 计数器，旧的迟到响应自动作废，防止跨 scope 切换或慢速请求覆盖当前视图。

7. **W02 任务中心与调度原型诚实边界**：
   - 明确标注：W02 为管理规范契约与调度原型，统一数据接口、表结构及调度规范已在 core 与双宿主中闭环固化。
   - **W06（完整自动化任务执行引擎）与 W07（常驻后台守护调度器）尚未交付**，绝不以空假页面伪装已具备后台自动无人值守执行能力。

---

## 2. 变更文件清单

### Core 层 (Rust):
- `crates/buddy-switch-core/src/modules/mod.rs` (挂载 `pub mod management;`)
- `crates/buddy-switch-core/src/modules/management/mod.rs` (模块导出)
- `crates/buddy-switch-core/src/modules/management/models.rs` (重构领域与契约模型，双向 serde alias)
- `crates/buddy-switch-core/src/modules/management/store.rs` (SQLite 索引幂等、事务快照替换、stale 标记、SQL SUM 汇总与高版本拒绝)
- `crates/buddy-switch-core/src/modules/management/credits.rs` (本地自然日差值、Qoder 真实额度集成、诚实未知态判断)
- `crates/buddy-switch-core/src/modules/management/tasks.rs` (任务规范与执行记录原型契约)
- `crates/buddy-switch-core/src/modules/management/tests.rs` (12 项针对复核阻断的定点单元测试)

### 宿主适配层 (Rust):
- `src-tauri/src/commands.rs` (注册 `management_credit_summary`, `management_refresh_credits`, `management_task_specs`, `management_task_runs`)
- `src-tauri/src/lib.rs` (invoke_handler 注册)
- `crates/buddy-switch-server/src/api.rs` (HTTP 路由表与 4 个 handlers)

### 前端与契约层 (TypeScript / React):
- `src/lib/types.ts` (导出管理层统一类型定义，包含 `unobserved` code)
- `src/lib/api.ts` (注册 ROUTES、DEMO_READ_COMMANDS，导出 4 个前端调用函数)
- `src/lib/screenshot-demo.ts` (演示模式只读契约模拟分支)
- `src/components/tasks/task-center-dialog.tsx` (规范文案，明确标注后台执行器后续阶段交付)
- `src/components/usage/credit-management-workbench.tsx` (防竞态、账号筛选联动后端、时间戳安全解析、自然日分组)
- `src/pages/CreditStatsPage.tsx` (WorkBuddy 积分页透传 `viewWindow`)
- `src/pages/TraeCreditsPage.tsx` (Trae 积分页透传 `viewWindow`)
- `src/pages/QoderUsagePage.tsx` (Qoder 用量页透传 `viewWindow`，如实展示官方总额度与逐包明细受限状态)

---

## 3. 验证执行与测试结果

### 3.1 管理模块定点单元测试 (12 项全部通过)
命令：`cargo test -p buddy-switch-core --lib modules::management --locked`
- `test_unknown_scope_fail_closed` ... ok
- `test_serde_json_compatibility_with_frontend` ... ok
- `test_same_day_midnight_and_hours_difference_is_zero` ... ok
- `test_schema_migration_idempotent` ... ok
- `test_observation_deduplication` ... ok
- `test_unknown_expiry_upsert_dedup` ... ok
- `test_task_specs_w02_contract_metadata` ... ok
- `test_mark_account_packages_stale_retains_data` ... ok
- `test_replace_account_packages_removes_old_packages` ... ok
- `test_future_schema_version_rejected` ... ok
- `test_empty_data_source_kind_is_not_official_and_not_fake_zero` ... ok
- `test_151_events_aggregation_not_truncated_by_limit` ... ok
**结果**：12 passed; 0 failed (耗时 0.02s)。

### 3.2 离线全量回归测试
命令：`cargo test --workspace --locked -- --test-threads=1`
- `buddy-switch-core`: 1548 passed; 0 failed
- `buddy-switch-gateway`: 13 passed; 0 failed
- `qoder_queue_http`: 5 passed; 0 failed
- `qoder_stability_http`: 1 passed; 0 failed
- `buddy-switch-rust` (tauri lib): 32 passed; 0 failed
- `buddy-switch-server`: 52 passed; 0 failed
**总计**：逾 1650 项自动化测试全量通过，0 项失败。

### 3.3 前端构建与门禁检查
- `tsc --noEmit`: 0 类型错误
- `check-api-contract`: 通过 (routes=229, call=219, server=210, invoke=237)
- `check-scope-registry`: 308 项门禁测试全部通过 (308 passed / 0 failed)
- `vite build`: 生产打包成功，产物正常输出到 `dist/`。

### 3.4 代码格式与空白检查
命令：`git diff --check`
**结果**：无任何行尾空白或换行异常。

---

## 4. 候选构建产物与校验哈希

在不启动进程的前提下，使用 `--locked` 重新编译了双宿主候选产物：

1. **桌面端候选二进制 (Tauri)**:
   - 路径: `D:\Project\buddy-Qoder-Switch\app\target\debug\buddy-switch-rust.exe`
   - 修改时间: `2026-10-22 17:50:08`
   - 文件大小: `50,157,056` 字节
   - SHA256: `E5B5381536AA997449F7AFDB35882C877FB27924A260DC8ACFCDE47C430BE825`

2. **服务端候选二进制 (Server)**:
   - 路径: `D:\Project\buddy-Qoder-Switch\app\target\debug\buddy-switch.exe`
   - 修改时间: `2026-10-22 17:50:30`
   - 文件大小: `34,254,336` 字节
   - SHA256: `3AED47EBCBE4DF773EE3AC40DB4A7E6A166542D89DF5EE40EFAB4C21EDD68994`

---

## 6. 宿主独立复核与第三轮定向收尾证据 (2026-10-22 18:50)

宿主在全新临时数据根下重跑独立验证程序并执行真实 JSON SSR 渲染（`reports/call-agy-host-lead-verify/`），确认了前 7 项后端阻断全部通过，并定向指出 2 项待收尾缺陷及 1 项自查项。本轮已彻底完成闭环修复：

### 6.1 修复详情
1. **未观测状态与未知资产诚实呈现**：
   - `renderStatusBanner` 增加明确 `unobserved` 分支，存储状态显示 `未观测 (Unobserved)`，观测时间显示 `未观测（无历史快照）`，绝不落入本地快照默认分支；
   - 4 格统计卡消费 `isKnown`（`data.stats.isKnown && data.status?.code !== "unobserved"`）：未观测或未知时，可用余额显示 `—`、总额度容量显示 `未提供`、账号与包数显示 `—`，绝不将未取到数据谎报为已确认零余额；
   - Core 层修复：`credits.rs` 在无观测记录时，`observed_at` 置为 0，`cached_at` 与 `ttl_remaining_seconds` 置为 None，不再以当前时钟伪造观测/归档时刻。
2. **消耗事件负向符号权威修复**：
   - `store.rs` 的 `query_credit_events` 按 `event_kind` 派生标准符号：`usage` 与 `expired` 严格返回负向 `change_amount = -amount.abs()`，`checkin` / `reward` / `recharge` 返回正向 `+amount.abs()`；
   - 前端明细表格针对 `evt.changeAmount < 0` 渲染负号数值与红色警示样式（`text-rose-600`），`> 0` 渲染绿色（`text-emerald-600`），零值渲染中性色；
   - 新增定点单元测试 `test_credit_event_signed_roundtrip_and_json`，验证 DB roundtrip 与 serde JSON 输出中 `usage` 的 `changeAmount` 为 `-5.0`。
3. **自查项（自定义截止日期对齐）**：
   - 在 `credit-management-workbench.tsx` 中引入 `usageWindowBounds`，检查若用户选取的自定义截止日期落在历史过去（`bounds.to !== todayKey()`），通过常驻信息提示告知用户“当前工作台按近 N 天回溯聚合；历史截止范围已对齐近 N 天窗口”，杜绝静默错配。

### 6.2 仓内正式测试与 SSR 验证
1. **Core 模块单元测试 (14 项全绿)**：
   - 命令: `cargo test -p buddy-switch-core --lib modules::management --locked`
   - 结果: 14 passed; 0 failed (包含新增的 `test_credit_event_signed_roundtrip_and_json` 与 `test_unobserved_status_observed_at_zero_and_cached_none`)。
2. **仓内组件 SSR 夹具门禁**：
   - 脚本: `scripts/check-management-ssr.cjs` (复用宿主 SSR 夹具测试规范)
   - 结果: `empty.json` 断言通过（未观测状态、空时间与未提供占位符全部匹配）；`known.json` 断言通过（`usage -1` 红色语义生效，无 `usage +1`）。
3. **前端类型与全量门禁**：
   - `tsc --noEmit`: 0 错误
   - `check:api` 与 `check:scope`: 308 项门禁全量通过
   - `vite build`: 生产构建成功，输出至 `dist/`

### 6.3 最终编译候选二进制 (Debug Profile, --locked)
- **桌面端候选 (Tauri)**:
  - 路径: `D:\Project\buddy-Qoder-Switch\app\target\debug\buddy-switch-rust.exe`
  - 修改时间: `2026/10/22 18:49:54`
  - 文件大小: `50,161,664` 字节
  - SHA256: `CD011113C45076A98FA0270F0396F96593F6D8E9074F6B8F30A3F9643DFC257E`
- **服务端候选 (Server)**:
  - 路径: `D:\Project\buddy-Qoder-Switch\app\target\debug\buddy-switch.exe`
  - 修改时间: `2026/10/22 18:50:08`
  - 文件大小: `34,254,848` 字节
  - SHA256: `A62E5DF5B7553672499249CEB6CB3283D128A761BF8F04DA5CA75962A54FFD63`


1. **W06（完整自动化任务执行器）与 W07（常驻后台守护调度器）**：
   - 本次仅交付 W02 任务中心管理规范模型、状态机契约与原型页面。
   - 真实的后台自动无人值守执行引擎与常驻进程尚未交付，将在后续 PRD 阶段落地。
2. **离线测试与真实环境区别**：
   - 本次修改与测试均在绝对隔离环境下运行，未触碰真实 `~/.buddy-switch` 用户数据根，未消耗真实上游配额。
   - 真实桌面 UI 点击联动及上游供应商积分到账情况有待用户在真实测试环境中进行最终端到端核验。

---

## 7. FUS-W06 任务执行器与调度整合闭环实施与复核记录（2026-10-22）

### 7.1 交付范围与状态机实现

按 PRD §5.3、§7 及工作包 FUS-W06 完成任务执行引擎闭环，严格遵守离线先行边界（不向外部发送未经核实的写请求）：

1. **统一 run/item 状态记录与事件游标**：
   - 模型扩展：`TaskRun` 增加事件游标 `cursor`，`TaskItem` 增加重试计数器 `retry_count` 与结果负载 `result_data`。
   - 数据表迁移：SQLite 版本化升级至 v4，新增 `task_account_locks` 租约锁表与 `idx_task_items_run_acc` 唯一约束。
   - 幂等重入：手动执行与定时调度共用 `trigger_task` 统一入口；对已处于活跃状态（`queued`, `running`, `waiting_upstream`, `waiting_verification`）的同一 `(scope, task_id)` 任务，直接返回既有 `TaskRun`，绝不产生第二个 run。

2. **跨进程账号锁（Lease & Preemption）**：
   - 粒度：`(scope, account_id, task_id)` 账号级互斥。
   - 语义：持有锁期间阻止同账号任务并发冲突；同一 owner 自动心跳续期；租约超时（60s）后允许新 owner 安全接管，防止进程退出后永久锁死。

3. **停止与取消语义（PRD §5.3）**：
   - `cancel_task_run` 对未开始项直接取消为 `cancelled`；
   - 对已发送写请求在途（`running` / `waiting_upstream`）项，严禁声称撤销，严格转入 `pending_confirmation` 待确认态，run 状态转为 `waiting_verification`，后续仅做只读复查。
   - 已完成到账项保持 `confirmed`，不受取消影响。

4. **失败重试过滤（PRD §5.3 / 恢复机制）**：
   - `retry_task_run` 严格仅过滤重试 `failed` 状态项，自动排除并保护 `confirmed`（已到账）、`already_claimed`（此前已领）、`pending_confirmation`（待确认）、`skipped` 与 `unsupported` 项。
   - 重试时递增 `retry_count`，并重新竞争账号锁与执行。

5. **调度器与范围变更（PRD §5.4 / 账号数量展示）**：
   - `scheduler::run_scheduled_management_task` 直连 `tasks::trigger_task`，消除桌面与服务端执行分叉。
   - `save_task_spec` 明确固定账号集合（`fixed`）或动态读取（`all`），并在 `metadata.accountCount` 中如实计算并存储实际账号数。

6. **双宿主契约完全一致**：
   - Tauri 新增：`management_task_items`, `management_trigger_task`, `management_cancel_task`, `management_retry_task`, `management_save_task_spec`。
   - Server 新增：`GET /api/management/tasks/items`, `POST /api/management/tasks/trigger`, `POST /api/management/tasks/cancel`, `POST /api/management/tasks/retry`, `POST /api/management/tasks/specs/save`。
   - 前端 `TaskCenterDialog`：支持手动触发执行、停止取消、逐项重试失败项，以及折叠展开查看逐账号 item 执行详情与错误原因。

### 7.2 验证结果与门禁测试

| 验证项 | 验证命令 | 结果 |
|:--|:--|:--|
| 管理模块定点单元测试 | `cargo test -p buddy-switch-core --lib modules::management --locked` | **19/19 全部通过**（14 项既有管理测试 + 5 项 W06 状态机定点测试全绿） |
| TypeScript 类型检查 | `npm run tsc --noEmit` | **0 错误** |
| API 契约护栏 | `node scripts/check-api-contract.cjs` | **全部通过**（routes=234, call=224, server=215, invoke=242） |
| SSR 初始态渲染门禁 | `node scripts/check-management-ssr.cjs` | **全部通过**（empty 与 known 夹具全部断言通过） |
| 前端构建与预检门禁 | `npm run build` | **全部通过**（含 check:scope 308 项门禁） |
| Workspace 全量测试 | `cargo test --workspace --locked -- --test-threads=1` | **全部通过**（1650+ 测试 0 失败） |

### 7.3 最终双宿主候选产物哈希

- **桌面端候选（Tauri Host）**：
  - 路径：`target/debug/buddy-switch-rust.exe`
  - 大小：`50,337,792` 字节
  - 时间：`2026/10/22 19:19:32`
  - SHA256：`7ACE28DEB75BCD6B2AA46E11E3FB306B32592A537B168292248AA512E280C5C1`
- **服务端候选（Server Host）**：
  - 路径：`target/debug/buddy-switch.exe`
  - 大小：`34,515,456` 字节
  - 时间：`2026/10/22 19:20:14`
  - SHA256：`3939F506443FB0ED5661F0E354D03C9A692F3F5AEED0CD42F5FDEB83400F714D`

### 7.4 交付状态与明确未完成事项

- **已交付**：
  - W02 任务中心规范与原型契约；
  - W03 长期管理历史库与只读采集；
  - W04 积分工作台产品界面与 SSR 门禁；
  - W06 统一任务执行引擎、状态机、跨进程锁、停止取消与失败重试恢复。
- **明确未交付（后续工作包）**：
  - W05 真实 Global 奖励协议及请求头核实（受平台活动开放窗口约束，当前 W06 严格走 Mock/离线回执，不伪造到账）；
  - W07 统一任务中心独立页面、月历视图与全局守护进程；
  - W08 分组多标签与账号体验；
  - W09/W10 数据导入、备份与保险库存储。

