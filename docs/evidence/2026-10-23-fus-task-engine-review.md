# FUS-W06 执行器接续复核与修正（2026-10-23）

> 更正（后续接续复核）：本页关于测试隔离与未接触真实用户数据的描述已被发现不成立，详见 [隔离事故与处置](2026-10-23-fus-runtime-and-isolation.md#1-测试隔离事故与证据更正)。本页原始记录保留；当前排程/恢复状态亦以后续记录为准。

> 状态：实施与验证记录；本轮仅离线测试及候选构建。没有运行应用、接触真实账号或替换现有实例。

## 接续依据

保留 2026-10-22 的交接、补丁与证据原文，按当前工作树复核，不继承其中“全量闭环”的结论。依据为现行整合 PRD §5.3、§7、§10，以及权威 v1.1 的平台隔离与结果验收要求。

复核发现的阻断：默认离线 handler 返回 `confirmed`；无账号时生成 `default_account`；空历史生成带成功计数的原型 run；取消后执行循环继续执行后续账号且覆盖待确认；运行开始超过五分钟即标失败并允许新运行；明细查询未核对 run 的 scope；固定账号配置未用于生产触发；并发触发与锁获取包含先查后写竞争。

## 本轮修改

- 离线动作返回 `unsupported / adapter_unavailable`，成功账号数为 0，保留 `offline: true`。未接入不再冒充领取成功。
- 空账号集合报错且不创建 run；移除虚拟账号与虚构历史；显式账号去重；Trae 按区域读取账号库。
- 生产触发校验已登记任务和账号所属范围，固定集合实际用于触发；保存拒绝 scope/ID 不符、空固定集合与未知账号。默认新计划停用，停用计划拒绝 scheduler 触发。
- 以 SQLite `IMMEDIATE` 事务保护活跃运行检查和创建；run ID 使用 UUID；锁获取采用单条条件 UPSERT；执行期间定时续租。
- 每个账号领取执行前检查持久化状态；取消将未开始项置为 cancelled，在途项置为 pending_confirmation；handler 返回后不能覆盖待确认。取消期间保留在途锁，由执行者返回后释放。重试采用相同保护，仅 failed 项重新排队。
- handler 返回不确定错误时进入待确认，避免自动重复领取；开始时间过长不能作为可重放依据，保留原运行等待后续恢复/复查能力。
- 明细查询先核对运行所属 scope；前端阻止旧 scope/旧弹窗请求回填，阻止重复动作；取消、重试和手动刷新会重新取明细，失败信息可见。
- 界面明确区分“未接入、未执行”与到账；保存的 cron 不宣称已接入调度。

涉及源码：`crates/buddy-switch-core/src/modules/management/{tasks,store,tests}.rs`、`src/components/tasks/task-center-dialog.tsx`。沿用上一批未提交代码，没有回退他人改动。

## 验证

| 检查 | 本轮结果 |
| --- | --- |
| `cargo test --workspace --locked -- --test-threads=1` | 2406 passed、0 failed、17 ignored，退出码 0；管理模块 27 项通过 |
| `npm run build` | 退出码 0，类型检查、API 等门禁、作用域 308 项检查与 Vite 构建通过；保留既有大 chunk 提示 |
| `node scripts/check-management-ssr.cjs` | empty/known 两夹具通过，覆盖积分工作台未观测与消耗负号；不代表任务弹窗点击验收 |
| `git diff --check` | 退出码 0；仅有 Git CRLF 转 LF 提示 |

新增 8 项回归覆盖：执行中取消、重试中取消、默认离线不确认奖励、空账号/未知任务、重复账号去重、老活跃 run 不因开始时间重放、配置作用域/固定范围校验、两线程同时触发只执行一次。原 19 项管理测试保留；原重入测试改用可注入 handler，继续断言同一 run ID，生产入口新增的任务登记校验由新测试覆盖。

测试数据根为 `.test-home/fusion-resume-20261023-final2`。原始日志：工作区 `reports/fusion-resume-20261023-workspace-final.log`、`reports/fusion-resume-20261023-frontend-final.log`。早期测试登记校验失败及事务跨 await 导致双宿主 `Send` 编译失败的日志也保留；事务已收敛到同步代码块，最终双宿主全仓回归通过。

双宿主候选通过 `cargo build -p buddy-switch-rust -p buddy-switch-server --locked`（退出码 0，仅有链接器输出提示），没有启动或安装：

| 文件 | 字节数 | SHA-256 |
| --- | --- | --- |
| `target/debug/buddy-switch-rust.exe` | 50397184 | `3B68FC14A775D91146345124D8DCC87BC06149520F4AF157F4CD1C122FF093D3` |
| `target/debug/buddy-switch.exe` | 34559488 | `725629178E4A227C0F11975FC2F7CF518DDC989A82FF4F39C2EF2213A9791569` |

构建日志为工作区 `reports/fusion-resume-20261023-candidates.log`；HEAD、四个修改源码的哈希、测试数据根及候选哈希保存在 `reports/fusion-resume-20261023-manifest.json`。进程只读核查显示现有 BuddySwitch 位于 `D:/buddy-switch/BuddySwitch/`，候选输出位于本工作区 `app/target/debug/`，未停止或替换该进程。

## 当前交付边界

| 状态 | 内容 |
| --- | --- |
| 已实现并通过离线测试 | 诚实离线回执、空数据处理、账号去重、scope 守卫、事务化创建与锁、执行中取消、重试过滤与取消、并发重入 |
| 已实现，待真实宿主验证 | 前端旧响应保护、重复动作保护与明细更新；桌面/HTTP 管理交互；定时心跳完整长耗时验收。类型检查、代码检查与候选编译不等于桌面点击验收 |
| 尚未实现 | 真实平台 handler、管理任务 cron 主循环接线、跨进程重启后的自动恢复与只读复查、独立事件日志补读 |
| 后续工作包 | W07 独立任务中心/月历/通知，W08 分组多标签，后续数据导入与保险库 |
| 本轮未测量 | W01 推理稳定性、W05 真实活动窗口与奖励到账、三自然日观察结果 |

`run.cursor` 当前只是逐账号进度计数，不能当作完整事件日志。`scheduler::run_scheduled_management_task` 当前是复用入口，不代表已注册 cron 调度。运行记录可重新查询，不代表后台恢复机制已交付。后续接 W07 之前仍需补齐这些 W06 能力。
