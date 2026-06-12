use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

use crate::agent::ci::{CheckItem, ChecksOptions, ChecksReport, collect_checks};
use crate::agent::feedback::{
    FeedbackItem, FeedbackOptions, FeedbackReport, OutdatedThreadMode, collect_feedback,
    resolve_repo_selector,
};
use crate::agent::notification::{NotificationAttempt, notify_run};
use crate::agent::state;
use crate::error::{Result, TuicrError};

const DEFAULT_WORKSPACE: &str = "/Users/tbedor/Development";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchOptions {
    pub repo: String,
    pub pr: u64,
    pub dry_run: bool,
    pub allow_non_owned: bool,
    pub agent_command: Option<String>,
    pub workspace_root: Option<PathBuf>,
    pub worktree_root: Option<PathBuf>,
    pub robot_logins: Vec<String>,
    pub ignored_comment_patterns: Vec<String>,
    pub outdated_thread_mode: OutdatedThreadMode,
    pub feedback_thread_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchReport {
    pub run_id: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub status_updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    pub status: DispatchStatus,
    pub repository: String,
    pub pr: u64,
    pub run_dir: PathBuf,
    pub prompt_path: PathBuf,
    pub run_record_path: PathBuf,
    #[serde(default)]
    pub log_path: Option<PathBuf>,
    #[serde(default)]
    pub summary_path: Option<PathBuf>,
    pub workdir: PathBuf,
    #[serde(default)]
    pub worktree_source: Option<String>,
    #[serde(default)]
    pub worktree_branch: Option<String>,
    pub tmux_session: Option<String>,
    pub feedback_count: usize,
    pub failing_check_count: usize,
    pub command: Option<String>,
    pub message: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub notification_attempts: Vec<NotificationAttempt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    DryRun,
    NoAction,
    Started,
    Running,
    Pushed,
    Replied,
    WaitingForUser,
    Succeeded,
    Failed,
    Cancelled,
}

impl DispatchStatus {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            DispatchStatus::Started
                | DispatchStatus::Running
                | DispatchStatus::Pushed
                | DispatchStatus::Replied
                | DispatchStatus::WaitingForUser
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            DispatchStatus::DryRun
                | DispatchStatus::NoAction
                | DispatchStatus::Succeeded
                | DispatchStatus::Failed
                | DispatchStatus::Cancelled
        )
    }
}

pub fn list_runs() -> Result<Vec<DispatchReport>> {
    let root = run_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("run.json");
        if !path.exists() {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path)
            && let Ok(report) = serde_json::from_str::<DispatchReport>(&content)
        {
            runs.push(report);
        }
    }
    runs.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    Ok(runs)
}

pub fn show_run(id_prefix: &str) -> Result<DispatchReport> {
    let matches = list_runs()?
        .into_iter()
        .filter(|run| run.run_id.starts_with(id_prefix))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [run] => Ok(run.clone()),
        [] => Err(TuicrError::InvalidInput(format!(
            "No tuicr agent run matches `{id_prefix}`"
        ))),
        _ => Err(TuicrError::InvalidInput(format!(
            "Multiple tuicr agent runs match `{id_prefix}`"
        ))),
    }
}

pub fn complete_run(id_prefix: &str, exit_code: i32) -> Result<DispatchReport> {
    let mut report = show_run(id_prefix)?;
    let now = Utc::now();
    report.status_updated_at = Some(now);
    report.completed_at = Some(now);
    report.exit_code = Some(exit_code);
    if exit_code == 0 {
        report.status = DispatchStatus::Succeeded;
        report.message = format!("Agent command completed successfully with exit code {exit_code}");
    } else {
        report.status = DispatchStatus::Failed;
        report.message = format!("Agent command failed with exit code {exit_code}");
    }
    let notification_attempt = notify_run(&report);
    report.notification_attempts.push(notification_attempt);
    write_run_summary(&report)?;
    write_run_record(&report)?;
    Ok(report)
}

pub fn update_run_status(
    id_prefix: &str,
    status: DispatchStatus,
    message: Option<String>,
) -> Result<DispatchReport> {
    let mut report = show_run(id_prefix)?;
    if status.is_terminal() {
        return Err(TuicrError::InvalidInput(
            "Use `prs runs complete` or `prs runs cancel` for terminal run statuses".to_string(),
        ));
    }
    report.status = status;
    report.status_updated_at = Some(Utc::now());
    if let Some(message) = message {
        report.message = message;
    } else {
        report.message = default_status_message(status).to_string();
    }
    let notification_attempt = notify_run(&report);
    report.notification_attempts.push(notification_attempt);
    write_run_summary(&report)?;
    write_run_record(&report)?;
    Ok(report)
}

pub fn cancel_run(id_prefix: &str) -> Result<DispatchReport> {
    let mut report = show_run(id_prefix)?;
    if let Some(session) = &report.tmux_session
        && tmux_session_exists(session)
    {
        kill_tmux_session(session)?;
    }
    let now = Utc::now();
    report.status_updated_at = Some(now);
    report.completed_at = Some(now);
    report.exit_code = None;
    report.status = DispatchStatus::Cancelled;
    report.message = "Agent run cancelled by user".to_string();
    let notification_attempt = notify_run(&report);
    report.notification_attempts.push(notification_attempt);
    write_run_record(&report)?;
    Ok(report)
}

pub fn attach_run(id_prefix: &str) -> Result<()> {
    let report = show_run(id_prefix)?;
    let session = attachable_tmux_session(&report)?;
    let err = Command::new("tmux")
        .arg("attach")
        .arg("-t")
        .arg(session)
        .exec();
    Err(TuicrError::Forge(format!(
        "Failed to attach tmux session `{session}`: {err}"
    )))
}

pub fn active_run_for_pr(repository: &str, pr: u64) -> Result<Option<DispatchReport>> {
    Ok(list_runs()?.into_iter().find(|run| {
        run.repository == repository
            && run.pr == pr
            && run.status.is_active()
            && run.tmux_session.as_deref().is_some_and(tmux_session_exists)
    }))
}

fn attachable_tmux_session(report: &DispatchReport) -> Result<&str> {
    let Some(session) = report.tmux_session.as_deref() else {
        return Err(TuicrError::InvalidInput(format!(
            "Run {} does not have a tmux session",
            report.run_id
        )));
    };
    if !tmux_session_exists(session) {
        return Err(TuicrError::InvalidInput(format!(
            "tmux session `{session}` for run {} is no longer available",
            report.run_id
        )));
    }
    Ok(session)
}

pub fn dispatch(options: DispatchOptions) -> Result<DispatchReport> {
    let resolved_repository = resolve_repo_selector(&options.repo)?;
    let repository_name = resolved_repository.display_name();
    let _lock = match DispatchLock::acquire(&repository_name, options.pr)? {
        Some(lock) => lock,
        None => {
            if let Some(active) = active_run_for_pr(&repository_name, options.pr)? {
                return Ok(active);
            }
            DispatchLock::break_stale(&repository_name, options.pr)?;
            DispatchLock::acquire(&repository_name, options.pr)?.ok_or_else(|| {
                TuicrError::Forge(format!(
                    "Another tuicr dispatch is already starting for {repository_name}#{}",
                    options.pr
                ))
            })?
        }
    };
    if let Some(active) = active_run_for_pr(&repository_name, options.pr)? {
        return Ok(active);
    }

    let feedback = collect_feedback(FeedbackOptions {
        repo: options.repo.clone(),
        pr: options.pr,
        viewer_login: None,
        robot_logins: options.robot_logins.clone(),
        ignored_comment_patterns: options.ignored_comment_patterns.clone(),
        outdated_thread_mode: options.outdated_thread_mode,
        require_owned_pr: !options.allow_non_owned,
    })?;
    let checks = collect_checks(ChecksOptions {
        repo: options.repo.clone(),
        pr: options.pr,
    })?;
    warn_state_error(
        "record pending feedback",
        state::record_pending_feedback(&feedback),
    );
    warn_state_error(
        "record check snapshot",
        state::record_check_snapshot(&checks),
    );

    let feedback_items =
        feedback_items_for_thread(&feedback.feedback, options.feedback_thread_id.as_deref());
    let failing_checks = if options.feedback_thread_id.is_some() {
        Vec::new()
    } else {
        checks.repair_candidates.clone()
    };
    let has_work = !feedback_items.is_empty() || !failing_checks.is_empty();
    let workdir = if has_work && !options.dry_run {
        prepare_agent_workdir(
            &options.repo,
            &feedback.repository,
            feedback.pr.number,
            &feedback.pr.head_ref_name,
            options.workspace_root.as_deref(),
            options.worktree_root.as_deref(),
        )?
    } else {
        resolve_base_workdir(
            &options.repo,
            &feedback.repository,
            options.workspace_root.as_deref(),
            feedback.pr.head_ref_name.as_str(),
            if options.dry_run {
                "dry_run_base_checkout"
            } else {
                "base_checkout"
            },
        )
    };
    let run_id = Uuid::new_v4().to_string();
    let run_dir = run_root()?.join(&run_id);
    fs::create_dir_all(&run_dir)?;
    let prompt = build_prompt(
        &run_id,
        &workdir.path,
        &feedback,
        &checks,
        &feedback_items,
        &failing_checks,
    )?;
    let prompt_path = run_dir.join("prompt.md");
    fs::write(&prompt_path, prompt)?;

    let run_record_path = run_dir.join("run.json");
    let log_path = run_dir.join("run.log");
    let summary_path = run_dir.join("summary.md");
    if feedback_items.is_empty() && failing_checks.is_empty() {
        let report = DispatchReport {
            run_id,
            created_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
            status: DispatchStatus::NoAction,
            repository: feedback.repository,
            pr: feedback.pr.number,
            run_dir,
            prompt_path,
            run_record_path,
            log_path: None,
            summary_path: Some(summary_path),
            workdir: workdir.path,
            worktree_source: Some(workdir.source),
            worktree_branch: workdir.branch,
            tmux_session: None,
            feedback_count: 0,
            failing_check_count: 0,
            command: None,
            message: no_action_message(options.feedback_thread_id.as_deref()),
            exit_code: None,
            status_updated_at: Some(Utc::now()),
            notification_attempts: Vec::new(),
        };
        warn_state_error(
            "record repo worktree",
            state::record_repo_worktree(
                &report.repository,
                report.pr,
                &report.workdir,
                report.worktree_branch.as_deref(),
                report.worktree_source.as_deref(),
            ),
        );
        write_run_record(&report)?;
        write_run_summary(&report)?;
        return Ok(report);
    }

    let agent_command = options.agent_command.unwrap_or_else(|| {
        "codex exec --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check -".to_string()
    });
    let tmux_session = format!("tuicr-{}", short_run_id(&run_id));
    let tuicr_exe = std::env::current_exe()
        .map_err(|err| TuicrError::Forge(format!("Could not resolve current executable: {err}")))?;
    let command = tmux_shell_command(&agent_command, &prompt_path, &log_path, &run_id, &tuicr_exe);

    let mut report = DispatchReport {
        run_id,
        created_at: Some(Utc::now()),
        status_updated_at: Some(Utc::now()),
        completed_at: None,
        status: if options.dry_run {
            DispatchStatus::DryRun
        } else {
            DispatchStatus::Started
        },
        repository: feedback.repository,
        pr: feedback.pr.number,
        run_dir,
        prompt_path,
        run_record_path,
        log_path: Some(log_path),
        summary_path: Some(summary_path),
        workdir: workdir.path,
        worktree_source: Some(workdir.source),
        worktree_branch: workdir.branch,
        tmux_session: if options.dry_run {
            None
        } else {
            Some(tmux_session.clone())
        },
        feedback_count: feedback_items.len(),
        failing_check_count: failing_checks.len(),
        command: Some(command.clone()),
        message: if options.dry_run {
            "Dry run wrote prompt and run record; no tmux session started".to_string()
        } else {
            format!("Started tmux session `{tmux_session}`")
        },
        exit_code: None,
        notification_attempts: Vec::new(),
    };
    warn_state_error(
        "record repo worktree",
        state::record_repo_worktree(
            &report.repository,
            report.pr,
            &report.workdir,
            report.worktree_branch.as_deref(),
            report.worktree_source.as_deref(),
        ),
    );

    write_run_record(&report)?;
    if !options.dry_run {
        start_tmux_session(&tmux_session, &report.workdir, &command)?;
        let notification_attempt = notify_run(&report);
        report.notification_attempts.push(notification_attempt);
        write_run_record(&report)?;
    } else {
        write_run_summary(&report)?;
    }
    Ok(report)
}

fn build_prompt(
    run_id: &str,
    workdir: &Path,
    feedback: &FeedbackReport,
    _checks: &ChecksReport,
    feedback_items: &[FeedbackItem],
    failing_checks: &[CheckItem],
) -> Result<String> {
    let feedback_json = serde_json::to_string_pretty(feedback_items)?;
    let checks_json = serde_json::to_string_pretty(failing_checks)?;
    Ok(format!(
        r#"You are a delegated Codex session handling a GitHub pull request.

PR: {url}
Repository: {repo}
Local checkout/workdir: {workdir}
PR number: {number}
Title: {title}
Head branch: {head_ref}
Head SHA at dispatch: {head_sha}
Base branch: {base_ref}

Address only the actionable feedback and failing checks listed below.

Required workflow:
1. Mark this run active with `tuicr prs runs status {run_id} --status running --message "Inspecting PR state"`.
2. Inspect the live PR, current branch, and local git state before editing.
3. Preserve unrelated local/user changes. Use the prepared local checkout/workdir above; if it is not usable, create or reuse an isolated git worktree before editing.
4. Re-check outdated comments for relevance against the current code before changing anything for them.
5. Implement the smallest code/doc/test changes needed for the listed feedback or CI failures.
6. Run focused validation appropriate to the touched files.
7. Commit changes if needed. Immediately before pushing, run `tuicr prs guard-head --repo {repo} --pr {number} --expected-head-sha {head_sha}`. If it fails, stop with `waiting-for-user` instead of pushing. If it passes, push to the PR branch and run `tuicr prs runs status {run_id} --status pushed --message "Pushed fixes to the PR branch"`.
8. Reply in GitHub to every handled feedback item after pushing. Prefer `tuicr prs reply --repo {repo} --pr {number} --expected-head-sha {head_sha} --feedback-id <id> --body <summary>` so replies are refused if the PR head moved, prefixed with "🤖", and routed to the correct thread/comment. After replies, run `tuicr prs runs status {run_id} --status replied --message "Replied to handled GitHub feedback"`.
9. If a human decision is needed, run `tuicr prs runs status {run_id} --status waiting-for-user --message "<blocker>"` and stop.
10. Do not mark an item handled unless you fixed it or explicitly explained why no code change was appropriate.
11. Use any check details, summaries, URLs, annotations, and log references included in the failing check JSON when diagnosing CI.
12. If CI failure logs are inaccessible or the same failure repeats, stop and explain the blocker.

Actionable feedback JSON:
{feedback_json}

Failing check JSON:
{checks_json}
"#,
        url = feedback.pr.url,
        repo = feedback.repository,
        number = feedback.pr.number,
        title = feedback.pr.title,
        workdir = workdir.display(),
        head_ref = feedback.pr.head_ref_name,
        head_sha = feedback.pr.head_sha,
        base_ref = feedback.pr.base_ref_name,
        run_id = run_id,
        feedback_json = feedback_json,
        checks_json = checks_json,
    ))
}

fn default_status_message(status: DispatchStatus) -> &'static str {
    match status {
        DispatchStatus::Started => "Agent run has started",
        DispatchStatus::Running => "Agent run is active",
        DispatchStatus::Pushed => "Agent run pushed changes",
        DispatchStatus::Replied => "Agent run replied to GitHub feedback",
        DispatchStatus::WaitingForUser => "Agent run needs user input",
        DispatchStatus::DryRun
        | DispatchStatus::NoAction
        | DispatchStatus::Succeeded
        | DispatchStatus::Failed
        | DispatchStatus::Cancelled => "Agent run is complete",
    }
}

fn feedback_items_for_thread(
    feedback_items: &[FeedbackItem],
    thread_id: Option<&str>,
) -> Vec<FeedbackItem> {
    let Some(thread_id) = thread_id else {
        return feedback_items.to_vec();
    };
    feedback_items
        .iter()
        .filter(|item| {
            item.thread_id.as_deref() == Some(thread_id) || item.id.as_str() == thread_id
        })
        .cloned()
        .collect()
}

fn no_action_message(feedback_thread_id: Option<&str>) -> String {
    if let Some(thread_id) = feedback_thread_id {
        format!("No actionable feedback found for selected thread {thread_id}")
    } else {
        "No actionable feedback or failing checks found".to_string()
    }
}

fn warn_state_error(action: &str, result: Result<()>) {
    if let Err(err) = result {
        eprintln!("Warning: Failed to {action}: {err}");
    }
}

fn start_tmux_session(session: &str, workdir: &Path, command: &str) -> Result<()> {
    let status = Command::new("tmux")
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(session)
        .arg("-c")
        .arg(workdir)
        .arg(command)
        .status()
        .map_err(|err| TuicrError::Forge(format!("Failed to start tmux: {err}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(TuicrError::Forge(format!(
            "tmux new-session failed with status {status}"
        )))
    }
}

fn tmux_session_exists(session: &str) -> bool {
    Command::new("tmux")
        .arg("has-session")
        .arg("-t")
        .arg(session)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn kill_tmux_session(session: &str) -> Result<()> {
    let status = Command::new("tmux")
        .arg("kill-session")
        .arg("-t")
        .arg(session)
        .status()
        .map_err(|err| TuicrError::Forge(format!("Failed to cancel tmux session: {err}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(TuicrError::Forge(format!(
            "tmux kill-session failed with status {status}"
        )))
    }
}

fn tmux_shell_command(
    agent_command: &str,
    prompt_path: &Path,
    log_path: &Path,
    run_id: &str,
    tuicr_exe: &Path,
) -> String {
    format!(
        "set -o pipefail; {} prs runs status {run_id} --status running --message 'Agent command is running' >/dev/null 2>&1; {agent_command} < {} 2>&1 | tee -a {}; status=$?; {} prs runs complete {run_id} --exit-code $status >/dev/null 2>&1; echo; echo 'tuicr run {run_id} finished with status '$status; exec $SHELL -l",
        shell_quote(&tuicr_exe.to_string_lossy()),
        shell_quote(&prompt_path.to_string_lossy()),
        shell_quote(&log_path.to_string_lossy()),
        shell_quote(&tuicr_exe.to_string_lossy()),
    )
}

fn write_run_record(report: &DispatchReport) -> Result<()> {
    fs::write(
        &report.run_record_path,
        format!("{}\n", serde_json::to_string_pretty(report)?),
    )?;
    Ok(())
}

fn write_run_summary(report: &DispatchReport) -> Result<()> {
    let Some(summary_path) = &report.summary_path else {
        return Ok(());
    };
    let log_tail = report
        .log_path
        .as_deref()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|content| tail_lines(&content, 80));
    let mut summary = String::new();
    summary.push_str(&format!("# tuicr agent run {}\n\n", report.run_id));
    summary.push_str(&format!("- repository: {}\n", report.repository));
    summary.push_str(&format!("- pull request: #{}\n", report.pr));
    summary.push_str(&format!("- status: {:?}\n", report.status));
    summary.push_str(&format!("- message: {}\n", report.message));
    summary.push_str(&format!(
        "- feedback items: {}\n- failing checks: {}\n",
        report.feedback_count, report.failing_check_count
    ));
    if let Some(exit_code) = report.exit_code {
        summary.push_str(&format!("- exit code: {exit_code}\n"));
    }
    summary.push_str(&format!("- workdir: {}\n", report.workdir.display()));
    if let Some(log_path) = &report.log_path {
        summary.push_str(&format!("- log: {}\n", log_path.display()));
    }
    if let Some(log_tail) = log_tail
        && !log_tail.trim().is_empty()
    {
        summary.push_str("\n## Log Tail\n\n```text\n");
        summary.push_str(&log_tail);
        if !log_tail.ends_with('\n') {
            summary.push('\n');
        }
        summary.push_str("```\n");
    }
    fs::write(summary_path, summary)?;
    Ok(())
}

fn tail_lines(content: &str, max_lines: usize) -> String {
    let mut lines = content.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n")
}

struct DispatchLock {
    path: PathBuf,
}

impl DispatchLock {
    fn acquire(repository: &str, pr: u64) -> Result<Option<Self>> {
        let path = lock_path(repository, pr)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                writeln!(file, "pid={}", std::process::id())?;
                writeln!(file, "created_at={}", Utc::now().to_rfc3339())?;
                Ok(Some(Self { path }))
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn break_stale(repository: &str, pr: u64) -> Result<()> {
        let path = lock_path(repository, pr)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

impl Drop for DispatchLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn run_root() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| {
        TuicrError::Forge("HOME must be set to write tuicr agent run state".to_string())
    })?;
    Ok(PathBuf::from(home).join(".local/state/tuicr/agent-runs"))
}

fn lock_path(repository: &str, pr: u64) -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| {
        TuicrError::Forge("HOME must be set to write tuicr agent lock state".to_string())
    })?;
    Ok(PathBuf::from(home)
        .join(".local/state/tuicr/agent-locks")
        .join(format!(
            "{}--{}.lock",
            sanitize_lock_component(repository),
            pr
        )))
}

fn sanitize_lock_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkdirSelection {
    path: PathBuf,
    source: String,
    branch: Option<String>,
}

fn prepare_agent_workdir(
    repo_selector: &str,
    repository: &str,
    pr: u64,
    branch: &str,
    workspace_root: Option<&Path>,
    worktree_root: Option<&Path>,
) -> Result<WorkdirSelection> {
    let base = resolve_base_workdir(
        repo_selector,
        repository,
        workspace_root,
        branch,
        "base_checkout",
    );
    if !is_git_checkout(&base.path) {
        return Ok(WorkdirSelection {
            source: "fallback_non_git_base".to_string(),
            ..base
        });
    }
    if let Some(existing) = find_worktree_for_branch(&base.path, branch)? {
        return Ok(WorkdirSelection {
            path: existing,
            source: "reused_existing_worktree".to_string(),
            branch: Some(branch.to_string()),
        });
    }

    let root = selected_worktree_root(&base.path, repository, worktree_root);
    fs::create_dir_all(&root)?;
    let target = root.join(worktree_dir_name(repository, pr));
    if target.exists() {
        if is_git_checkout(&target) && current_branch(&target).ok().as_deref() == Some(branch) {
            return Ok(WorkdirSelection {
                path: target,
                source: "reused_target_worktree".to_string(),
                branch: Some(branch.to_string()),
            });
        }
        return Err(TuicrError::Forge(format!(
            "Cannot create worktree at {}; path already exists but is not a git checkout on branch `{branch}`",
            target.display()
        )));
    }

    fetch_origin_branch(&base.path, branch)?;
    if local_branch_exists(&base.path, branch) {
        git_status(
            &base.path,
            &["worktree", "add", target.to_string_lossy().as_ref(), branch],
        )?;
        Ok(WorkdirSelection {
            path: target,
            source: "created_existing_branch_worktree".to_string(),
            branch: Some(branch.to_string()),
        })
    } else {
        let remote_ref = format!("origin/{branch}");
        git_status(
            &base.path,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                target.to_string_lossy().as_ref(),
                remote_ref.as_str(),
            ],
        )?;
        Ok(WorkdirSelection {
            path: target,
            source: "created_tracking_branch_worktree".to_string(),
            branch: Some(branch.to_string()),
        })
    }
}

fn resolve_base_workdir(
    repo_selector: &str,
    repository: &str,
    workspace_root: Option<&Path>,
    branch: &str,
    source: &str,
) -> WorkdirSelection {
    let selected = PathBuf::from(repo_selector);
    if selected.exists()
        && let Ok(canonical) = selected.canonicalize()
    {
        return WorkdirSelection {
            path: canonical,
            source: source.to_string(),
            branch: Some(branch.to_string()),
        };
    }
    let workspace = workspace_root.unwrap_or_else(|| Path::new(DEFAULT_WORKSPACE));
    let repo_name = repository.rsplit('/').next().unwrap_or(repository);
    let candidate = workspace.join(repo_name);
    if candidate.exists()
        && let Ok(canonical) = candidate.canonicalize()
    {
        return WorkdirSelection {
            path: canonical,
            source: source.to_string(),
            branch: Some(branch.to_string()),
        };
    }
    WorkdirSelection {
        path: workspace.to_path_buf(),
        source: source.to_string(),
        branch: Some(branch.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitWorktree {
    path: PathBuf,
    branch: Option<String>,
}

fn find_worktree_for_branch(base: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let output = git_output(base, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list(&output)
        .into_iter()
        .find(|worktree| worktree.branch.as_deref() == Some(branch))
        .map(|worktree| worktree.path))
}

fn parse_worktree_list(output: &str) -> Vec<GitWorktree> {
    let mut worktrees = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    for line in output.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(path) = path.take() {
                worktrees.push(GitWorktree {
                    path,
                    branch: branch.take(),
                });
            }
            continue;
        }
        if let Some(raw_path) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(raw_path));
        } else if let Some(raw_branch) = line.strip_prefix("branch refs/heads/") {
            branch = Some(raw_branch.to_string());
        }
    }
    worktrees
}

fn selected_worktree_root(
    base: &Path,
    repository: &str,
    configured_root: Option<&Path>,
) -> PathBuf {
    if let Some(root) = configured_root {
        return root.to_path_buf();
    }
    let repo_name = repository.rsplit('/').next().unwrap_or(repository);
    let Some(parent) = base.parent() else {
        return PathBuf::from(DEFAULT_WORKSPACE).join(".worktrees");
    };
    let dot_worktrees = parent.join(".worktrees");
    if dot_worktrees.exists() {
        return dot_worktrees;
    }
    parent.join(format!("{repo_name}-worktrees"))
}

fn worktree_dir_name(repository: &str, pr: u64) -> String {
    let repo_name = repository.rsplit('/').next().unwrap_or(repository);
    format!("{}-pr-{pr}", sanitize_path_component(repo_name))
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn is_git_checkout(path: &Path) -> bool {
    path.join(".git").exists()
        || Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("rev-parse")
            .arg("--show-toplevel")
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

fn fetch_origin_branch(base: &Path, branch: &str) -> Result<()> {
    let refspec = format!("refs/heads/{branch}:refs/remotes/origin/{branch}");
    git_status(base, &["fetch", "--quiet", "origin", refspec.as_str()])
}

fn local_branch_exists(base: &Path, branch: &str) -> bool {
    let reference = format!("refs/heads/{branch}");
    Command::new("git")
        .arg("-C")
        .arg(base)
        .arg("show-ref")
        .arg("--verify")
        .arg("--quiet")
        .arg(reference)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn current_branch(base: &Path) -> Result<String> {
    Ok(git_output(base, &["branch", "--show-current"])?
        .trim()
        .to_string())
}

fn git_status(base: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(base)
        .args(args)
        .output()
        .map_err(|err| TuicrError::Forge(format!("Failed to run git: {err}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(TuicrError::VcsCommand(format!(
            "git -C {} {} failed: {}",
            base.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn git_output(base: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(base)
        .args(args)
        .output()
        .map_err(|err| TuicrError::Forge(format!("Failed to run git: {err}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(TuicrError::VcsCommand(format!(
            "git -C {} {} failed: {}",
            base.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn short_run_id(run_id: &str) -> String {
    run_id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_without_tmux() -> DispatchReport {
        DispatchReport {
            run_id: "run-1".to_string(),
            created_at: Some(Utc::now()),
            status_updated_at: Some(Utc::now()),
            completed_at: None,
            status: DispatchStatus::DryRun,
            repository: "squareup/java".to_string(),
            pr: 480718,
            run_dir: PathBuf::from("/tmp/run-1"),
            prompt_path: PathBuf::from("/tmp/run-1/prompt.md"),
            run_record_path: PathBuf::from("/tmp/run-1/run.json"),
            log_path: Some(PathBuf::from("/tmp/run-1/run.log")),
            summary_path: Some(PathBuf::from("/tmp/run-1/summary.md")),
            workdir: PathBuf::from("/tmp/java"),
            worktree_source: Some("dry_run_base_checkout".to_string()),
            worktree_branch: Some("feature".to_string()),
            tmux_session: None,
            feedback_count: 1,
            failing_check_count: 0,
            command: None,
            message: "dry run".to_string(),
            exit_code: None,
            notification_attempts: Vec::new(),
        }
    }

    fn feedback_item(id: &str, thread_id: Option<&str>) -> FeedbackItem {
        FeedbackItem {
            kind: crate::agent::feedback::FeedbackKind::ReviewThread,
            id: id.to_string(),
            thread_id: thread_id.map(str::to_string),
            author: Some("reviewer".to_string()),
            url: format!("https://github.com/squareup/java/pull/1#discussion_r{id}"),
            path: Some("src/main.rs".to_string()),
            line: Some(42),
            original_line: Some(42),
            is_outdated: false,
            requires_relevance_check: false,
            body: "please update".to_string(),
            diff_hunk: None,
            comments: Vec::new(),
        }
    }

    #[test]
    fn should_quote_shell_paths() {
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shell_quote("/tmp/a'b"), "'/tmp/a'\\''b'");
    }

    #[test]
    fn should_track_active_dispatch_statuses() {
        assert!(DispatchStatus::Started.is_active());
        assert!(DispatchStatus::Running.is_active());
        assert!(DispatchStatus::Pushed.is_active());
        assert!(DispatchStatus::Replied.is_active());
        assert!(DispatchStatus::WaitingForUser.is_active());
        assert!(!DispatchStatus::DryRun.is_active());
        assert!(!DispatchStatus::NoAction.is_active());
        assert!(!DispatchStatus::Succeeded.is_active());
        assert!(!DispatchStatus::Failed.is_active());
        assert!(!DispatchStatus::Cancelled.is_active());
    }

    #[test]
    fn should_sanitize_lock_components() {
        assert_eq!(sanitize_lock_component("squareup/java"), "squareup_java");
        assert_eq!(
            sanitize_lock_component("github.example.com/owner/repo"),
            "github.example.com_owner_repo"
        );
    }

    #[test]
    fn should_parse_git_worktree_porcelain() {
        let worktrees = parse_worktree_list(
            "worktree /Users/tbedor/Development/java\nHEAD abc\nbranch refs/heads/main\n\nworktree /Users/tbedor/Development/java-worktrees/java-pr-480718\nHEAD def\nbranch refs/heads/feature/test\n\nworktree /tmp/detached\nHEAD 123\ndetached\n",
        );

        assert_eq!(worktrees.len(), 3);
        assert_eq!(
            worktrees[0].path,
            PathBuf::from("/Users/tbedor/Development/java")
        );
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(
            worktrees[1].path,
            PathBuf::from("/Users/tbedor/Development/java-worktrees/java-pr-480718")
        );
        assert_eq!(worktrees[1].branch.as_deref(), Some("feature/test"));
        assert_eq!(worktrees[2].branch, None);
    }

    #[test]
    fn should_build_worktree_paths_from_repository_and_pr() {
        assert_eq!(worktree_dir_name("squareup/java", 480718), "java-pr-480718");
        assert_eq!(
            worktree_dir_name("github.example.com/team/repo.with/slash", 9),
            "slash-pr-9"
        );
        assert_eq!(sanitize_path_component("repo/name"), "repo-name");
    }

    #[test]
    fn should_prefer_configured_worktree_root() {
        let root = selected_worktree_root(
            Path::new("/Users/tbedor/Development/java"),
            "squareup/java",
            Some(Path::new("/tmp/worktrees")),
        );
        assert_eq!(root, PathBuf::from("/tmp/worktrees"));
    }

    #[test]
    fn should_reject_attach_when_run_has_no_tmux_session() {
        let err = attachable_tmux_session(&report_without_tmux()).unwrap_err();
        assert!(err.to_string().contains("does not have a tmux session"));
    }

    #[test]
    fn should_filter_feedback_items_to_selected_thread() {
        let items = vec![
            feedback_item("comment-1", Some("thread-1")),
            feedback_item("comment-2", Some("thread-2")),
            feedback_item("thread-3", None),
        ];

        assert_eq!(feedback_items_for_thread(&items, None), items.clone());
        assert_eq!(
            feedback_items_for_thread(&items, Some("thread-1")),
            vec![feedback_item("comment-1", Some("thread-1"))]
        );
        assert_eq!(
            feedback_items_for_thread(&items, Some("thread-3")),
            vec![feedback_item("thread-3", None)]
        );
        assert!(feedback_items_for_thread(&items, Some("missing")).is_empty());
    }

    #[test]
    fn should_describe_no_action_for_selected_thread() {
        assert_eq!(
            no_action_message(Some("thread-1")),
            "No actionable feedback found for selected thread thread-1"
        );
        assert_eq!(
            no_action_message(None),
            "No actionable feedback or failing checks found"
        );
    }

    #[test]
    fn tmux_command_should_feed_prompt_to_agent_and_keep_shell_open() {
        let command = tmux_shell_command(
            "codex exec -",
            Path::new("/tmp/prompt.md"),
            Path::new("/tmp/run.log"),
            "abc123",
            Path::new("/tmp/tuicr"),
        );
        assert!(command.contains("codex exec - < '/tmp/prompt.md' 2>&1 | tee -a '/tmp/run.log'"));
        assert!(command.contains(
            "'/tmp/tuicr' prs runs status abc123 --status running --message 'Agent command is running'"
        ));
        assert!(command.contains("'/tmp/tuicr' prs runs complete abc123 --exit-code $status"));
        assert!(command.contains("tuicr run abc123 finished"));
        assert!(command.contains("exec $SHELL -l"));
    }

    #[test]
    fn should_tail_log_lines() {
        let log = "one\ntwo\nthree\nfour\n";
        assert_eq!(tail_lines(log, 2), "three\nfour");
    }
}
