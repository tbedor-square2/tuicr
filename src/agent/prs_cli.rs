use crate::agent::ci::{CheckItem, CheckState, ChecksOptions, collect_checks};
use crate::agent::dashboard::{DashboardOptions, DashboardPr, DashboardReport, dashboard};
use crate::agent::dashboard_tui;
use crate::agent::dispatch::{
    DispatchOptions, DispatchReport, DispatchStatus, attach_run, cancel_run, complete_run,
    dispatch, list_runs, show_run, update_run_status,
};
use crate::agent::feedback::{
    FeedbackItem, FeedbackKind, FeedbackOptions, OutdatedThreadMode, collect_feedback,
};
use crate::agent::github_actions::{
    ReplyOptions, ReplyReport, ReplyTarget, ResolveOptions, ResolveReport, read_body_arg, reply,
    resolve_thread,
};
use crate::agent::pr_list::{PrListOptions, PrListReport, list_prs};
use crate::agent::state;
use crate::agent::watch::{
    WatchIterationReport, WatchOptions, watch, watch_with_iteration_handler,
};
use crate::cli::{PrsCommand, ReviewFilterArg};
use crate::config::AgentConfig;
use crate::error::{Result, TuicrError};

const DEFAULT_WATCH_INTERVAL_SECONDS: u64 = 300;
const DEFAULT_MAX_CI_RETRIES: u32 = 2;

pub fn run(command: PrsCommand) -> Result<()> {
    let agent_config = load_agent_config();
    match command {
        PrsCommand::List {
            owners,
            repositories,
            author,
            draft,
            ready,
            review,
            limit,
            json,
        } => {
            let report = list_prs(PrListOptions {
                owners: configured_owners(owners, &agent_config),
                repositories,
                author: Some(author),
                limit,
                draft_filter: draft_filter(draft, ready),
                review_filter: review_filter(review),
                repository_include: configured_repository_include(&agent_config),
                repository_exclude: configured_repository_exclude(&agent_config),
            })?;
            warn_state_error("record watched PRs", state::record_watched_prs(&report));
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_pr_list_summary(&report);
            }
            Ok(())
        }
        PrsCommand::Checks { repo, pr, json } => {
            let report = collect_checks(ChecksOptions { repo, pr })?;
            warn_state_error(
                "record check snapshot",
                state::record_check_snapshot(&report),
            );
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_checks_summary(&report);
            }
            Ok(())
        }
        PrsCommand::Dashboard {
            owners,
            repositories,
            author,
            draft,
            ready,
            review,
            needs_action,
            limit,
            json,
            tui,
            allow_non_owned,
        } => {
            let options = DashboardOptions {
                owners: configured_owners(owners, &agent_config),
                repositories,
                author: Some(author),
                limit,
                draft_filter: draft_filter(draft, ready),
                review_filter: review_filter(review),
                needs_action,
                allow_non_owned,
                agent_command: agent_command_from_config(None, &agent_config),
                workspace_root: agent_config.workspace_root.clone(),
                worktree_root: agent_config.worktree_root.clone(),
                repository_include: configured_repository_include(&agent_config),
                repository_exclude: configured_repository_exclude(&agent_config),
                robot_logins: configured_robot_logins(Vec::new(), &agent_config),
                ignored_comment_patterns: configured_ignored_comment_patterns(&agent_config),
                outdated_thread_mode: configured_outdated_thread_mode(&agent_config),
            };
            if tui {
                dashboard_tui::run(options)?;
            } else if json {
                let report = dashboard(options)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let report = dashboard(options)?;
                print_dashboard_summary(&report);
            }
            Ok(())
        }
        PrsCommand::Dispatch {
            repo,
            pr,
            dry_run,
            json,
            allow_non_owned,
            agent_command,
        } => {
            let report = dispatch(DispatchOptions {
                repo,
                pr,
                dry_run,
                allow_non_owned,
                agent_command: agent_command_from_config(agent_command, &agent_config),
                workspace_root: agent_config.workspace_root.clone(),
                worktree_root: agent_config.worktree_root.clone(),
                robot_logins: configured_robot_logins(Vec::new(), &agent_config),
                ignored_comment_patterns: configured_ignored_comment_patterns(&agent_config),
                outdated_thread_mode: configured_outdated_thread_mode(&agent_config),
                feedback_thread_id: None,
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_dispatch_summary(&report);
            }
            Ok(())
        }
        PrsCommand::GuardHead {
            repo,
            pr,
            expected_head_sha,
            json,
            allow_non_owned,
        } => {
            let report = guard_head(GuardHeadOptions {
                repo,
                pr,
                expected_head_sha,
                allow_non_owned,
                robot_logins: configured_robot_logins(Vec::new(), &agent_config),
                ignored_comment_patterns: configured_ignored_comment_patterns(&agent_config),
                outdated_thread_mode: configured_outdated_thread_mode(&agent_config),
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_guard_head_summary(&report);
            }
            Ok(())
        }
        PrsCommand::Reply {
            repo,
            pr,
            feedback_id,
            thread_id,
            body,
            input,
            resolve,
            expected_head_sha,
            dry_run,
            json,
            allow_non_owned,
        } => {
            let body = read_body_arg(body, input)?;
            let report = reply(ReplyOptions {
                repo,
                pr,
                feedback_id,
                thread_id,
                body,
                resolve,
                expected_head_sha,
                dry_run,
                allow_non_owned,
                robot_logins: configured_robot_logins(Vec::new(), &agent_config),
                ignored_comment_patterns: configured_ignored_comment_patterns(&agent_config),
                outdated_thread_mode: configured_outdated_thread_mode(&agent_config),
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_reply_summary(&report);
            }
            Ok(())
        }
        PrsCommand::Resolve {
            repo,
            pr,
            thread_id,
            expected_head_sha,
            dry_run,
            json,
            allow_non_owned,
        } => {
            let report = resolve_thread(ResolveOptions {
                repo,
                pr,
                thread_id,
                expected_head_sha,
                dry_run,
                allow_non_owned,
                robot_logins: configured_robot_logins(Vec::new(), &agent_config),
                ignored_comment_patterns: configured_ignored_comment_patterns(&agent_config),
                outdated_thread_mode: configured_outdated_thread_mode(&agent_config),
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_resolve_summary(&report);
            }
            Ok(())
        }
        PrsCommand::Runs { command } => match command {
            crate::cli::PrsRunsCommand::List { json } => {
                let runs = list_runs()?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&runs)?);
                } else {
                    print_runs_summary(&runs);
                }
                Ok(())
            }
            crate::cli::PrsRunsCommand::Show { run_id, json } => {
                let run = show_run(&run_id)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&run)?);
                } else {
                    print_dispatch_summary(&run);
                }
                Ok(())
            }
            crate::cli::PrsRunsCommand::Complete {
                run_id,
                exit_code,
                json,
            } => {
                let run = complete_run(&run_id, exit_code)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&run)?);
                } else {
                    print_dispatch_summary(&run);
                }
                Ok(())
            }
            crate::cli::PrsRunsCommand::Status {
                run_id,
                status,
                message,
                json,
            } => {
                let run = update_run_status(&run_id, status.into(), message)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&run)?);
                } else {
                    print_dispatch_summary(&run);
                }
                Ok(())
            }
            crate::cli::PrsRunsCommand::Cancel { run_id, json } => {
                let run = cancel_run(&run_id)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&run)?);
                } else {
                    print_dispatch_summary(&run);
                }
                Ok(())
            }
            crate::cli::PrsRunsCommand::Attach { run_id } => attach_run(&run_id),
        },
        PrsCommand::Watch {
            owners,
            repositories,
            author,
            draft,
            ready,
            review,
            limit,
            dry_run,
            json,
            once,
            interval_seconds,
            max_iterations,
            max_ci_retries,
            allow_non_owned,
            agent_command,
        } => {
            let options = WatchOptions {
                owners: configured_owners(owners, &agent_config),
                repositories,
                author: Some(author),
                limit,
                draft_filter: draft_filter(draft, ready),
                review_filter: review_filter(review),
                dry_run,
                allow_non_owned,
                agent_command: agent_command_from_config(agent_command, &agent_config),
                workspace_root: agent_config.workspace_root.clone(),
                worktree_root: agent_config.worktree_root.clone(),
                repository_include: configured_repository_include(&agent_config),
                repository_exclude: configured_repository_exclude(&agent_config),
                global_concurrency: agent_config.global_concurrency,
                repository_concurrency: agent_config.repository_concurrency,
                robot_logins: configured_robot_logins(Vec::new(), &agent_config),
                ignored_comment_patterns: configured_ignored_comment_patterns(&agent_config),
                outdated_thread_mode: configured_outdated_thread_mode(&agent_config),
                once,
                interval_seconds: configured_interval_seconds(interval_seconds, &agent_config),
                max_iterations,
                max_ci_retries: configured_max_ci_retries(max_ci_retries, &agent_config),
            };
            if json {
                let report = watch(options)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let mut printed_any = false;
                let _report = watch_with_iteration_handler(options, |iteration| {
                    if printed_any {
                        println!();
                    }
                    print_watch_iteration(iteration);
                    printed_any = true;
                    Ok(())
                })?;
            }
            Ok(())
        }
        PrsCommand::Feedback {
            repo,
            pr,
            json,
            user,
            robot_logins,
            allow_non_owned,
        } => {
            let report = collect_feedback(FeedbackOptions {
                repo,
                pr,
                viewer_login: user,
                robot_logins: configured_robot_logins(robot_logins, &agent_config),
                ignored_comment_patterns: configured_ignored_comment_patterns(&agent_config),
                outdated_thread_mode: configured_outdated_thread_mode(&agent_config),
                require_owned_pr: !allow_non_owned,
            })?;
            warn_state_error(
                "record pending feedback",
                state::record_pending_feedback(&report),
            );
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_feedback_summary(&report);
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GuardHeadOptions {
    repo: String,
    pr: u64,
    expected_head_sha: String,
    allow_non_owned: bool,
    robot_logins: Vec<String>,
    ignored_comment_patterns: Vec<String>,
    outdated_thread_mode: OutdatedThreadMode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct GuardHeadReport {
    repository: String,
    pr: u64,
    expected_head_sha: String,
    current_head_sha: String,
    matches: bool,
    message: String,
}

fn guard_head(options: GuardHeadOptions) -> Result<GuardHeadReport> {
    let expected = options.expected_head_sha.trim();
    if expected.is_empty() {
        return Err(TuicrError::InvalidInput(
            "--expected-head-sha cannot be empty".to_string(),
        ));
    }
    let feedback = collect_feedback(FeedbackOptions {
        repo: options.repo,
        pr: options.pr,
        viewer_login: None,
        robot_logins: options.robot_logins,
        ignored_comment_patterns: options.ignored_comment_patterns,
        outdated_thread_mode: options.outdated_thread_mode,
        require_owned_pr: !options.allow_non_owned,
    })?;
    if let Some(reason) = feedback.skipped_reason {
        return Err(TuicrError::InvalidInput(reason));
    }
    let matches = feedback.pr.head_sha == expected;
    let report = GuardHeadReport {
        repository: feedback.repository,
        pr: feedback.pr.number,
        expected_head_sha: expected.to_string(),
        current_head_sha: feedback.pr.head_sha,
        matches,
        message: if matches {
            "PR head matches expected SHA".to_string()
        } else {
            "PR head changed; re-read the PR before pushing or replying".to_string()
        },
    };
    if report.matches {
        Ok(report)
    } else {
        Err(TuicrError::InvalidInput(format!(
            "{}: expected {}, current {}",
            report.message, report.expected_head_sha, report.current_head_sha
        )))
    }
}

pub(crate) fn load_agent_config() -> AgentConfig {
    match crate::config::load_config() {
        Ok(outcome) => {
            for warning in outcome.warnings {
                eprintln!("{warning}");
            }
            outcome
                .config
                .and_then(|config| config.agent)
                .unwrap_or_default()
        }
        Err(err) => {
            eprintln!("Warning: Failed to load config for PR orchestration: {err}");
            AgentConfig::default()
        }
    }
}

fn warn_state_error(action: &str, result: Result<()>) {
    if let Err(err) = result {
        eprintln!("Warning: Failed to {action}: {err}");
    }
}

fn configured_owners(cli_owners: Vec<String>, config: &AgentConfig) -> Vec<String> {
    if cli_owners.is_empty() {
        config.github_owners.clone().unwrap_or_default()
    } else {
        cli_owners
    }
}

pub(crate) fn configured_robot_logins(
    cli_logins: Vec<String>,
    config: &AgentConfig,
) -> Vec<String> {
    if cli_logins.is_empty() {
        config.robot_logins.clone().unwrap_or_default()
    } else {
        cli_logins
    }
}

pub(crate) fn configured_ignored_comment_patterns(config: &AgentConfig) -> Vec<String> {
    config.ignored_comment_patterns.clone().unwrap_or_default()
}

pub(crate) fn configured_outdated_thread_mode(config: &AgentConfig) -> OutdatedThreadMode {
    match config.outdated_thread_relevance.as_deref() {
        Some("include") => OutdatedThreadMode::Include,
        Some("ignore") => OutdatedThreadMode::Ignore,
        _ => OutdatedThreadMode::Recheck,
    }
}

fn draft_filter(draft: bool, ready: bool) -> Option<bool> {
    if draft {
        Some(true)
    } else if ready {
        Some(false)
    } else {
        None
    }
}

fn review_filter(review: Option<ReviewFilterArg>) -> Option<String> {
    review.map(|review| review.as_gh_value().to_string())
}

pub(crate) fn agent_command_from_config(
    cli_command: Option<String>,
    config: &AgentConfig,
) -> Option<String> {
    cli_command.or_else(|| config.agent_command.clone())
}

fn configured_repository_include(config: &AgentConfig) -> Vec<String> {
    config.repository_include.clone().unwrap_or_default()
}

fn configured_repository_exclude(config: &AgentConfig) -> Vec<String> {
    config.repository_exclude.clone().unwrap_or_default()
}

fn configured_interval_seconds(cli_interval: u64, config: &AgentConfig) -> u64 {
    if cli_interval == DEFAULT_WATCH_INTERVAL_SECONDS {
        config
            .ci_poll_interval_seconds
            .map(|value| value as u64)
            .unwrap_or(cli_interval)
    } else {
        cli_interval
    }
}

fn configured_max_ci_retries(cli_retries: u32, config: &AgentConfig) -> u32 {
    if cli_retries == DEFAULT_MAX_CI_RETRIES {
        config
            .max_ci_retries
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(cli_retries)
    } else {
        cli_retries
    }
}

fn print_reply_summary(report: &ReplyReport) {
    println!("{}#{} reply", report.repository, report.pr);
    match &report.target {
        ReplyTarget::ReviewThread {
            thread_id,
            feedback_id,
        } => {
            println!("target: review thread {thread_id}");
            if let Some(feedback_id) = feedback_id {
                println!("feedback: {feedback_id}");
            }
        }
        ReplyTarget::PullRequestComment {
            feedback_id,
            source_url,
        } => {
            println!("target: pull request comment");
            if let Some(feedback_id) = feedback_id {
                println!("feedback: {feedback_id}");
            }
            if let Some(source_url) = source_url {
                println!("source: {source_url}");
            }
        }
    }
    println!("dry-run: {}", report.dry_run);
    if let Some(reply) = &report.reply {
        if let Some(url) = &reply.url {
            println!("reply: {url}");
        } else if let Some(id) = &reply.id {
            println!("reply: {id}");
        }
    }
    if let Some(resolved) = &report.resolved {
        println!("resolved: {}", resolved.is_resolved.unwrap_or(false));
    }
    println!("{}", report.message);
}

fn print_guard_head_summary(report: &GuardHeadReport) {
    println!("{}#{} guard-head", report.repository, report.pr);
    println!("expected: {}", report.expected_head_sha);
    println!("current: {}", report.current_head_sha);
    println!("matches: {}", report.matches);
    println!("{}", report.message);
}

fn print_dashboard_summary(report: &DashboardReport) {
    println!(
        "dashboard {}: {} PRs by {} in {}",
        report.generated_at.to_rfc3339(),
        report.pull_requests.len(),
        report.author,
        report.owners.join(", ")
    );
    for pr in &report.pull_requests {
        print_dashboard_pr(pr);
    }
}

fn print_dashboard_pr(pr: &DashboardPr) {
    let draft = if pr.is_draft { " draft" } else { "" };
    let feedback = pr
        .feedback_count
        .map(|count| count.to_string())
        .unwrap_or_else(|| "?".to_string());
    let check_state = pr.check_state.map(state_label).unwrap_or("unknown");
    let failing = pr
        .failing_check_count
        .map(|count| count.to_string())
        .unwrap_or_else(|| "?".to_string());
    let run = pr
        .latest_run
        .as_ref()
        .map(|run| {
            format!(
                " run={}({})",
                run.run_id.chars().take(8).collect::<String>(),
                dispatch_status_label(run.status)
            )
        })
        .unwrap_or_default();
    println!(
        "- {}#{}{} state={} {} -> {} head={} feedback={} checks={} failing={}{} {}",
        pr.repository,
        pr.number,
        draft,
        pr.state,
        empty_label(&pr.head_ref_name),
        empty_label(&pr.base_ref_name),
        short_sha(&pr.head_sha),
        feedback,
        check_state,
        failing,
        run,
        pr.title
    );
    if let Some(error) = &pr.error {
        println!("  error: {error}");
    }
    println!("  {}", pr.url);
}

fn print_resolve_summary(report: &ResolveReport) {
    println!(
        "{}#{} resolve {}",
        report.repository, report.pr, report.thread_id
    );
    println!("dry-run: {}", report.dry_run);
    if let Some(is_resolved) = report.is_resolved {
        println!("resolved: {is_resolved}");
    }
    println!("{}", report.message);
}

fn print_watch_iteration(iteration: &WatchIterationReport) {
    println!(
        "watch {}: {} PRs checked, {} dispatched",
        iteration.checked_at.to_rfc3339(),
        iteration.pull_requests.len(),
        iteration.dispatch_count
    );
    for pr in &iteration.pull_requests {
        let feedback = pr
            .feedback_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "?".to_string());
        let failing = pr
            .failing_check_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "?".to_string());
        let state = pr.overall_check_state.map(state_label).unwrap_or("unknown");
        println!(
            "- {}#{} feedback={} failing_checks={} overall={}",
            pr.repository, pr.pr, feedback, failing, state
        );
        if let Some(ci_retry) = &pr.ci_retry
            && !ci_retry.attempts.is_empty()
        {
            let attempts = ci_retry
                .attempts
                .iter()
                .map(|attempt| {
                    format!(
                        "{}={}/{}",
                        attempt.check_name, attempt.attempts, attempt.max_attempts
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            println!("  ci retries: {attempts}");
        }
        if let Some(dispatch) = &pr.dispatch {
            let tmux = dispatch
                .tmux_session
                .as_ref()
                .map(|session| format!(" tmux={session}"))
                .unwrap_or_default();
            let reused = if dispatch.reused_existing {
                " existing"
            } else {
                ""
            };
            println!(
                "  dispatched{} {} [{}]{}",
                reused, dispatch.run_id, dispatch.status, tmux
            );
        }
        if let Some(reason) = &pr.skipped_reason {
            println!("  skipped: {reason}");
        }
        if let Some(error) = &pr.error {
            println!("  error: {error}");
        }
    }
}

fn print_pr_list_summary(report: &PrListReport) {
    println!(
        "open PRs by {} in {}: {}",
        report.author,
        report.owners.join(", "),
        report.pull_requests.len()
    );
    for pr in &report.pull_requests {
        let draft = if pr.is_draft { " draft" } else { "" };
        let updated = pr
            .updated_at
            .map(|timestamp| format!(" updated={}", timestamp.to_rfc3339()))
            .unwrap_or_default();
        println!(
            "- {}#{}{} state={}{} {}",
            pr.repository, pr.number, draft, pr.state, updated, pr.title
        );
        println!("  {}", pr.url);
    }
}

fn print_runs_summary(runs: &[DispatchReport]) {
    if runs.is_empty() {
        println!("agent runs: 0");
        return;
    }
    println!("agent runs: {}", runs.len());
    for run in runs {
        println!(
            "- {} [{}] {}#{} feedback={} failing_checks={}{}{}",
            run.run_id,
            dispatch_status_label(run.status),
            run.repository,
            run.pr,
            run.feedback_count,
            run.failing_check_count,
            run.tmux_session
                .as_ref()
                .map(|session| format!(" tmux={session}"))
                .unwrap_or_default(),
            run.exit_code
                .map(|exit_code| format!(" exit={exit_code}"))
                .unwrap_or_default()
        );
    }
}

fn print_dispatch_summary(report: &crate::agent::dispatch::DispatchReport) {
    println!("{}#{} run {}", report.repository, report.pr, report.run_id);
    println!("status: {}", dispatch_status_label(report.status));
    println!("run dir: {}", report.run_dir.display());
    println!("prompt: {}", report.prompt_path.display());
    if let Some(log_path) = &report.log_path {
        println!("log: {}", log_path.display());
    }
    if let Some(summary_path) = &report.summary_path {
        println!("summary: {}", summary_path.display());
    }
    println!("workdir: {}", report.workdir.display());
    if let Some(source) = &report.worktree_source {
        println!("worktree source: {source}");
    }
    if let Some(branch) = &report.worktree_branch {
        println!("worktree branch: {branch}");
    }
    println!(
        "feedback: {}  failing checks: {}",
        report.feedback_count, report.failing_check_count
    );
    if let Some(completed_at) = report.completed_at {
        println!("completed: {}", completed_at.to_rfc3339());
    }
    if let Some(exit_code) = report.exit_code {
        println!("exit: {exit_code}");
    }
    if let Some(notification) = report.notification_attempts.last() {
        println!(
            "notification: {} success={} {}",
            notification.sink, notification.success, notification.message
        );
    }
    if let Some(session) = &report.tmux_session {
        println!("tmux: {session}");
        println!("attach: tmux attach -t {session}");
    }
    println!("{}", report.message);
}

pub(crate) fn dispatch_status_label(status: DispatchStatus) -> &'static str {
    match status {
        DispatchStatus::DryRun => "dry-run",
        DispatchStatus::NoAction => "no-action",
        DispatchStatus::Started => "started",
        DispatchStatus::Running => "running",
        DispatchStatus::Pushed => "pushed",
        DispatchStatus::Replied => "replied",
        DispatchStatus::WaitingForUser => "waiting-for-user",
        DispatchStatus::Succeeded => "succeeded",
        DispatchStatus::Failed => "failed",
        DispatchStatus::Cancelled => "cancelled",
    }
}

impl From<crate::cli::RunStatusArg> for DispatchStatus {
    fn from(status: crate::cli::RunStatusArg) -> Self {
        match status {
            crate::cli::RunStatusArg::Started => DispatchStatus::Started,
            crate::cli::RunStatusArg::Running => DispatchStatus::Running,
            crate::cli::RunStatusArg::Pushed => DispatchStatus::Pushed,
            crate::cli::RunStatusArg::Replied => DispatchStatus::Replied,
            crate::cli::RunStatusArg::WaitingForUser => DispatchStatus::WaitingForUser,
        }
    }
}

fn print_checks_summary(report: &crate::agent::ci::ChecksReport) {
    println!(
        "{}#{} {}",
        report.repository, report.pr.number, report.pr.title
    );
    println!("url: {}", report.pr.url);
    println!(
        "overall: {}  passing: {}  pending: {}  failing: {}  cancelled: {}  skipped: {}  unknown: {}",
        state_label(report.overall_state),
        report.counts.passing,
        report.counts.pending,
        report.counts.failing,
        report.counts.cancelled,
        report.counts.skipped,
        report.counts.unknown
    );
    if report.checks.is_empty() {
        println!("checks: 0");
        return;
    }
    for check in &report.checks {
        print_check_item(check);
    }
}

fn print_check_item(check: &CheckItem) {
    let repair = if check.needs_repair { " repair" } else { "" };
    println!("- [{}{}] {}", state_label(check.state), repair, check.name);
    if let Some(url) = &check.url
        && !url.is_empty()
    {
        println!("  {url}");
    }
    if let Some(details) = &check.details {
        if let Some(summary) = &details.summary {
            let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
            if !summary.is_empty() {
                println!("  summary: {}", clip_text(&summary, 180));
            }
        }
        if !details.annotations.is_empty() {
            println!("  annotations: {}", details.annotations.len());
            for annotation in details.annotations.iter().take(3) {
                let location = annotation
                    .path
                    .as_ref()
                    .map(|path| match annotation.start_line {
                        Some(line) => format!("{path}:{line}"),
                        None => path.clone(),
                    })
                    .unwrap_or_else(|| "<unknown>".to_string());
                println!("    - {} {}", location, clip_text(&annotation.message, 160));
            }
        }
        if let Some(log) = &details.log_excerpt {
            let truncated = if log.truncated { " truncated" } else { "" };
            println!(
                "  log excerpt: {} lines={}{}",
                log.adapter, log.line_count, truncated
            );
            for line in log.text.lines().take(8) {
                println!("    {}", clip_text(line, 180));
            }
        }
        if let Some(error) = &details.log_fetch_error {
            println!("  log fetch error: {}", clip_text(error, 220));
        }
    }
    if !check.log_references.is_empty() {
        for reference in &check.log_references {
            if reference.url.is_empty() {
                println!("  logs: {} ({})", reference.adapter, reference.hint);
            } else {
                println!(
                    "  logs: {} {} ({})",
                    reference.adapter, reference.url, reference.hint
                );
            }
        }
    }
}

fn clip_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() > max_chars {
        let prefix = value
            .chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>();
        format!("{}...", prefix.trim_end())
    } else {
        value.to_string()
    }
}

fn empty_label(value: &str) -> &str {
    if value.is_empty() { "?" } else { value }
}

fn short_sha(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}

pub(crate) fn state_label(state: CheckState) -> &'static str {
    match state {
        CheckState::Pending => "pending",
        CheckState::Passing => "passing",
        CheckState::Failing => "failing",
        CheckState::Cancelled => "cancelled",
        CheckState::Skipped => "skipped",
        CheckState::Unknown => "unknown",
    }
}

fn print_feedback_summary(report: &crate::agent::feedback::FeedbackReport) {
    println!(
        "{}#{} {}",
        report.repository, report.pr.number, report.pr.title
    );
    println!("url: {}", report.pr.url);
    println!(
        "viewer: {}  author: {}  owned: {}",
        report.viewer_login,
        report.pr.author.as_deref().unwrap_or("<unknown>"),
        report.owned_by_viewer
    );
    if let Some(reason) = &report.skipped_reason {
        println!("skipped: {reason}");
        return;
    }
    if report.feedback.is_empty() {
        println!("actionable feedback: 0");
        return;
    }
    println!("actionable feedback: {}", report.feedback.len());
    for (idx, item) in report.feedback.iter().enumerate() {
        print_feedback_item(idx + 1, item);
    }
}

fn print_feedback_item(index: usize, item: &FeedbackItem) {
    let kind = match item.kind {
        FeedbackKind::ReviewThread => "review-thread",
        FeedbackKind::IssueComment => "issue-comment",
    };
    let location = item
        .path
        .as_ref()
        .map(|path| match item.line.or(item.original_line) {
            Some(line) => format!("{path}:{line}"),
            None => path.clone(),
        })
        .unwrap_or_else(|| "PR".to_string());
    let relevance = if item.requires_relevance_check {
        " relevance-check"
    } else {
        ""
    };
    println!(
        "{}. [{}{}] {} by {}",
        index,
        kind,
        relevance,
        location,
        item.author.as_deref().unwrap_or("<unknown>")
    );
    println!("   {}", item.url);
    let excerpt = item.body.split_whitespace().collect::<Vec<_>>().join(" ");
    if !excerpt.is_empty() {
        let clipped = if excerpt.chars().count() > 160 {
            let prefix = excerpt.chars().take(157).collect::<String>();
            format!("{}...", prefix.trim_end())
        } else {
            excerpt
        };
        println!("   {clipped}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn agent_config() -> AgentConfig {
        AgentConfig {
            workspace_root: Some(PathBuf::from("/workspace")),
            github_owners: Some(vec!["squareup".to_string(), "block".to_string()]),
            robot_logins: Some(vec!["review-bot".to_string()]),
            agent_command: Some("codex exec -".to_string()),
            ci_poll_interval_seconds: Some(45),
            max_ci_retries: Some(5),
            ..AgentConfig::default()
        }
    }

    #[test]
    fn should_use_agent_config_defaults_when_cli_values_are_empty_or_default() {
        let config = agent_config();

        assert_eq!(
            configured_owners(Vec::new(), &config),
            vec!["squareup".to_string(), "block".to_string()]
        );
        assert_eq!(
            configured_robot_logins(Vec::new(), &config),
            vec!["review-bot".to_string()]
        );
        assert_eq!(
            agent_command_from_config(None, &config),
            Some("codex exec -".to_string())
        );
        assert_eq!(configured_interval_seconds(300, &config), 45);
        assert_eq!(configured_max_ci_retries(2, &config), 5);
        assert_eq!(config.global_concurrency, None);
        assert_eq!(config.repository_concurrency, None);
    }

    #[test]
    fn should_prefer_cli_values_over_agent_config_defaults() {
        let config = agent_config();

        assert_eq!(
            configured_owners(vec!["cashapp".to_string()], &config),
            vec!["cashapp".to_string()]
        );
        assert_eq!(
            configured_robot_logins(vec!["cli-bot".to_string()], &config),
            vec!["cli-bot".to_string()]
        );
        assert_eq!(
            agent_command_from_config(Some("custom agent".to_string()), &config),
            Some("custom agent".to_string())
        );
        assert_eq!(configured_interval_seconds(10, &config), 10);
        assert_eq!(configured_max_ci_retries(9, &config), 9);
    }
}
