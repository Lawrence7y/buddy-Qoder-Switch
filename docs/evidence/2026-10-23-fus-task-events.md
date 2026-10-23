# FUS-W06 任务事件持久化、游标增量补读与 SSR 门禁修复（2026-10-23）

> 状态：任务生命周期事件持久化、游标增量分页补读、双宿主同构转发、前端去重与状态防回退、SSR 真实夹具门禁已实现并通过离线与隔离测试；真实平台动作、到账复查与 W07 独立任务中心/月历/计划编辑未交付。本轮未启动或安装候选应用。

## 1. 交付内容与架构设计

### 1.1 Schema v6 迁移与事件持久化
- `CURRENT_SCHEMA_VERSION` 升级至 `6`；新增 `task_events` 数据表与 `(scope, id)`、`(run_id, id)` 索引。
- 字段契约：`id INTEGER PRIMARY KEY AUTOINCREMENT`（对应单调递增 `sequence`，契约中保证 `sequence == id`）、`run_id`、`scope`、`account_id`、`stage`、`status`、`occurred_at_ms`、`error_category`、`message`。
- **游标分立**：旧 `TaskRun.cursor` 保持为逐账号处理进度计数器，与全新的单调事件游标 `TaskEvent.id` / `sequence` 完全解耦，绝不混淆。
- **严格事件脱敏**：事件表只记录受控 stage/status/errorCategory 与固定短文案（如 `"上游调用失败，可稍后重试"`、`"写请求已发出，转入待确认状态"`），严禁透传 `item.error` 中的原始凭据、URL token、prompt、或原始上游响应。

### 1.2 关键生命周期与事务原子性
- 任务生命周期各关键阶段变更与事件写入均置于同一数据库事务中：
  - 任务创建并启动：`prepare` / `running`
  - 逐账号执行开始：`upstream_write` / `running`
  - 逐账号执行结束：`confirmed` / `failed` / `pending_confirmation` / `unsupported` / `skipped`
  - 任务汇总结算：`finish` (`completed` / `failed` / `waiting_verification` / `cancelled`)
  - 用户取消：未开始账号标 `cancelled`，在途写请求转 `pending_confirmation`，运行转 `waiting_verification` 或 `cancelled`
  - 重试：`retry_start`，重试账号重新排队与执行
  - 租约超时恢复：`recovery`，中断写请求转 `pending_confirmation`，未开始项转 `failed`
- **取消幂等保护**：若任务运行已处于 `waiting_verification` 或 `cancelled` 且本次取消未产生任何账号状态迁移，取消操作保持严格幂等，不重复写入 cancel 事件。
- **无无意义刷屏**：锁续期与心跳不产生无意义事件刷屏；恢复逻辑重复触发严格幂等无重复事件。

### 1.3 查询与分页 Fail-Closed
- `store::query_task_events` / `tasks::get_task_events`：
  - 支持 `scope` 过滤，可选 `run_id` 范围限制。
  - `after_cursor` 单调向后增量读取；`limit` 默认 50，有界夹紧 `[1, 200]`。
  - 返回 `TaskEventPage { items, next_cursor, has_more }`。
  - 空页与末尾页稳定返回 `next_cursor: None` 与 `has_more: false`，不出现空页回 0 循环重读。
  - **严格 Fail-Closed**：未知 scope 直接报错拒绝；若指定 `run_id` 但在 `task_runs` 中不存在或不属于该 `scope`，一律 fail-closed 报错拒绝。

### 1.4 双宿主与前端契约同构
- **宿主转发**：
  - Tauri 命令 `management_task_events` 注册于 `src-tauri/src/commands.rs` 与 `src-tauri/src/lib.rs`。
  - Server REST 端点 `GET /api/management/tasks/events` 注册于 `crates/buddy-switch-server/src/api.rs`。
  - 宿主仅做参数解析与 core 转发，无重复业务逻辑。
- **前端接入与防回退** (`src/components/tasks/task-center-dialog.tsx`)：
  - 展开运行明细卡片支持增量加载生命周期事件与“增量补读”按钮；
  - 按 `sequence ?? id` 进行前端去重并稳定排序；
  - 并发与迟到保护：维护每运行独立的请求版本计数器与加载锁，迟到的异步旧响应绝不覆盖最新状态或清空当前游标；
  - 错误可见化：事件加载失败在卡片内展示错误横幅并提供“重试”按钮，不以静默 `console.warn` 结束；
  - 弹窗关闭或切换 scope 时清理状态，重新打开安全重新读回全量历史，补读操作不触发后台任务执行。

### 1.5 修正 SSR 门禁缺陷与真实 Rust 离线夹具
- 修复 `scripts/check-management-ssr.cjs`：
  - 彻底移除对缺少夹具时 `warn + continue` 仍宣称通过的缺陷，缺少夹具或指定不存在目录时直接通过 `assert.fail` 退出码 1 报错；
  - 彻底移除渲染前将 `changeAmount` 篡改为负值的 monkey-patch 代码；
  - 每轮测试采用独立的临时目录，测试结束后自动清理，避免缓存锁与 ACL 冲突。
- 提取 `credits::project_credit_summary_in_conn`：
  - 允许在只读/内存数据库连接中注入确定性时间戳进行积分投影；
  - 在单元测试 `test_generate_management_ssr_fixtures` 中直接由 Rust 序列化生成真实的 `fixtures/management/empty.json` 与 `fixtures/management/known.json`；
  - `known.json` 中的 `usage` 消耗变动额原生为 `-1.0`，`empty.json` 的 `isKnown` 为 `false` 且 `code` 为 `"unobserved"`，断言真实投影。

---

## 2. 离线验证记录

| 验证项 | 命令 | 结果与说明 |
| --- | --- | --- |
| 管理定点隔离测试 | `pwsh -File scripts/test-management-isolated.ps1` | **46 passed / 0 failed**；包含真实 Handler 假凭据负控（`trigger_task_at_with_handler` + `retry_task_run_with_handler`）、公开 trigger/cancel/recovery 在 SQLite abort trigger 下的同事务回滚、取消操作幂等、游标边缘分页等新增测试全部绿。 |
| 双宿主编译检查 | `cargo check -p buddy-switch-server -p buddy-switch-rust --locked` | **0 errors / 0 warnings**；Server 与 Desktop 宿主编译检查全部通过。 |
| 前端完整门禁构建 | `npm run build` | `tsc`、`check:api` (routes=235, call=225, invoke=243)、`check:docker`、`check:store-selectors`、`check:scope` (308 项全部通过)、Vite 生产构建完成。 |
| SSR 真实夹具验证 | `node scripts/check-management-ssr.cjs` | 2 个真实 Rust 夹具断言全部通过，未观测占位符、负号与红色样式校验通过（父代理已独立复核 exit 0）。 |
| SSR 负向门禁验证 | `$env:MANAGEMENT_FIXTURES_DIR = "D:\non-existent-fixtures"; node scripts/check-management-ssr.cjs` | **退出码 1**；断言失败明确拒绝，绝不报虚假通过（父代理已独立复核 exit 1）。 |
| 真实夹具 SHA-256 | `Get-FileHash fixtures/management/*.json` | `empty.json`: `D594F5EB...`<br>`known.json`: `A346DDBD...` |
| 全仓基线参考 | 历史完整回归 | 早期全仓回归为 2421 passed / 0 failed / 17 ignored（发生在后续源码修改之前；中间局部库测试 1666 项未计全部集成测试）。**本轮收尾仅执行管理层定点隔离测试与构建门禁，不以定点复验替代最终全仓完结或全部宿主验收**。 |

详细定点命令与输出已记录于 `reports/fusion-events-20261023-final.log`。

---

## 3. 边界与诚实声明

| 状态 | 涉及内容 |
| --- | --- |
| **已实现并通过离线定点验证** | Schema v6 迁移与单调任务事件持久化；真实 TaskActionHandler 注入假凭据/prompt 泄漏负控（trigger 与 retry 路径下序列化无敏感数据）；关键流转同事务回滚（公开 trigger、cancel、recovery 入口经 SQLite abort trigger 验证）；取消操作幂等保护；查询与分页 fail-closed；双宿主同构转发；前端彻底移除对已有 run 的破坏性 reset，保留完整历史增量追读；真实 Rust 离线夹具与 SSR 门禁修复。 |
| **已修复待 host 独立复核** | 前端 TSX 闭包在 650 条事件增量追读下“已有 run 被强制 reset 导致中段丢失”的问题（host v2 历史实测 400 条 vs 650 条失败证据保留于 `reports/`）。现已移除外部强制 reset，保持完整追读；**该项状态保持待 host 独立重新验证，在 host 独立运行并给出通过结果前不擅自宣称通过**。 |
| **已实现待真实验证** | 双宿主长时间常驻运行下的事件累积与心跳交互；桌面端真实 GUI 点击与交互操作。 |
| **尚未实现** | 真实平台动作 Handler（当前仍为 Offline 安全隔离）；真实到账复查；W07 独立任务中心页面、签到月历视图、计划完整编辑与系统通知。 |
| **保留边界** | 未启动或停止任何 BuddySwitch、ZCode、TraeWorkAssistant 实例；未触碰真实账号、用户数据、系统代理或 `.mimosa`；测试全程运行于预创建的 `.test-home` 独立隔离环境，结束后恢复环境变量；历史 `reports/call-agy-host-*` 与既有 evidence 未做任何修改。 |
