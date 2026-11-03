//! Notification decision logic (port of Go `utils/notifier`).

pub mod expire;
pub mod load;
pub mod offline;
pub mod renewal;
pub mod traffic;

pub use expire::{ExpireAlert, ExpireCheck, check_expire, next_run};
pub use load::{
    check_metric_threshold, execute_load_task, get_metric_value, run_all_load_tasks,
    should_skip_notification,
};
pub use offline::{DueOffline, OfflineNotifier, OfflineTracker, OnlineDecision};
pub use renewal::{check_and_auto_renewal, days_left};
pub use traffic::{
    TrafficAlert, TrafficTracker, TrafficUsage, check_traffic, compute_used_by_type, human_bytes,
};
