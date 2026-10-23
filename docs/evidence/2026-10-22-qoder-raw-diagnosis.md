<!--
W00 证据归档元数据（YAML）
original_path: D:\Project\buddy-Qoder-Switch\reports\QODER-RAW-DIAGNOSIS-20261022.md
original_sha256: cca589fd0c3cae97c6a6abfc80c35c6f753a5b91ed3afadd191193938a3dc5f1
collected_at: 2026-10-22 11:02:04 (+0800, 文件 mtime)
archived_at: 2026-10-22 (本任务执行时)
scope: 两区原始流探针诊断报告；PRD §3.2
sanitization: 原件已无令牌/账号身份；不含 prompt 或响应正文
-->

# Qoder DeepSeek 工具、连接与速率排查（2026-10-22）

状态：本地参数透传缺陷已修复并测试；上游持续排队，Global 提前结束和 CN 实际吞吐尚未完成真实验收。

## 用户指定的执行顺序

1. 先处理 Qoder Global / CN 的 DeepSeek 工具调用、连接、输出截断与速度问题。
2. 上述链路验证后，再核实并修复 Qoder Global 的签到奖励；本轮尚未更改签到逻辑。

## 本次直接证据

- 用户截图显示 Qoder 官方客户端也提示“模型请求正在排队”，与网关连续记录 `10605 / CALL_COUNT_LIMIT` 一致。这支持“首字长等待与上游排队有关”，不能单独证明开始输出后的解码速度。
- 当前运行进程来自 `release/BuddySwitch-Fusion-Preview-20261022-stream-stability/`；本次没有停止、替换它，也未修改两个 ZCode 会话。
- 直接上游探针两区各发一次短请求，不执行工具、不换号、不刷新凭据、不自动重试。CN 和 Global 都是 HTTP 200 内嵌业务 429；均未产生正文或工具调用。因此没有可用的实际吞吐测量，不能填 0，也不能据此承诺 200 token/秒。
- 原始帧时序见 `qoder-raw-20261022/raw-*.json`。这两份初始探针在识别业务错误后仍读到 45 秒截止，因此 `readFailed=true` 发生在已明确的业务限流之后，不能把它另算为实际生成中断。探针现已改为看到业务错误即收尾，不再继续等。
- `qoder-raw-account-integrity.json` 核对了探针前后两区账号文件 SHA-256，一致；未输出令牌或账号身份。
- 上轮 Global 在 10:46:19 收到 `end_turn`、573 正文字符、0 工具调用、无流错误。CN 在 10:46:42 和 10:47:30 收到并执行工具调用。记录见 `zcode-qoder-20261022-blank-stop-evidence.json`。这是客户端侧证据，未捕获对应历史请求的原始上游完成帧，不能直接断言 Global 是供应商主动停止或网关提前收尾。

## 已确认并修复的代码缺陷

Qoder 的 Anthropic 入口原先经共用转换函数，只保留模型、输出预算等通用字段，会丢失显式推理档位；后续构造 Qoder 上游请求时无法恢复用户选择。

现增加 Qoder 专用 `payload::from_anthropic`，保留原工具定义与调用历史，只映射明确指定的档位：`reasoning_effort`、`reasoning.effort`、`thinking.effort/level`、`output_config.effort`；显式 `thinking.type=disabled` 映射为 `none`。没有指定、只有 adaptive、仅 budget_tokens 或未知值时均不猜测档位，不默认降低思考强度。其他产品入口未改动。

`output_config.effort` 的参数位置依据：[Anthropic 官方文档](https://platform.claude.com/docs/en/build-with-claude/effort)。本修改只修正参数丢失，不宣称它就是本次 Global 早停或 CN 慢的根因。

## 速率口径与后续验收

当前真实网关成功日志多次缺少上游 usage，Anthropic 层以 UTF-8 字节数除以 3 估算 token；包含思考正文与工具参数。ZCode 界面的 token/秒不能直接与供应商标准 token 吞吐做等值比较。两区目录目前显示 `DeepSeek-Flash`，本次没有将这个展示名当作底层模型版本和吞吐保证。

新增 `qoder_stream_timing` 诊断示例记录 HTTP 头时间、首个有效输出时间、最后输出时间、原始结束原因、结束原因后是否仍有工具/正文、工具参数字节、DONE、usage 及分帧长度，不记录正文、提示词、令牌或身份。只有供应商实际给出 completion_tokens 才计算 token 速率，否则保持 null；原始字节速率另列。

上游可用后，应先用短文本、单个无副作用工具、多轮工具结果回传分别验收，再对比同上下文和推理档位下的两区速度。不要把语义上的“说完计划就结束”盲目重放成第二次执行，也不要通过忽略错误或伪造工具调用掩盖失败。

本地回归与候选包结果见 `qoder-reasoning-*.log` 和 `qoder-reasoning-package.json`；未将离线测试或构建通过当作真实 Global 长会话验收。
