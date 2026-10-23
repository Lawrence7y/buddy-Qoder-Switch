//! 管理数据模型与投影契约（FUS-W02 / W03 / W04）。
//!
//! 统一积分工作台、到期日历、积分包明细、历史观测与任务中心的结构化契约。
//! 遵循 PRD §5.2、§6.1、§9.1、§10：
//! - 复合身份严格按 `(scope, account_id)` 建模，未知 scope fail-closed；
//! - 缺失值显式为 None / "未提供"，禁止用 0 或 "永久有效" 替代；
//! - 官方账单、快照差分与估算分列，不可比/未知不造假；
//! - 任务运行态与奖励结果分离，明确标注任务执行引擎交付状态。

use serde::{Deserialize, Serialize};

/// 账号元数据引用快照（不存凭证，仅作身份聚合与显示名缓存）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountRef {
    pub scope: String,
    pub account_id: String,
    pub display_name: String,
    pub is_active: bool,
    pub updated_at_ms: i64,
}

/// 单次余额观测快照（来自官方余额接口或本地观测）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreditObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub scope: String,
    pub account_id: String,
    pub source: String,
    pub remaining: f64,
    pub total: f64,
    pub unit: String,
    pub is_stale: bool,
    pub is_comparable: bool,
    pub observed_at_ms: i64,
}

/// 单个积分包明细（前后端权威契约完整对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreditPackageItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub scope: String,
    pub account_id: String,
    pub account_name: String,
    pub package_id: String,
    pub name: String,
    /// 前端别名
    pub package_name: String,
    /// 类别：plan（套餐/订阅周期）、addon（附加包）、activity（活动奖励）、custom（专属）
    pub package_type: String,
    pub remaining: f64,
    /// 前端别名
    pub remaining_balance: f64,
    pub total: f64,
    /// 前端别名
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_capacity: Option<f64>,
    pub unit: String,
    /// 到期时间戳毫秒。None 表示上游未提供到期时间（前端显示“未提供”，不写“永久”）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_at_ms: Option<i64>,
    /// 前端秒时间戳
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_time: Option<i64>,
    /// 格式化日期 YYYY-MM-DD
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_time_formatted: Option<String>,
    /// 是否为本地推断包（如从无逐包明细的快照中派生）
    pub is_inferred: bool,
    /// 是否属于历史缓存或更新失败保留态
    pub is_stale: bool,
    /// 来源说明（official:官方接口 / snapshot:本地快照 / inferred:推断）
    pub source: String,
    pub source_type: String,
    pub observed_at_ms: i64,
    pub observed_at: i64,
    pub status: String,
}

/// 积分变动或相关活动事件（入账/消耗/过期/签到奖励）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreditEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub scope: String,
    pub account_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// 事件类型：usage（消耗）、checkin（签到入账）、reward（奖励）、recharge（充值）、expired（过期）
    pub event_kind: String,
    pub event_type: String,
    pub amount: f64,
    pub change_amount: f64,
    pub balance_after: f64,
    pub unit: String,
    pub occurred_at_ms: i64,
    pub recorded_at: i64,
    /// 证据等级：official_api（官方消费明细）/ snapshot_diff（本地快照下降推算）/ checkin_receipt（签到服务端回执）
    pub evidence_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 到期日历中的单个具体到期条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExpiryCalendarItem {
    pub key: String,
    pub scope: String,
    pub account_id: String,
    pub account_name: String,
    pub package_id: String,
    pub package_name: String,
    pub package_type: String,
    pub remaining: f64,
    pub amount: f64,
    pub total: f64,
    pub unit: String,
    pub expire_at_ms: i64,
    pub expire_time: i64,
    pub days_remaining: i64,
}

/// 到期日历按日聚合的节点（支持点击筛选当日到期积分包）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExpiryCalendarDateGroup {
    /// 业务日 YYYY-MM-DD
    pub date: String,
    pub timestamp_ms: i64,
    /// 距离当前自然日的天数（<=0 当日到期或已过期，1-7 临期，>7 正常）
    pub days_until: i64,
    pub days_remaining: i64,
    /// 当日到期的积分总额
    pub total_expiring: f64,
    /// 当日到期的包数量
    pub count: usize,
    pub package_count: usize,
    /// 当日到期的各包明细
    pub items: Vec<ExpiryCalendarItem>,
}

/// 状态与数据源标记（完全兼容两种前端字段体系）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManagementDataStatus {
    pub is_stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_ms: Option<i64>,
    /// official: 官方数据优先；snapshot: 本地观察估算；mixed: 混用；unknown: 未知
    pub source_kind: String,
    /// live | cached | stale | unsupported | unobserved | failed
    pub code: String,
    pub message: String,
    pub observed_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_remaining_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 4 格统计指标聚合（同时满足多重视图与前端消费）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreditSummaryStats {
    pub current_remaining: f64,
    pub total_remaining_balance: f64,
    pub total_original_capacity: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_in: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_out: Option<f64>,
    pub expiring_soon: f64,
    pub expiring_within7_days_count: usize,
    pub expiring_within7_days_amount: f64,
    pub currency: String,
    pub total_accounts: usize,
    pub active_accounts: usize,
    pub total_packages: usize,
    /// 标记是否为已知已观测到的真实数据（未取到时为 false，不可伪装为已确认 0 余额）
    pub is_known: bool,
}

/// 统一积分工作台投影总览（FUS-R02 / R03 / R04 查询接口回执）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreditSummaryProjection {
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_account: Option<String>,
    pub summary: CreditSummaryStats,
    /// 兼容前端 stats 别名，确保无论通过 summary 还是 stats 读取均一致
    pub stats: CreditSummaryStats,
    pub packages: Vec<CreditPackageItem>,
    pub calendar: Vec<ExpiryCalendarDateGroup>,
    pub observations: Vec<CreditObservation>,
    pub events: Vec<CreditEvent>,
    /// 兼容前端 recentEvents 别名
    pub recent_events: Vec<CreditEvent>,
    pub status: ManagementDataStatus,
}

// ---------------------------------------------------------------------------
// 任务中心模型（FUS-W02 原型与契约）
// ---------------------------------------------------------------------------

/// 自动/手动任务定义契约。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskSpec {
    pub id: String,
    pub task_id: String,
    pub scope: String,
    pub name: String,
    pub description: String,
    pub task_kind: String,
    pub trigger_type: String,
    pub account_scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_cron: Option<String>,
    pub default_schedule: String,
    pub timezone: String,
    pub enabled: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_policy: Option<String>,
    /// 交付说明与扩展元数据（包含明确标注执行引擎尚未交付的契约说明）
    pub metadata: serde_json::Value,
    pub updated_at_ms: i64,
}

/// 任务运行记录（FUS-W02 / 状态机契约）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskRun {
    pub run_id: String,
    pub task_id: String,
    pub scope: String,
    pub task_kind: String,
    pub trigger_source: String,
    pub triggered_by: String,
    /// queued, running, waiting_upstream, waiting_verification, completed, failed, cancelled
    pub status: String,
    pub started_at_ms: i64,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    pub total_items: usize,
    pub success_items: usize,
    pub failed_items: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
}

/// 任务中逐账号执行项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub run_id: String,
    pub scope: String,
    pub account_id: String,
    pub stage: String,
    /// confirmed, already_claimed, pending, running, waiting_upstream, waiting_verification, pending_confirmation, failed, unsupported, skipped, cancelled
    pub status: String,
    pub started_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub retry_count: i32,
}

/// 任务生命周期持久化事件（FUS-W06）。
///
/// 包含单调递增的序列 sequence / id，关联 runId 与作用域。
/// 严格过滤：不保存凭据、prompt、敏感响应或任意原始 result。
/// 明确：旧 TaskRun.cursor 仍是处理账号计数，与这里的单调事件游标 id/sequence 完全分离。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskEvent {
    pub id: i64,
    /// 单调递增序号（对齐 PRD sequence 契约，与 id 等值，供前端按 sequence 去重）
    #[serde(default)]
    pub sequence: i64,
    pub run_id: String,
    pub scope: String,
    pub account_id: String,
    pub stage: String,
    pub status: String,
    pub occurred_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 游标增量分页查询任务事件的回执（FUS-W06）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskEventPage {
    pub items: Vec<TaskEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<i64>,
    pub has_more: bool,
}
