//! Agent orchestration support.
//!
//! Keep this module isolated from the core review TUI so this fork can rebase
//! upstream while the PR automation grows.

pub mod ci;
pub mod ci_adapters;
pub mod ci_retries;
pub mod dashboard;
pub mod dashboard_tui;
pub mod dispatch;
pub mod feedback;
pub mod github_actions;
pub mod notification;
pub mod pr_list;
pub mod prs_cli;
pub mod state;
pub mod watch;
