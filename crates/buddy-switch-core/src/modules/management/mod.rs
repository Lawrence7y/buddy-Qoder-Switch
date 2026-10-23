//! 管理与编排层（FUS-W02 / W03 / W04）。
//!
//! 包含统一积分工作台持久化与投影（FUS-R02/R03/R04）、
//! 可点击到期日历（FUS-A06）、
//! 以及任务中心原型与契约固化（FUS-W02）。

pub mod credits;
pub mod models;
pub mod runtime;
pub mod store;
pub mod tasks;

#[cfg(test)]
mod tests;

pub use credits::{project_credit_summary, refresh_credits, validate_scope};
pub use models::{
    AccountRef, CreditEvent, CreditObservation, CreditPackageItem, CreditSummaryProjection,
    CreditSummaryStats, ExpiryCalendarDateGroup, ExpiryCalendarItem, ManagementDataStatus,
    TaskEvent, TaskEventPage, TaskItem, TaskRun, TaskSpec,
};
pub use store::{db_path, ensure_schema, open_db};
pub use tasks::{
    cancel_task_run, cancel_task_run_in_conn, get_task_events, get_task_items, get_task_runs,
    get_task_specs, retry_task_run, retry_task_run_with_handler, save_task_spec, trigger_task,
    trigger_task_with_handler, OfflineTaskActionHandler, TaskActionHandler,
    TaskItemExecutionResult,
};
