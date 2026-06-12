use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::agent::dispatch::{DispatchReport, DispatchStatus};

const DISABLE_ENV: &str = "TUICR_NOTIFY";
const COMMAND_ENV: &str = "TUICR_NOTIFY_COMMAND";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationAttempt {
    pub delivered_at: DateTime<Utc>,
    pub sink: String,
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotificationPayload {
    title: String,
    body: String,
}

pub fn notify_run(report: &DispatchReport) -> NotificationAttempt {
    let payload = run_payload(report);
    notify_payload(&payload, report)
}

pub fn notify_custom(title: &str, body: &str) -> NotificationAttempt {
    let payload = NotificationPayload {
        title: title.to_string(),
        body: body.to_string(),
    };
    notify_payload_without_run(&payload)
}

fn notify_payload(payload: &NotificationPayload, report: &DispatchReport) -> NotificationAttempt {
    if notifications_disabled() {
        return attempt("disabled", true, "Notifications disabled by TUICR_NOTIFY=0");
    }
    if let Some(command) = configured_notification_command() {
        return run_command_hook(&command, payload, report);
    }
    if cfg!(target_os = "macos") {
        return run_osascript(payload);
    }
    attempt(
        "unsupported",
        false,
        "No notification sink configured; set TUICR_NOTIFY_COMMAND",
    )
}

fn notify_payload_without_run(payload: &NotificationPayload) -> NotificationAttempt {
    if notifications_disabled() {
        return attempt("disabled", true, "Notifications disabled by TUICR_NOTIFY=0");
    }
    if let Some(command) = configured_notification_command() {
        return run_command_hook_without_run(&command, payload);
    }
    if cfg!(target_os = "macos") {
        return run_osascript(payload);
    }
    attempt(
        "unsupported",
        false,
        "No notification sink configured; set TUICR_NOTIFY_COMMAND",
    )
}

fn notifications_disabled() -> bool {
    std::env::var(DISABLE_ENV)
        .map(|value| matches!(value.trim(), "0" | "false" | "False" | "FALSE"))
        .unwrap_or(false)
}

fn configured_notification_command() -> Option<String> {
    if let Ok(command) = std::env::var(COMMAND_ENV)
        && !command.trim().is_empty()
    {
        return Some(command);
    }
    crate::config::load_config()
        .ok()
        .and_then(|outcome| outcome.config)
        .and_then(|config| config.agent)
        .and_then(|agent| agent.notification_command)
        .map(|command| command.trim().to_string())
        .filter(|command| !command.is_empty())
}

fn run_command_hook_without_run(
    command: &str,
    payload: &NotificationPayload,
) -> NotificationAttempt {
    match Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("TUICR_NOTIFICATION_TITLE", &payload.title)
        .env("TUICR_NOTIFICATION_BODY", &payload.body)
        .env("TUICR_RUN_STATUS", "blocked")
        .status()
    {
        Ok(status) if status.success() => {
            attempt("command", true, "Notification command completed")
        }
        Ok(status) => attempt(
            "command",
            false,
            &format!("Notification command exited with status {status}"),
        ),
        Err(err) => attempt(
            "command",
            false,
            &format!("Failed to run notification command: {err}"),
        ),
    }
}

fn run_command_hook(
    command: &str,
    payload: &NotificationPayload,
    report: &DispatchReport,
) -> NotificationAttempt {
    match Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("TUICR_NOTIFICATION_TITLE", &payload.title)
        .env("TUICR_NOTIFICATION_BODY", &payload.body)
        .env("TUICR_RUN_ID", &report.run_id)
        .env("TUICR_REPOSITORY", &report.repository)
        .env("TUICR_PR", report.pr.to_string())
        .env("TUICR_RUN_STATUS", status_label(report.status))
        .status()
    {
        Ok(status) if status.success() => {
            attempt("command", true, "Notification command completed")
        }
        Ok(status) => attempt(
            "command",
            false,
            &format!("Notification command exited with status {status}"),
        ),
        Err(err) => attempt(
            "command",
            false,
            &format!("Failed to run notification command: {err}"),
        ),
    }
}

fn run_osascript(payload: &NotificationPayload) -> NotificationAttempt {
    let script = format!(
        "display notification {} with title {}",
        applescript_string(&payload.body),
        applescript_string(&payload.title),
    );
    match Command::new("osascript").arg("-e").arg(script).status() {
        Ok(status) if status.success() => attempt("osascript", true, "Desktop notification sent"),
        Ok(status) => attempt(
            "osascript",
            false,
            &format!("osascript exited with status {status}"),
        ),
        Err(err) => attempt(
            "osascript",
            false,
            &format!("Failed to run osascript: {err}"),
        ),
    }
}

fn attempt(sink: &str, success: bool, message: &str) -> NotificationAttempt {
    NotificationAttempt {
        delivered_at: Utc::now(),
        sink: sink.to_string(),
        success,
        message: message.to_string(),
    }
}

fn run_payload(report: &DispatchReport) -> NotificationPayload {
    let title = match report.status {
        DispatchStatus::Started => "tuicr agent started",
        DispatchStatus::Running => "tuicr agent running",
        DispatchStatus::Pushed => "tuicr agent pushed changes",
        DispatchStatus::Replied => "tuicr agent replied to feedback",
        DispatchStatus::WaitingForUser => "tuicr agent needs input",
        DispatchStatus::Succeeded => "tuicr agent completed",
        DispatchStatus::Failed => "tuicr agent failed",
        DispatchStatus::Cancelled => "tuicr agent cancelled",
        DispatchStatus::NoAction => "tuicr agent found no work",
        DispatchStatus::DryRun => "tuicr agent dry run",
    }
    .to_string();
    let mut body = format!(
        "{}#{} feedback={} failing_checks={}",
        report.repository, report.pr, report.feedback_count, report.failing_check_count
    );
    if let Some(exit_code) = report.exit_code {
        body.push_str(&format!(" exit={exit_code}"));
    }
    if let Some(session) = &report.tmux_session {
        body.push_str(&format!(" tmux={session}"));
    }
    NotificationPayload { title, body }
}

fn status_label(status: DispatchStatus) -> &'static str {
    match status {
        DispatchStatus::DryRun => "dry_run",
        DispatchStatus::NoAction => "no_action",
        DispatchStatus::Started => "started",
        DispatchStatus::Running => "running",
        DispatchStatus::Pushed => "pushed",
        DispatchStatus::Replied => "replied",
        DispatchStatus::WaitingForUser => "waiting_for_user",
        DispatchStatus::Succeeded => "succeeded",
        DispatchStatus::Failed => "failed",
        DispatchStatus::Cancelled => "cancelled",
    }
}

fn applescript_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn report(status: DispatchStatus) -> DispatchReport {
        DispatchReport {
            run_id: "run-1".to_string(),
            created_at: Some(Utc::now()),
            status_updated_at: Some(Utc::now()),
            completed_at: None,
            status,
            repository: "squareup/java".to_string(),
            pr: 480718,
            run_dir: PathBuf::from("/tmp/run-1"),
            prompt_path: PathBuf::from("/tmp/run-1/prompt.md"),
            run_record_path: PathBuf::from("/tmp/run-1/run.json"),
            log_path: Some(PathBuf::from("/tmp/run-1/run.log")),
            summary_path: Some(PathBuf::from("/tmp/run-1/summary.md")),
            workdir: PathBuf::from("/tmp/java"),
            worktree_source: Some("reused_existing_worktree".to_string()),
            worktree_branch: Some("feature".to_string()),
            tmux_session: Some("tuicr-run1".to_string()),
            feedback_count: 2,
            failing_check_count: 1,
            command: None,
            message: "started".to_string(),
            exit_code: None,
            notification_attempts: Vec::new(),
        }
    }

    #[test]
    fn should_build_run_started_payload() {
        let payload = run_payload(&report(DispatchStatus::Started));
        assert_eq!(payload.title, "tuicr agent started");
        assert!(payload.body.contains("squareup/java#480718"));
        assert!(payload.body.contains("feedback=2"));
        assert!(payload.body.contains("failing_checks=1"));
        assert!(payload.body.contains("tmux=tuicr-run1"));
    }

    #[test]
    fn should_build_failed_payload_with_exit_code() {
        let mut report = report(DispatchStatus::Failed);
        report.exit_code = Some(17);
        let payload = run_payload(&report);
        assert_eq!(payload.title, "tuicr agent failed");
        assert!(payload.body.contains("exit=17"));
    }

    #[test]
    fn should_build_cancelled_payload() {
        let payload = run_payload(&report(DispatchStatus::Cancelled));
        assert_eq!(payload.title, "tuicr agent cancelled");
    }

    #[test]
    fn should_escape_applescript_strings() {
        assert_eq!(
            applescript_string("quote \" and slash \\"),
            "\"quote \\\" and slash \\\\\""
        );
    }

    #[test]
    fn should_format_status_labels() {
        assert_eq!(status_label(DispatchStatus::Succeeded), "succeeded");
        assert_eq!(status_label(DispatchStatus::NoAction), "no_action");
        assert_eq!(status_label(DispatchStatus::Cancelled), "cancelled");
    }
}
