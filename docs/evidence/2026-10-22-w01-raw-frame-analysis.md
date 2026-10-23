# 2026-10-22 W01 原始帧取证分析

> 状态：只读取证分析归档 / W01 交付物  
> 规范依据：[PRD §6.2 推理速度与队列](file:///D:/desktop/FUSION-INTEGRATION-v1.1.md#L257-L265)（原件路径：`D:\desktop\FUSION-INTEGRATION-v1.1.md:257-265`）  
> 工作区：`D:\Project\buddy-Qoder-Switch\app`（`docs/evidence/`）  
> 数据源范围：`reports/`（原始证据库，只读）与 `docs/evidence/`（脱敏归档副本，只读）  
> 安全准则：纯离线只读静态分析；禁止网络访问；禁止发起新探针或实例；禁止 Git 写操作；全文遵循严格脱敏红线（无凭据/令牌、无完整 Prompt、无正文回放、数据点全部标注来源坐标）。

---

## 0. 范围与数据源清单

### 0.1 取证背景与目标

本报告针对 Qoder 引擎在接入 DeepSeek 模型（含 CN 与 Global 两大服务区）过程中出现的**推理速度迟缓、上游持续排队、Global 会话疑似提前结束（Blank Stop / 仅输出计划未调用工具）以及历史空结束（合成 end_turn）**现象展开离线取证分析。

依据 [PRD §6.2](file:///D:/desktop/FUSION-INTEGRATION-v1.1.md#L257-L265) 规范要求，推理诊断必须按请求生命周期的五类关键时间点（**请求发出、HTTP 响应、首次有效增量、最后有效增量、终态**）进行结构化还原，严格区分真生成与心跳/伪造帧，不将展示名等同于物理速率承诺，并在原始通信帧层面给出可验证的分类判据与证明力边界。

### 0.2 数据源详表

全部数据源均作为只读证据检视，原始件位于 `reports/` 目录，脱敏副本位于工作区 `docs/evidence/` 目录。各文件体量、行数、条目数、SHA-256 哈希值与脱敏核验状态见下表：

| 序号 | 证据文件路径 | 来源性质 | 文件体量 (Bytes) | 行数 / 条目数 | 完整 SHA-256 哈希值 | 脱敏状态评估与核验依据 |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **D01** | `reports/qoder-raw-20261022/raw-cn-tool-20261022T025842Z.json` | 原始探针帧 | 742 | 33 行 / 1 记录 | `8649EE69626FAB03AE87E5717B4F6A4FD22ED8050DAC0695F1D1B378EC6D4A32` | **脱敏合规**。仅含诊断时序指标与配置键值，无凭据、无 Prompt、无模型正文。 |
| **D02** | `reports/qoder-raw-20261022/raw-global-tool-20261022T025756Z.json` | 原始探针帧 | 747 | 33 行 / 1 记录 | `8DD066FD1D2834A89E7E5051FEC3C18008968C6294674137B0F25CAB6D7E6F17` | **脱敏合规**。同 D01，仅含 Global 探针时序与参数，无敏感数据。 |
| **D03** | `reports/queue-live-probe.jsonl` | 原始活体探针 | 435 | 2 行 / 2 记录 | `CD3703E6A0BDEED52370BAFF0F38B95A7D161F1AE19CDFFEC2559AFDFF6B8D9D` | **脱敏合规**。Anthropic 入口极短诊断记录，仅含状态码与字节数。 |
| **D04** | `reports/queue-live-probe-rendered.jsonl` | 原始渲染探针 | 256 | 1 行 / 1 记录 | `C30989A30E4468C1CA32BB55033A485E32D7EE0D1A783F9CABE0654038955630` | **脱敏合规**。含 `renderedCharacters: 16`，无实际提示词或个人信息。 |
| **D05** | `reports/queue-live-dfmodel.sse` | 原始 SSE 载荷 | 1,345 | 33 行 / 7 帧 | `18792D6B743B9E16BDFE5E188E96CB2ADBC1D0EF81FA3A7F1E156182EBBC16AA` | **脱敏合规**。仅含固定诊断标记 `GATEWAY_PROBE_OK`，无业务数据。 |
| **D06** | `reports/zcode-qoder-20261022-blank-stop-evidence.json` | 原始客户端日志 | 6,349 | 226 行 / 14 记录 | `ED56391CBE7CCB6B217C436B2DA10DFEB405C9F6B7605E8BFE1AB7333F0278E6` | **脱敏合规**。ZCode SDK 客户端生命周期日志，会话 ID 为本地哈希，已清凭据与正文。 |
| **D07** | `docs/evidence/2026-10-22-zcode-qoder-blank-stop-evidence.json` | 交叉脱敏副本 | 6,966 | 234 行 / 14 记录 | `47EF257F13E9CDE9C18245CA098EBAED957F9C105A2D17AB1BCB80F3DF9C2A72` | **脱敏合规**。W00 归档件，头部带 YAML 审计元数据，正文与 D06 字节一致。 |
| **D08** | `reports/QODER-RAW-DIAGNOSIS-20261022.md` | 原始诊断报告 | 4,124 | 35 行 | `CCA589FD0C3CAE97C6A6ABFC80C35C6F753A5B91ED3AFADD191193938A3DC5F1` | **脱敏合规**。技术排查结论文档，无账号密钥与提示词泄露。 |
| **D09** | `docs/evidence/2026-10-22-qoder-raw-diagnosis.md` | 交叉脱敏副本 | 4,569 | 45 行 | `36B0182F0BB7D63744F7E17F785B4EC3D7CC6B36C05D6650D8E30622620EB6D1` | **脱敏合规**。带 W00 归档头，内容与 D08 完全对齐。 |
| **D10** | `reports/QUEUE-FIX.md` | 原始修复记录 | 4,010 | 64 行 | `A33636B5F59E437552C43C6F2018923B57B1DA40FE30B4ED3DC56C176954A753` | **脱敏合规**。本地 58892 端口队列修复记录，已剥离真实任务提示词。 |
| **D11** | `docs/evidence/2026-10-22-queue-fix.md` | 交叉脱敏副本 | 4,524 | 74 行 | `C0E78B3390260DD9A0BA43617E16DA0C7BA3CA9729E5F73DCF1E5D5A347C5863` | **脱敏合规**。带 W00 归档头，内容与 D10 完全对齐。 |
| **D12** | `reports/queue-evidence-summary.json` | 原始队列摘要 | 7,352 | 258 行 / 17 记录 | `6F79F479BC126D8886B8271120954BA2DEE238A2FC4294094106EF468B2A4708` | **脱敏合规**。15 条上游 10605 排队失败记录与 2 条 ZCode 空完成记录。 |

---

## 1. 逐会话五时间点明细

按照 [PRD §6.2](file:///D:/desktop/FUSION-INTEGRATION-v1.1.md#L257-L265) 的硬性要求，每个会话均逐项核验并记录以下五类关键时间点：
1. **① 请求发出**（网关或探针向物理网络发出首字节的绝对时间戳）
2. **② HTTP 响应到达**（网络层接收到 HTTP 状态行与完整响应头的时刻）
3. **③ 首次有效增量**（首个正文文本 `content`、思考 `reasoning_content` 或工具调用参数 `tool_calls` 到达时刻，**排队心跳与 SSE 注释不计入**）
4. **④ 最后有效增量**（最后一个包含有效增量载荷到达时刻）
5. **⑤ 终态**（流结束标记 `[DONE]`、`message_stop`、超时截止或错误返回时刻）

> **数据缺失声明准则**：在原始离线数据中未显式记录的时间点，一律如实标注为「**数据缺失**」，严禁基于总时长或上下文进行主观推测或线性拟合。

---

### 1.1 2026-10-22 实时上游原始帧探针（受控单次请求）

#### 会话 S01：CN 区分隔诊断探针 (`raw-cn-tool`)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | 单次直接上游探针（文件命名：`raw-cn-tool-20261022T025842Z`） | `reports/qoder-raw-20261022/raw-cn-tool-20261022T025842Z.json` |
| **通信协议** | Qoder 专有上游 HTTPS SSE 编码协议 (`/v2/chat/completions/sse`) | `crates/buddy-switch-gateway/examples/qoder_stream_timing.rs:180` |
| **服务区域 / 模型** | CN (`region: "cn"`) / `dfmodel` (展示名：`DeepSeek-Flash`) | `.region` (行 25), `.model` (行 16), `.catalogDisplayName` (行 7) |
| **HTTP 状态** | **HTTP 200** | `.httpStatus = 200` (行 14) |
| **内嵌业务码** | **业务错误 429** (`CALL_COUNT_LIMIT / 10605`) | `.businessErrorStatus = 429` (行 5); `QODER-RAW-DIAGNOSIS-20261022.md:12` |
| **有效增量统计** | 正文字符 0，思考字符 0，输出字节数 **0 Bytes**，工具调用 0 | `.textChars = 0` (行 27), `.reasoningChars = 0` (行 24), `.outputBytes = 0` (行 17) |
| **结束形态** | 未收到上游完成帧，探针在 45 秒超时截止后收尾 (`readFailed: true`) | `.upstreamDone = false` (行 30), `.readFailed = true` (行 23), `.elapsedMs = 45205` (行 8) |
| **① 请求发出** | **数据缺失**（未记录绝对 UTC 毫秒时间戳；粗粒度文件名提供秒级基线 `2026-10-22T02:58:42Z`，单次探针相对原点设为 `t = 0 ms`） | 文件命名基准；数据内缺绝对时间戳字段 |
| **② HTTP 响应** | 相对请求发出 **+196 ms**（绝对时间戳：**数据缺失**） | `.headersMs = 196` (行 13) |
| **③ 首次有效增量** | **数据缺失**（未产生任何有效增量输出） | `.firstOutputMs = null` (行 11), `.outputBytes = 0` (行 17) |
| **④ 最后有效增量** | **数据缺失**（未产生任何有效增量输出） | `.lastOutputMs = null` (行 15) |
| **⑤ 终态到达** | 相对请求发出 **+45,205 ms**（绝对时间戳：**数据缺失**） | `.elapsedMs = 45205` (行 8) |

---

#### 会话 S02：Global 区分隔诊断探针 (`raw-global-tool`)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | 单次直接上游探针（文件命名：`raw-global-tool-20261022T025756Z`） | `reports/qoder-raw-20261022/raw-global-tool-20261022T025756Z.json` |
| **通信协议** | Qoder 专有上游 HTTPS SSE 编码协议 (`/v2/chat/completions/sse`) | `crates/buddy-switch-gateway/examples/qoder_stream_timing.rs:180` |
| **服务区域 / 模型** | Global (`region: "global"`) / `dfmodel` (展示名：`DeepSeek-Flash`) | `.region` (行 25), `.model` (行 16), `.catalogDisplayName` (行 7) |
| **HTTP 状态** | **HTTP 200** | `.httpStatus = 200` (行 14) |
| **内嵌业务码** | **业务错误 429** (`CALL_COUNT_LIMIT / 10605`) | `.businessErrorStatus = 429` (行 5); `QODER-RAW-DIAGNOSIS-20261022.md:12` |
| **有效增量统计** | 正文字符 0，思考字符 0，输出字节数 **0 Bytes**，工具调用 0 | `.textChars = 0` (行 27), `.reasoningChars = 0` (行 24), `.outputBytes = 0` (行 17) |
| **结束形态** | 未收到上游完成帧，探针在 45 秒超时截止后收尾 (`readFailed: true`) | `.upstreamDone = false` (行 30), `.readFailed = true` (行 23), `.elapsedMs = 47126` (行 8) |
| **① 请求发出** | **数据缺失**（未记录绝对 UTC 毫秒时间戳；粗粒度文件名提供秒级基线 `2026-10-22T02:57:56Z`，单次探针相对原点设为 `t = 0 ms`） | 文件命名基准；数据内缺绝对时间戳字段 |
| **② HTTP 响应** | 相对请求发出 **+2,113 ms**（绝对时间戳：**数据缺失**） | `.headersMs = 2113` (行 13) |
| **③ 首次有效增量** | **数据缺失**（未产生任何有效增量输出） | `.firstOutputMs = null` (行 11), `.outputBytes = 0` (行 17) |
| **④ 最后有效增量** | **数据缺失**（未产生任何有效增量输出） | `.lastOutputMs = null` (行 15) |
| **⑤ 终态到达** | 相对请求发出 **+47,126 ms**（绝对时间戳：**数据缺失**） | `.elapsedMs = 47126` (行 8) |

---

### 1.2 2026-10-22 客户端生命周期日志（ZCode SDK 视角）

数据源来自 `reports/zcode-qoder-20261022-blank-stop-evidence.json`（对应脱敏副本 `docs/evidence/2026-10-22-zcode-qoder-blank-stop-evidence.json`）。该数据捕获了两个长会话的 8 次模型请求及关联的工具执行动作。

> **客户端日志特性说明**：ZCode 客户端仅记录请求完成时刻（`time`）与整个调用生命周期的总耗时（`durationMs`）。因此，原始日志中未显式记录物理发出时刻、HTTP 响应头到达时刻以及首尾有效增量的分段毫秒时间戳。

#### 会话 S03：CN 会话轮次 1 (`session_48d4d720...` 限流)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_48d4d720060f80547bf9d80f5eb9c0f4` / 来源：`main_turn` | `zcode-qoder-20261022-blank-stop-evidence.json:[0]` (行 2-17) |
| **通信协议** | Anthropic 协议（经本地网关 58892 转换为 Qoder 上游） | `QUEUE-FIX.md:17-18` |
| **服务区域 / 模型** | CN / `qoder-cn:dfmodel` | `.[0].model` (行 6) |
| **HTTP 状态** | **HTTP 429**（网关向客户端透传 429） | `.[0].statusCode = 429` (行 15) |
| **内嵌业务码** | 限流状态 (`reason: "rate_limited"`) | `.[0].reason = "rate_limited"` (行 16) |
| **有效增量统计** | 正文字符 null，思考字符 null，输出增量 0 | `.[0].textDeltaChars = null` (行 12), `reasoningDeltaChars = null` (行 13) |
| **结束形态** | 请求失败 (`event: "model.request.failed"`) | `.[0].event = "model.request.failed"` (行 4) |
| **① 请求发出** | **数据缺失**（未显式记录原始发出字段；按 `time - durationMs` 计算推导基线为 `02:44:01.904Z`） | `.[0].durationMs = 94894` (行 8), `.[0].time` (行 3) |
| **② HTTP 响应** | **数据缺失**（客户端 SDK 未分离 HTTP 响应头到达时间） | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失**（限流无增量输出） | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失**（限流无增量输出） | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-22T02:45:36.798Z**（总耗时 94,894 ms） | `.[0].time` (行 3), `.[0].durationMs = 94894` (行 8) |

---

#### 会话 S04：Global 会话轮次 1 (`session_dd1c4aa5...` 限流)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_dd1c4aa5bb0917d81c9ce91bd4bd531f` / 来源：`main_turn` | `zcode-qoder-20261022-blank-stop-evidence.json:[1]` (行 18-33) |
| **通信协议** | Anthropic 协议（经本地网关 58892） | `QUEUE-FIX.md:17-18` |
| **服务区域 / 模型** | Global / `qoder-global:dfmodel` | `.[1].model` (行 22) |
| **HTTP 状态** | **HTTP 429** | `.[1].statusCode = 429` (行 31) |
| **内嵌业务码** | 限流状态 (`reason: "rate_limited"`) | `.[1].reason = "rate_limited"` (行 32) |
| **有效增量统计** | 正文字符 null，思考字符 null，输出增量 0 | `.[1].textDeltaChars = null` (行 28), `reasoningDeltaChars = null` (行 29) |
| **结束形态** | 请求失败 (`event: "model.request.failed"`) | `.[1].event = "model.request.failed"` (行 20) |
| **① 请求发出** | **数据缺失**（推导计算值：`02:43:42.564Z`） | `.[1].durationMs = 119192` (行 24), `.[1].time` (行 19) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失** | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失** | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-22T02:45:41.756Z**（总耗时 119,192 ms） | `.[1].time` (行 19), `.[1].durationMs = 119192` (行 24) |

---

#### 会话 S05：Global 会话轮次 2（Blank-Stop 关键样本）

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_dd1c4aa5bb0917d81c9ce91bd4bd531f` / 来源：`main_turn` | `zcode-qoder-20261022-blank-stop-evidence.json:[2]` (行 34-49) |
| **通信协议** | Anthropic 协议（经本地网关 58892） | `QUEUE-FIX.md:17-18` |
| **服务区域 / 模型** | Global / `qoder-global:dfmodel` | `.[2].model` (行 38) |
| **HTTP 状态** | **HTTP 200**（客户端流正常完成） | `.[2].statusCode = null` (行 47), `.[2].emittedError = false` (行 46) |
| **内嵌业务码** | **无错误** (`statusCode: null, reason: null`) | `.[2].statusCode` (行 47), `.[2].reason` (行 48) |
| **有效增量统计** | 正文字符 **573 chars**，思考字符 **58 chars**，工具调用 **0 次** | `.[2].textDeltaChars = 573` (行 44), `.[2].reasoningDeltaChars = 58` (行 45), `.[2].toolCallCount = 0` (行 43) |
| **结束形态** | **正常停止** (`finish: "stop"`, `rawFinish: "end_turn"`) | `.[2].finish = "stop"` (行 41), `.[2].rawFinish = "end_turn"` (行 42) |
| **① 请求发出** | **数据缺失**（推导计算值：`02:46:11.769Z`） | `.[2].durationMs = 7897` (行 40), `.[2].time` (行 35) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失**（SDK 仅聚合总量，未记录首包毫秒戳） | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失**（SDK 未记录尾包毫秒戳） | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-22T02:46:19.666Z**（流耗时 7,897 ms） | `.[2].time` (行 35), `.[2].durationMs = 7897` (行 40) |

---

#### 会话 S06：CN 会话轮次 2 (`session_48d4d720...` 正常工具调用)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_48d4d720060f80547bf9d80f5eb9c0f4` / 来源：`main_turn` | `zcode-qoder-20261022-blank-stop-evidence.json:[3]` (行 50-65) |
| **通信协议** | Anthropic 协议（经本地网关 58892） | `QUEUE-FIX.md:17-18` |
| **服务区域 / 模型** | CN / `qoder-cn:dfmodel` | `.[3].model` (行 54) |
| **HTTP 状态** | **HTTP 200** | `.[3].statusCode = null` (行 63), `.[3].emittedError = false` (行 62) |
| **内嵌业务码** | **无错误** | `.[3].statusCode` (行 63), `.[3].reason` (行 64) |
| **有效增量统计** | 正文字符 **119 chars**，思考字符 **4,374 chars**，工具调用 **2 次** | `.[3].textDeltaChars = 119` (行 60), `.[3].reasoningDeltaChars = 4374` (行 61), `.[3].toolCallCount = 2` (行 59) |
| **结束形态** | **工具调用结束** (`finish: "tool-calls"`, `rawFinish: "tool_use"`)，且随后触发并完成了 2 次工具实际执行 | `.[3].finish = "tool-calls"` (行 57), `.[3].rawFinish = "tool_use"` (行 58); `.[4, 6, 7, 8]` 记录工具执行完整闭环 |
| **① 请求发出** | **数据缺失**（推导计算值：`02:46:06.819Z`） | `.[3].durationMs = 36069` (行 56), `.[3].time` (行 51) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失** | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失** | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-22T02:46:42.888Z**（流耗时 36,069 ms） | `.[3].time` (行 51), `.[3].durationMs = 36069` (行 56) |

---

#### 会话 S07：Global 会话轮次 3 (`session_dd1c4aa5...` 标题生成超时中断)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_dd1c4aa5bb0917d81c9ce91bd4bd531f` / 来源：`session_title` | `zcode-qoder-20261022-blank-stop-evidence.json:[5]` (行 82-97) |
| **通信协议** | Anthropic 协议（经本地网关 58892） | `QUEUE-FIX.md:17-18` |
| **服务区域 / 模型** | Global / `qoder-global:dfmodel` | `.[5].model` (行 86) |
| **HTTP 状态** | **数据缺失**（客户端主动取消，未记录最终 HTTP 状态） | `.[5].statusCode = null` (行 95) |
| **内嵌业务码** | 客户端取消 (`reason: "cancelled"`) | `.[5].reason = "cancelled"` (行 96) |
| **有效增量统计** | 正文字符 null，思考字符 null，增量 0 | `.[5].textDeltaChars = null` (行 92), `reasoningDeltaChars = null` (行 93) |
| **结束形态** | **首包前中断**（在 60,009 ms 达到客户端超时阈值，取消请求） | `.[5].durationMs = 60009` (行 88), `.[5].reason = "cancelled"` (行 96) |
| **① 请求发出** | **数据缺失**（推导计算值：`02:46:20.367Z`） | `.[5].durationMs = 60009` (行 88), `.[5].time` (行 83) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失**（首字到达前已中断） | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失**（首字到达前已中断） | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-22T02:47:20.376Z**（耗时 60,009 ms） | `.[5].time` (行 83), `.[5].durationMs = 60009` (行 88) |

---

#### 会话 S08：CN 会话轮次 3 (`session_48d4d720...` 正常多轮工具调用)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_48d4d720060f80547bf9d80f5eb9c0f4` / 来源：`main_turn` | `zcode-qoder-20261022-blank-stop-evidence.json:[9]` (行 146-161) |
| **通信协议** | Anthropic 协议（经本地网关 58892） | `QUEUE-FIX.md:17-18` |
| **服务区域 / 模型** | CN / `qoder-cn:dfmodel` | `.[9].model` (行 150) |
| **HTTP 状态** | **HTTP 200** | `.[9].statusCode = null` (行 159), `.[9].emittedError = false` (行 158) |
| **内嵌业务码** | **无错误** | `.[9].statusCode` (行 159), `.[9].reason` (行 160) |
| **有效增量统计** | 正文字符 **124 chars**，思考字符 **278 chars**，工具调用 **1 次** | `.[9].textDeltaChars = 124` (行 156), `.[9].reasoningDeltaChars = 278` (行 157), `.[9].toolCallCount = 1` (行 155) |
| **结束形态** | **工具调用结束** (`finish: "tool-calls"`, `rawFinish: "tool_use"`)，且随后成功执行工具 | `.[9].finish = "tool-calls"` (行 153), `.[9].rawFinish = "tool_use"` (行 154); `.[10, 11]` 工具执行完成 |
| **① 请求发出** | **数据缺失**（推导计算值：`02:47:24.940Z`） | `.[9].durationMs = 5072` (行 152), `.[9].time` (行 147) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失** | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失** | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-22T02:47:30.012Z**（耗时 5,072 ms） | `.[9].time` (行 147), `.[9].durationMs = 5072` (行 152) |

---

#### 会话 S09：Global 会话轮次 4 (`session_dd1c4aa5...` 记忆提取限流)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_dd1c4aa5bb0917d81c9ce91bd4bd531f` / 来源：`project_memory_extract` | `zcode-qoder-20261022-blank-stop-evidence.json:[12]` (行 194-209) |
| **通信协议** | Anthropic 协议（经本地网关 58892） | `QUEUE-FIX.md:17-18` |
| **服务区域 / 模型** | Global / `qoder-global:dfmodel` | `.[12].model` (行 198) |
| **HTTP 状态** | **HTTP 429** | `.[12].statusCode = 429` (行 207) |
| **内嵌业务码** | 限流状态 (`reason: "rate_limited"`) | `.[12].reason = "rate_limited"` (行 208) |
| **有效增量统计** | 正文字符 null，思考字符 null，增量 0 | `.[12].textDeltaChars = null` (行 204), `reasoningDeltaChars = null` (行 205) |
| **结束形态** | 请求失败 (`event: "model.request.failed"`) | `.[12].event = "model.request.failed"` (行 196) |
| **① 请求发出** | **数据缺失**（推导计算值：`02:46:21.354Z`） | `.[12].durationMs = 116313` (行 200), `.[12].time` (行 195) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失** | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失** | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-22T02:48:17.667Z**（总耗时 116,313 ms） | `.[12].time` (行 195), `.[12].durationMs = 116313` (行 200) |

---

#### 会话 S10：CN 会话轮次 4 (`session_48d4d720...` 后续限流)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_48d4d720060f80547bf9d80f5eb9c0f4` / 来源：`main_turn` | `zcode-qoder-20261022-blank-stop-evidence.json:[13]` (行 210-225) |
| **通信协议** | Anthropic 协议（经本地网关 58892） | `QUEUE-FIX.md:17-18` |
| **服务区域 / 模型** | CN / `qoder-cn:dfmodel` | `.[13].model` (行 214) |
| **HTTP 状态** | **HTTP 429** | `.[13].statusCode = 429` (行 223) |
| **内嵌业务码** | 限流状态 (`reason: "rate_limited"`) | `.[13].reason = "rate_limited"` (行 224) |
| **有效增量统计** | 正文字符 null，思考字符 null，增量 0 | `.[13].textDeltaChars = null` (行 220), `reasoningDeltaChars = null` (行 221) |
| **结束形态** | 请求失败 (`event: "model.request.failed"`) | `.[13].event = "model.request.failed"` (行 212) |
| **① 请求发出** | **数据缺失**（推导计算值：`02:47:31.955Z`） | `.[13].durationMs = 94972` (行 216), `.[13].time` (行 211) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失** | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失** | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-22T02:49:06.927Z**（总耗时 94,972 ms） | `.[13].time` (行 211), `.[13].durationMs = 94972` (行 216) |

---

### 1.3 2026-10-21 队列修复后活体探针（Anthropic 入口）

#### 会话 S11：dfmodel 首轮生成探针

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | 活体网关探测行 1 | `reports/queue-live-probe.jsonl:1` |
| **通信协议** | Anthropic SSE 协议 (`protocol: "anthropic"`) | `.protocol` (行 1) |
| **服务区域 / 模型** | CN (网关默认) / `dfmodel` | `.model` (行 1) |
| **HTTP 状态** | **HTTP 200** | `.httpStatus = 200` (行 1) |
| **内嵌业务码** | **无错误** (`error: null, errorEvents: 0`) | `.error` (行 1), `.errorEvents = 0` (行 1) |
| **有效增量统计** | 响应载荷 **1,346 Bytes** | `.responseBytes = 1346` (行 1) |
| **结束形态** | **正常停止** (`messageStops: 1`) | `.messageStops = 1` (行 1) |
| **① 请求发出** | **数据缺失**（未记录分段请求发出戳） | 原始日志字段缺位 |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失** | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失** | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-20T16:12:39.101499800+00:00** (UTC) | `.time` (行 1) |

---

#### 会话 S12：qfmodel 首轮生成探针

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | 活体网关探测行 2 | `reports/queue-live-probe.jsonl:2` |
| **通信协议** | Anthropic SSE 协议 (`protocol: "anthropic"`) | `.protocol` (行 2) |
| **服务区域 / 模型** | CN (网关默认) / `qfmodel` | `.model` (行 2) |
| **HTTP 状态** | **HTTP 200** | `.httpStatus = 200` (行 2) |
| **内嵌业务码** | **无错误** (`error: null, errorEvents: 0`) | `.error` (行 2), `.errorEvents = 0` (行 2) |
| **有效增量统计** | 响应载荷 **885 Bytes** | `.responseBytes = 885` (行 2) |
| **结束形态** | **正常停止** (`messageStops: 1`) | `.messageStops = 1` (行 2) |
| **① 请求发出** | **数据缺失** | 原始日志字段缺位 |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失** | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失** | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-20T16:12:40.495747100+00:00** (UTC) | `.time` (行 2) |

---

#### 会话 S13：dfmodel 渲染校验补发探针

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | 补发渲染探针（关联原始流：`queue-live-dfmodel.sse`） | `reports/queue-live-probe-rendered.jsonl:1`; `reports/queue-live-dfmodel.sse` |
| **通信协议** | Anthropic SSE 协议 (`protocol: "anthropic"`) | `queue-live-probe-rendered.jsonl:1` |
| **服务区域 / 模型** | CN (网关默认) / `dfmodel` | `.model` (行 1) |
| **HTTP 状态** | **HTTP 200** | `.httpStatus = 200` (行 1) |
| **内嵌业务码** | **无错误** (`error: null, errorEvents: 0`) | `.error` (行 1), `.errorEvents = 0` (行 1) |
| **有效增量统计** | 渲染字符 **16 chars**（文本为 `GATEWAY_PROBE_OK`），响应载荷 **1,345 Bytes**，工具块 **0** | `.renderedCharacters = 16` (行 1), `.responseBytes = 1345` (行 1), `.toolBlocks = 0` (行 1) |
| **结束形态** | **正常停止** (`stop_reason: "end_turn"`, `message_stop`)，带诊断标记 | `.hasDiagnosticMarker = true` (行 1); `queue-live-dfmodel.sse:29, 32` |
| **① 请求发出** | **数据缺失** | 原始日志字段缺位 |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失**（原始 SSE 分片为 `G`、`ATE`、`WAY`、`_PRO`、`BE`、`_OK`，无相对时间戳） | `queue-live-dfmodel.sse:8-24` |
| **④ 最后有效增量** | **数据缺失** | `queue-live-dfmodel.sse:23` |
| **⑤ 终态到达** | **2026-10-20T16:16:56.590926300+00:00** (UTC) | `queue-live-probe-rendered.jsonl:1` |

---

### 1.4 2026-10-21 历史排队合成错误对照（旧网关 Bug 样本）

#### 会话 S14：历史空结束会话 1 (`turn_ea197ed7...`)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_b4ac5e96a587aff29a5f9aaedf1c7020` / `turn_ea197ed7-e0c0-43ca-80b3-fd24200f5c5c` | `reports/queue-evidence-summary.json:.zcodeMainTurn[0]` (行 185-196) |
| **通信协议** | Anthropic SSE 协议（经本地网关 58892） | `.[0].providerKind = "anthropic"`, `.[0].transport = "sse"` |
| **服务区域 / 模型** | CN / `qoder-cn:dfmodel` | `.[0].modelId = "qoder-cn:dfmodel"` (行 193) |
| **HTTP 状态** | **上游返回 502**（旧网关转成 HTTP 200 发送给客户端） | `.[0].status = "completed"`; `reports/QUEUE-FIX.md:15-16` |
| **内嵌业务码** | **业务错误 10605 / CALL_COUNT_LIMIT** (`queued: true`) | `.upstreamRequests[1].businessCode = "10605"`, `.subCode = "CALL_COUNT_LIMIT"` (行 23-25) |
| **有效增量统计** | 上游完成 token 0，客户端接收可见文本 **0 chars**（伪造空帧） | `.upstreamRequests[1].completionTokens = 0` (行 10); `QUEUE-FIX.md:16` |
| **结束形态** | **伪造正常结束**（网关丢弃 error 帧，合成 `message_start → end_turn → message_stop`） | `QUEUE-FIX.md:5-7, 18-19`; `.[0].status = "completed"` (行 191) |
| **① 请求发出** | **数据缺失**（上游网关记录时刻为 CST `2026-10-20T23:57:01.925+08:00`，上游耗时 251 ms） | `.upstreamRequests[1].atCst` (行 16), `.latencyMs = 251` (行 20) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失**（无真实增量输出） | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失**（无真实增量输出） | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-20T15:57:01.929Z**（客户端记录耗时 271 ms） | `.[0].timestamp` (行 186), `.[0].durationMs = 271` (行 190) |

---

#### 会话 S15：历史空结束会话 2 (`turn_ef182afb...`)

| 维度字段 | 取证数据与具体值 | 证据来源坐标与依据 |
| :--- | :--- | :--- |
| **会话标识** | `session_b4ac5e96a587aff29a5f9aaedf1c7020` / `turn_ef182afb-71b4-41aa-b496-ba6093510883` | `reports/queue-evidence-summary.json:.zcodeMainTurn[3]` (行 222-232) |
| **通信协议** | Anthropic SSE 协议（经本地网关 58892） | `.[3].providerKind = "anthropic"`, `.[3].transport = "sse"` |
| **服务区域 / 模型** | CN / `qoder-cn:dfmodel` | `.[3].modelId = "qoder-cn:dfmodel"` (行 229) |
| **HTTP 状态** | **上游返回 502**（旧网关转成 HTTP 200 发送给客户端） | `.[3].status = "completed"`; `reports/QUEUE-FIX.md:15-16` |
| **内嵌业务码** | **业务错误 10605 / CALL_COUNT_LIMIT** (`queued: true`) | `.upstreamRequests[11].businessCode = "10605"`, `.subCode = "CALL_COUNT_LIMIT"` (行 143-145) |
| **有效增量统计** | 上游完成 token 0，客户端接收可见文本 **0 chars**（伪造空帧） | `.upstreamRequests[11].completionTokens = 0` (行 142); `QUEUE-FIX.md:16` |
| **结束形态** | **伪造正常结束**（网关丢弃 error 帧，合成 `message_start → end_turn → message_stop`） | `QUEUE-FIX.md:5-7, 18-19`; `.[3].status = "completed"` (行 227) |
| **① 请求发出** | **数据缺失**（上游网关记录时刻为 CST `2026-10-20T23:57:39.080+08:00`，上游耗时 735 ms） | `.upstreamRequests[11].atCst` (行 136), `.latencyMs = 735` (行 140) |
| **② HTTP 响应** | **数据缺失** | 原始日志字段缺位 |
| **③ 首次有效增量** | **数据缺失**（无真实增量输出） | 原始日志字段缺位 |
| **④ 最后有效增量** | **数据缺失**（无真实增量输出） | 原始日志字段缺位 |
| **⑤ 终态到达** | **2026-10-20T15:57:39.084Z**（客户端记录耗时 749 ms） | `.[3].timestamp` (行 222), `.[3].durationMs = 749` (行 226) |

---

## 2. 三分类汇总（排队 / 断流 / 正常结束）

### 2.1 判定基线与分类准则

依据任务指导规范，所有分析会话必须且仅能归入以下三分类之一：
1. **排队 (Queuing)**：
   - **严格判据**：物理连接建立且 HTTP 响应到达（HTTP 200 或网关透传的 429/502），但响应体内嵌业务限流错误码（`10605 / CALL_COUNT_LIMIT` 或 `businessErrorStatus: 429 / rate_limited`），且**整个请求生命周期内没有任何有效输出增量**（正文、思考或工具参数均为 0）。
2. **断流 (Stream Break)**：
   - **严格判据**：请求发出后，**在首个有效增量（First Effective Delta）到达之前**由于网络连接断开、协议解析严重损坏、或客户端/服务端超时机制主动终止，导致会话异常中断。
3. **正常结束 (Normal End)**：
   - **严格判据**：响应流完整生成，明确携带 `end_turn` 或 `tool_use` 终止标记，且具备**客观行为证据**（如客户端正确渲染文本、或成功解析工具定义并随后实际发起了工具调用闭环）。

---

### 2.2 全量会话分类汇总表

下表汇总全部 15 个取证会话的归类结果、判定依据与原始证据出处：

| 会话编号 | 会话简述与所属集合 | 区域与模型 | 最终分类 | 判定核心依据 | 证据来源坐标与关键值 |
| :---: | :--- | :--- | :---: | :--- | :--- |
| **S01** | CN 独立诊断探针 | CN / `dfmodel` | **排队** | HTTP 200 + 内嵌业务 429，有效输出 0 字节 | `raw-cn-tool:5, 14, 17` (`businessErrorStatus: 429`) |
| **S02** | Global 独立诊断探针 | Global / `dfmodel` | **排队** | HTTP 200 + 内嵌业务 429，有效输出 0 字节 | `raw-global-tool:5, 14, 17` (`businessErrorStatus: 429`) |
| **S03** | CN ZCode 会话轮次 1 | CN / `dfmodel` | **排队** | 透传 429 `rate_limited`，无有效输出 | `zcode-evidence:[0]` (行 15-16, `durationMs: 94894`) |
| **S04** | Global ZCode 会话轮次 1 | Global / `dfmodel` | **排队** | 透传 429 `rate_limited`，无有效输出 | `zcode-evidence:[1]` (行 31-32, `durationMs: 119192`) |
| **S05** | Global ZCode 会话轮次 2 | Global / `dfmodel` | **正常结束** | `end_turn` + 573 字符正文与 58 字符思考行为证据 | `zcode-evidence:[2]` (行 41-45, `textChars: 573`) |
| **S06** | CN ZCode 会话轮次 2 | CN / `dfmodel` | **正常结束** | `tool_use` + 2 次工具调用发起并成功执行闭环 | `zcode-evidence:[3]` (行 58-61), `.[4, 6, 7, 8]` |
| **S07** | Global 标题生成轮次 | Global / `dfmodel` | **断流** | 首个有效增量前中断（60 秒客户端超时主动 Cancel） | `zcode-evidence:[5]` (行 88, 96, `durationMs: 60009`) |
| **S08** | CN ZCode 会话轮次 3 | CN / `dfmodel` | **正常结束** | `tool_use` + 1 次工具调用发起并成功执行闭环 | `zcode-evidence:[9]` (行 154-157), `.[10, 11]` |
| **S09** | Global 记忆提取轮次 | Global / `dfmodel` | **排队** | 透传 429 `rate_limited`，无有效输出 | `zcode-evidence:[12]` (行 207-208, `durationMs: 116313`) |
| **S10** | CN ZCode 会话轮次 4 | CN / `dfmodel` | **排队** | 透传 429 `rate_limited`，无有效输出 | `zcode-evidence:[13]` (行 223-224, `durationMs: 94972`) |
| **S11** | 队列修复后活体探针 1 | CN / `dfmodel` | **正常结束** | HTTP 200 + 1,346 字节输出 + `messageStops: 1` | `queue-live-probe.jsonl:1` (`responseBytes: 1346`) |
| **S12** | 队列修复后活体探针 2 | CN / `qfmodel` | **正常结束** | HTTP 200 + 885 字节输出 + `messageStops: 1` | `queue-live-probe.jsonl:2` (`responseBytes: 885`) |
| **S13** | 队列修复渲染校验探针 | CN / `dfmodel` | **正常结束** | 16 字符诊断标记 `GATEWAY_PROBE_OK` 完整拼接 | `queue-live-probe-rendered.jsonl:1`; `queue-live-dfmodel.sse:29` |
| **S14** | 历史排队合成错误样本 1 | CN / `dfmodel` | **排队** | 上游 10605 CALL_COUNT_LIMIT 排队，0 真实输出 | `queue-evidence-summary.json:.upstreamRequests[1]` |
| **S15** | 历史排队合成错误样本 2 | CN / `dfmodel` | **排队** | 上游 10605 CALL_COUNT_LIMIT 排队，0 真实输出 | `queue-evidence-summary.json:.upstreamRequests[11]` |

---

### 2.3 分类统计汇总

对上述分析样本进行多维度交叉统计，结果如下：

| 数据集合维度 | 样本总数 | 排队 (Queuing) | 断流 (Stream Break) | 正常结束 (Normal End) |
| :--- | :---: | :---: | :---: | :---: |
| **全量分析会话总计** | **15** | **8** (53.3%) | **1** (6.7%) | **6** (40.0%) |
| **2026-10-22 实时与客户端核心取证集** (S01~S10) | **10** | **6** (60.0%) | **1** (10.0%) | **3** (30.0%) |
| **2026-10-21 队列修复活体验证集** (S11~S13) | **3** | **0** (0.0%) | **0** (0.0%) | **3** (100.0%) |
| **2026-10-21 历史 Bug 对照集** (S14~S15) | **2** | **2** (100.0%) | **0** (0.0%) | **0** (0.0%) |

#### 分类特性深层剖析：
1. **排队样本的本质共性**：
   - 无论是在受控直接探针（S01、S02）还是长会话轮次（S03、S04、S09、S10），或是历史网关日志（S14、S15），上游返回限流时**完全不吐出任何生成 Token**。排队期间等待耗时极长（客户端观察到 94~119 秒；历史日志记录上游握手虽仅耗时数百毫秒，但多次轮询均立即被拒绝）。
2. **断流样本的唯一性**：
   - 全量数据中仅有 S07（`session_title`）归入断流。其根本原因是客户端为副任务（自动提炼标题）设置了严格的 60 秒超时控制（`durationMs: 60009`）；由于上游排队阻塞未在 60 秒内产出首字，客户端 SDK 主动发出 `cancelled` 中断连接。这属于「首字前因超时中断」，而非网络通信底层异常断流。
3. **正常结束样本的结构分层**：
   - S11~S13 证明网关修复后在短请求场景下具备生成与渲染完整闭环能力；
   - S06 与 S08 证明 CN 区在获取到生成机会时能够正常输出深层思考（4,374 字符）并正确产出 `tool_use`，且客户端能接收参数并执行外部命令；
   - S05 则属于关键争议样本：它在协议与通信层归入「正常结束」，但在业务语义与用户预期层存在早停缺陷。

---

## 3. blank-stop 样本证明力说明

针对关键样本 **会话 S05**（`reports/zcode-qoder-20261022-blank-stop-evidence.json:[2]`，时间戳：`2026-10-22T02:46:19.666Z`），其在 ZCode 客户端表现为：耗时 7,897 ms，输出 573 个文本字符、58 个思考字符，未调用任何工具（`toolCallCount: 0`），收到 `stop / end_turn` 终态信号后直接停止，用户界面出现“模型仅复述了执行计划但未发起任何实际操作”的早停假象。

必须从司法取证与协议通信角度，严格界定该客户端日志的**证明力边界**：

```
                【取证证明力断层模型】
                
 ┌─────────────────┐      HTTP / SSE      ┌──────────────────┐      HTTPS / SSE      ┌─────────────────┐
 │   ZCode 客户端   │ ◄────────────────── │  BuddySwitch 网关 │ ◄─────────────────── │  Qoder 上游服务  │
 │ (SDK 聚合日志)   │   (明文 JSON-RPC)   │ (协议转换/中继)   │   (专有编码/加密)   │ (DeepSeek 引擎) │
 └─────────────────┘                      └──────────────────┘                       └─────────────────┘
         │                                         ▲                                          ▲
         │                                         │                                          │
   【现有直接证据】                         【原始帧证据缺失】                         【服务端黑盒】
  * 证明端侧收到 end_turn                  * 无法证明网关是否丢弃 tool 帧              * 无法证明模型真实输出
  * 证明端侧收到 573+58 字符               * 无法证明发往上游的参数是否完整            * 无法证明是否服务端截断
  * 证明端侧连接未报错                     * 无法证明上游 raw finish_reason           * 无法证明真实 usage
```

### 3.1 客户端记录「能证明什么」

依据现有文件条目 `zcode-qoder-20261022-blank-stop-evidence.json:[2]`，可客观确证的事实仅限以下四项：
1. **能证明客户端 SDK 完整接收到了 `end_turn` 终态信号**：
   - 字段依据：`finish = "stop"`, `rawFinish = "end_turn"`，且 `emittedError = false`。这证明在客户端与本地网关（127.0.0.1:58892）之间的通信链路中，连接既未发生 TCP RST 中断，也未触发客户端解析崩溃，网关向客户端下发了标准的正常终止协议帧。
2. **能证明模型生成了部分内容且耗时为 7.897 秒**：
   - 字段依据：`durationMs = 7897`, `textDeltaChars = 573`, `reasoningDeltaChars = 58`。这证明该请求未被上游排队直接挂死（相较于 S04 的 119 秒限流，S05 实际获得了生成调度），并在约 7.9 秒内返回了 573 个正文字符与 58 个思考字符。
3. **能证明客户端在该轮次未识别到任何工具调用指令**：
   - 字段依据：`toolCallCount = 0`。这证明 ZCode 客户端在接收到的整个响应数据块中，未检测到任何符合 Anthropic 标准的 `tool_use` 结构体，因此客户端没有发起后续工具执行动作。
4. **能证明客户端表现不同于历史排队合成 Bug**：
   - 字段依据：不同于 2026-10-21 修复前的历史 Bug（S14、S15 中上游 10605 排队导致网关伪造空 `end_turn`、输出 0 字符），S05 确实包含了 573 字符的真实生成内容，并非纯空帧合成。

---

### 3.2 客户端记录「不能证明什么」

由于该证据仅为**客户端最外层的汇总事件记录**，缺失网关与上游之间的逐帧通信原始抓包，**绝对不能将其等同于上游流的真实与完整形态**。以下结论在现有证据下均**无法证明**：
1. **不能证明上游供应商输出流本身是否完整**：
   - 客户端日志无法反映 Qoder 上游服务在向网关推流时，是因为生成完毕自发下发 `finish_reason: "stop"`，还是因为服务端触发了某种内部安全审计、上下文长度限制、计费切断或单次推流缓冲区溢出而提前关闭了连接。
2. **不能证明网关是否在转换过程中丢弃了工具调用数据**：
   - 若上游实际在 SSE 载荷中推送了工具增量帧（`tool_calls`），但网关中旧的协议转换解析器（或参数适配模块）在特定结构下发生反序列化失配或未能识别工具块，网关可能在忽略工具帧后直接向客户端下发了文本和 `end_turn`。客户端记录只能证实自己收到 0 次工具调用，无法证明上游从未发出过。
3. **不能证明上游是否因为请求参数丢失而改变了生成策略**：
   - 依据 `reports/QODER-RAW-DIAGNOSIS-20261022.md:19-25` 的代码排查结论，网关原有的 Anthropic 转换模块会丢弃 `reasoning_effort` / `thinking.effort` 等显式推理档位参数。客户端仅记录了极短的 58 字符思考（相较于 S06 正常工具调用的 4,374 字符思考存在数量级差距），无法证明模型是否是因为未接收到深层思考配置，而自发退化为“仅在正文中陈述行动计划，而不发起工具调用”。
4. **不能证明上游物理 Token 吞吐与原始完成原因**：
   - 客户端记录中 `statusCode: null` 且缺少上游原始 `usage` 元数据（无 `prompt_tokens`、`completion_tokens`、`reasoning_tokens`），无法证明上游实际消耗的 Token 数量，亦无法据此推算真实的解码吞吐速率。

---

## 4. 结论与根因分析

### 4.1 Global 提前结束根因能否区分之明确判定

> **明确判断：依据现有离线证据，绝对无法区分 Global 提前结束（Blank Stop）的根本原因。**

#### 判定依据：
现有证据链在**网关至 Qoder 上游服务之间的原始数据交互层**存在完全断裂。对于 Global 会话 S05 的提前停止现象，当前至少存在三个在技术原理上均能独立解释现有全部现象的**候选假说**，且现有证据无法排除其中任何一个：

```
                      【候选根因竞争图谱】
                      
        ┌─────────────────────────────────────────────────┐
        │  现象：Global S05 输出 573 字计划后 stop, 无工具调用  │
        └─────────────────────────────────────────────────┘
                                 │
         ┌───────────────────────┼───────────────────────┐
         ▼                       ▼                       ▼
   【假说 A：参数丢失】    【假说 B：上游差异/截断】 【假说 C：网关协议丢帧】
  网关丢弃 reasoning 参数   Global 节点系统 Prompt/   网关解析特定 chunk 异常
  模型降档，仅列计划不调工具 上下文限制, 主动提前 stop 漏解析 tool_calls, 发 end_turn
         │                       │                       │
         └───────────────────────┼───────────────────────┘
                                 │
                                 ▼
                     【现有证据均无法证实/证伪】
```

1. **假说 A：网关请求参数丢失导致模型降级，自发做出不调工具的决策**
   - *支持依据*：`reports/QODER-RAW-DIAGNOSIS-20261022.md:19-25` 证实网关旧代码确实存在丢弃 `reasoning_effort` 参数的物理缺陷。DeepSeek 模型在未开启深层思考档位时，极易退化为仅复述待办事项而不发起工具调用。S05 的思考字符仅 58 chars，与正常工具轮次 S06 的 4,374 chars 形成鲜明对照。
   - *无法确证原因*：缺少发给上游的原始加密前 JSON 请求体，无法证明该次请求是否确实未带参数；且无法证明如果带上该参数，模型就必然会调用工具。
2. **假说 B：Qoder Global 上游服务端存在行为差异或服务侧主动提前收尾**
   - *支持依据*：Global 区分发节点与 CN 区分发节点可能部署了不同版本的 Prompt 包装模版、不同的工具指令系统提示词，或 Global 节点对上下文长度设置了更严苛的软截断策略，导致模型直接产出 `stop`。
   - *无法确证原因*：缺少 Qoder Global 上游返回的原始分帧 SSE 载荷与原始 `finish_reason`，无法获知供应商底层给出的真实终止原因。
3. **假说 C：网关中转层在解析特定上游响应流时发生丢帧或提前合成**
   - *支持依据*：历史已有 S14、S15 中网关错误转换协议并合成 `end_turn` 的先例（`QUEUE-FIX.md:5-7`）。若针对某些特定分块（Chunk），网关在反序列化或编码转换时发生未捕获的忽略行为，亦可导致工具帧丢失。
   - *无法确证原因*：缺少网关处理该次请求时的原始分帧调试日志（Raw Wire Chunk Trace）。

---

### 4.2 「还差什么证据」清单

为彻底闭环区分并确证上述候选根因，后续取证**必须采集以下四项关键缺失证据**：

| 缺失证据项 | 拟采集的数据形态与获取方式 | 对候选根因的鉴别区分机制（如何证实/证伪） |
| :--- | :--- | :--- |
| **证据 1：网关发往上游的原始解密请求体镜像** (`upstream_request_payload.json`) | 网关在调用 `cosy::qoder_encode` 之前，通过诊断钩子完整落盘的原始 JSON 结构体。 | **鉴别假说 A**：核验请求体中是否包含完整的 `tools` 定义，且是否正确注入了映射后的 `reasoning_effort`。若请求体中根本缺失工具或档位参数，则**确证假说 A 成立**；若参数完全规范，则**证伪假说 A**。 |
| **证据 2：上游返回的完整逐帧原始 SSE 载荷** (`upstream_wire_frames.sse`) | 绕过协议转换逻辑，将上游 HTTP 连接收到的原始字节流逐帧分块保存，记录每一帧的到达毫秒戳与原文。 | **鉴别假说 B 与 C**：<br>① 若原始帧中包含 `delta.tool_calls`，但客户端记录中 `toolCallCount = 0`，则**确证假说 C（网关丢帧）**；<br>② 若原始最后一帧明确为 `finish_reason: "stop"` 且通篇无任何工具字段，则**证伪假说 C，缩小范围至假说 B 与 A**。 |
| **证据 3：上游原始 Usage 统计与元数据** (`raw_upstream_usage`) | 上游响应末尾携带的包含 `prompt_tokens`、`completion_tokens`、`reasoning_tokens` 的官方统计块。 | **鉴别模型计算状态**：核验实际消耗的思考 Token。若 `reasoning_tokens` 接近 0，印证模型未进入深度推理模式；核验输出 Token 是否触及 `max_tokens: 768`，以区分是模型自发停止还是触碰长度硬截断。 |
| **证据 4：修复参数映射后的同上下文受控重放探针** (`qoder_stream_timing` 重放记录) | 在当前已修复 `payload::from_anthropic` 的网关环境下，使用与 S05 完全相同的提示词与上下文向 Global 区补发一次受控探针。 | **确证参数与行为的因果关系**：若在显式带上推理档位后，Global 节点成功生成工具调用，则**因果证明参数丢失是导致早停的充分根因**；若仍然复现早停，则**排除网关参数问题，确证为上游服务固有行为**。 |

---

### 4.3 持续 429 的定性准则

> **规范红线定性**：在所有现有数据源（S01、S02、S03、S04、S09、S10）中观察到的持续 429 现象（含 HTTP 429、HTTP 200 内嵌 `businessErrorStatus: 429`、以及 `10605 / CALL_COUNT_LIMIT`），**一律严格定性为「上游阻塞」，严禁在缺乏官方账单与服务侧监控日志的前提下进行任何主观根因推断。**

#### 严禁推断的未证实假说：
1. **严禁推断为「账号被官方风控或凭据失效」**：
   - 证据反驳：`reports/qoder-raw-account-integrity.json:22` 证实探针前后账号文件 SHA-256 完全一致；且在 2026-10-21 修复前，网关曾误将 10605 排队标为 Auth 死亡，后经 `QUEUE-FIX.md:28` 明确修正：排队限流不得误标 Access Token 死亡。
2. **严禁推断为「本地网络连通性故障或代理异常」**：
   - 证据反驳：HTTP 状态均正常握手并返回 200 或 429，TCP 连接未发生断开，证明物理网络通路完好。
3. **严禁推断为「底层模型物理算力彻底耗尽或服务永久下线」**：
   - 证据反驳：在限流间歇期间（如 02:46:42 与 02:47:30），CN 区依然成功处理了长达 36 秒的深度推理会话（S06），证明服务处于动态限流或波峰排队状态，并非永久瘫痪。

---

## 5. 取证方法与局限声明

### 5.1 取证方法论与操作边界

本报告全流程遵循以下工程取证准则：
1. **纯静态离线只读分析**：
   - 全程仅检视本地工作区及 `reports/` 目录下已落盘的日志与代码补丁；
   - 未执行任何 `curl`、`fetch` 或网络探针脚本；未向任何远程服务端或本地网关端口发起实时通信。
2. **严禁执行 Git 写操作与实例变更**：
   - 未执行 `git commit`、`git stash` 或任何改变版本库分支指针的操作；
   - 未启动新的测试进程，未终止当前预览版服务进程（保持 `QODER-RAW-DIAGNOSIS-20261022.md:13` 记录的运行态不受干扰）。
3. **严格脱敏与来源可回溯**：
   - 全文禁止出现任何形式的用户 Token、凭据密钥（严禁 `dt-`、`drt-`、Bearer 等特征字符）；
   - 全文不包含任何完整的业务提示词或长篇回复正文；
   - 报告中出现的每一个状态码、耗时毫秒数、字符数及字节大小，均标注了精确的来源文件、行号或 JSON 字段路径。

---

### 5.2 「数据缺失」时间点项系统总结

按照 [PRD §6.2](file:///D:/desktop/FUSION-INTEGRATION-v1.1.md#L257-L265) 规范对照，在全部 15 个会话的取证过程中，客观缺失的时间点统计如下：

| PRD §6.2 时间点项 | 在各数据源中的存在状态 | 数据缺失的具体机理与客观原因说明 |
| :--- | :---: | :--- |
| **① 请求发出时刻** | **普遍缺失绝对时间戳** | ① 原始探针 `raw-*.json` 仅以自身启动为相对 `t = 0`，未保存绝对 UTC 毫秒戳；<br>② ZCode 客户端日志仅在请求完成时记录一个 `time` 终态戳，原始发出的网络分段戳未落盘；<br>③ 历史网关日志仅记录请求结束时的写入时刻与 `latencyMs`。 |
| **② HTTP 响应到达** | **多数缺失** | ① 仅独立诊断探针（S01、S02）通过 `headersMs` 显式记录了 HTTP 握手与响应头到达时长（196 ms / 2,113 ms）；<br>② ZCode 客户端日志与活体探针 JSONL 均未分离 HTTP 响应头到达时间点。 |
| **③ 首次有效增量** | **严重缺失** | ① 现有探针在遇到 429 排队时根本无增量输出（`.outputBytes = 0`），故无此时间点；<br>② ZCode 客户端仅在整个 Stream 结束时聚合字符总数，未捕获首包流到达的相对/绝对时间；<br>③ `queue-live-dfmodel.sse` 为纯文本流，未附带每帧到达的毫秒级时间戳。 |
| **④ 最后有效增量** | **严重缺失** | 缺失机理同上。在缺乏逐帧微秒级 Wire Trace 的情况下，无法剥离最后有效增量与后续 `[DONE]` 帧之间的网络滑窗等待间隔。 |
| **⑤ 终态到达时刻** | **完整具备** | 所有 15 个会话均具备明确的终态记录（探针提供 `elapsedMs`，JSONL 与客户端日志提供 ISO 8601 UTC 绝对时间戳）。 |

---

### 5.3 取证局限声明

本报告的分析结论受限于以下客观技术条件：
1. **探针 45 秒超时的伴生属性声明**：
   - 在原始探针数据（S01、S02）中，记录了 `readFailed: true` 且 `elapsedMs` 分别为 45,205 ms 与 47,126 ms。依据 `reports/QODER-RAW-DIAGNOSIS-20261022.md:15` 的诊断记录，这是由于初代测试探针在判定业务错误后未立即主动断开，而是继续读取直到触及预设的 45 秒客户端读取超时上限（`read_timeout(Duration::from_secs(45))`）。**该 `readFailed` 属于限流发生后的超时附带现象，严禁将其解读为流式生成过程中的意外传输断流**。
2. **Token 生成速率无法计算声明**：
   - 依据 [PRD §6.2](file:///D:/desktop/FUSION-INTEGRATION-v1.1.md#L262)，在缺乏上游官方实际 `usage` 返回值时，物理 Token/秒速率必须记为 `null`。当前数据中两区探针由于处于排队限流状态，输出为 0，实际吞吐速率为 `null`；严禁依据两区展示名 `DeepSeek-Flash` 擅自编造或承诺 200 token/秒的物理吞吐。
3. **样本时效性与外推局限**：
   - 本报告所有数据均为特定测试窗口（2026-10-20 至 2026-10-22）的历史离线快照，仅反映对应时刻网关与供应商分发节点的局部链路表现，不能代表供应商当前或未来的整体可用性与吞吐水位。
