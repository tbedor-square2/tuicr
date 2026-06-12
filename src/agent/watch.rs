use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::agent::ci::{CheckState, ChecksOptions, collect_checks};
use crate::agent::ci_retries::{
    CiRetryDecision, decide_ci_retries, needs_ci_retry_exhausted_notification,
    record_ci_retry_attempts, record_ci_retry_exhausted_notification,
};
use crate::agent::dispatch::list_runs;
use crate::agent::dispatch::{DispatchOptions, DispatchReport, active_run_for_pr, dispatch};
use crate::agent::feedback::{FeedbackOptions, OutdatedThreadMode, collect_feedback};
use crate::agent::notification::notify_custom;
use crate::agent::pr_list::{PrListOptions, list_prs};
use crate::agent::state;
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOptions {
    pub owners: Vec<String>,
    pub repositories: Vec<String>,
    pub author: Option<String>,
    pub limit: usize,
    pub draft_filter: Option<bool>,
    pub review_filter: Option<String>,
    pub dry_run: bool,
    pub allow_non_owned: bool,
    pub agent_command: Option<String>,
    pub workspace_root: Option<PathBuf>,
    pub worktree_root: Option<PathBuf>,
    pub repository_include: Vec<String>,
    pub repository_exclude: Vec<String>,
    pub global_concurrency: Option<usize>,
    pub repository_concurrency: Option<usize>,
    pub robot_logins: Vec<String>,
    pub ignored_comment_patterns: Vec<String>,
    pub outdated_thread_mode: OutdatedThreadMode,
    pub once: bool,
    pub interval_seconds: u64,
    pub max_iterations: Option<usize>,
    pub max_ci_retries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchReport {
    pub iterations: Vec<WatchIterationReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchIterationReport {
    pub checked_at: DateTime<Utc>,
    pub author: String,
    pub owners: Vec<String>,
    pub pull_requests: Vec<WatchPrReport>,
    pub dispatch_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchPrReport {
    pub repository: String,
    pub pr: u64,
    pub title: String,
    pub url: String,
    pub feedback_count: Option<usize>,
    pub failing_check_count: Option<usize>,
    pub overall_check_state: Option<CheckState>,
    pub ci_retry: Option<CiRetryDecision>,
    pub dispatch: Option<WatchDispatchReport>,
    pub skipped_reason: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchDispatchReport {
    pub run_id: String,
    pub status: String,
    pub tmux_session: Option<String>,
    pub reused_existing: bool,
    pub message: String,
}

pub fn watch(options: WatchOptions) -> Result<WatchReport> {
    watch_with_iteration_handler(options, |_| Ok(()))
}

pub fn watch_with_iteration_handler<F>(
    options: WatchOptions,
    mut on_iteration: F,
) -> Result<WatchReport>
where
    F: FnMut(&WatchIterationReport) -> Result<()>,
{
    let mut iterations = Vec::new();
    loop {
        let iteration = watch_once(&options)?;
        on_iteration(&iteration)?;
        iterations.push(iteration);
        if options.once || reached_max_iterations(iterations.len(), options.max_iterations) {
            break;
        }
        thread::sleep(Duration::from_secs(options.interval_seconds.max(1)));
    }
    Ok(WatchReport { iterations })
}

fn reached_max_iterations(iteration_count: usize, max_iterations: Option<usize>) -> bool {
    max_iterations.is_some_and(|max| iteration_count >= max.max(1))
}

fn watch_once(options: &WatchOptions) -> Result<WatchIterationReport> {
    let pr_list = list_prs(PrListOptions {
        owners: options.owners.clone(),
        repositories: options.repositories.clone(),
        author: options.author.clone(),
        limit: options.limit,
        draft_filter: options.draft_filter,
        review_filter: options.review_filter.clone(),
        repository_include: options.repository_include.clone(),
        repository_exclude: options.repository_exclude.clone(),
    })?;
    warn_state_error("record watched PRs", state::record_watched_prs(&pr_list));
    let active_counts = active_run_counts()?;
    let mut started_global = 0usize;
    let mut started_by_repo: HashMap<String, usize> = HashMap::new();
    let mut pull_requests = Vec::new();
    for pr in pr_list.pull_requests {
        let concurrency_skip = dispatch_concurrency_skip(
            options,
            &pr.repository,
            &active_counts,
            started_global,
            &started_by_repo,
        );
        let report = match inspect_and_maybe_dispatch(
            options,
            &pr.repository,
            pr.number,
            concurrency_skip,
        ) {
            Ok(mut report) => {
                report.title = pr.title.clone();
                report.url = pr.url.clone();
                if report
                    .dispatch
                    .as_ref()
                    .is_some_and(|dispatch| !dispatch.reused_existing)
                {
                    started_global += 1;
                    *started_by_repo.entry(pr.repository.clone()).or_default() += 1;
                }
                report
            }
            Err(err) => WatchPrReport {
                repository: pr.repository.clone(),
                pr: pr.number,
                title: pr.title.clone(),
                url: pr.url.clone(),
                feedback_count: None,
                failing_check_count: None,
                overall_check_state: None,
                ci_retry: None,
                dispatch: None,
                skipped_reason: None,
                error: Some(err.to_string()),
            },
        };
        pull_requests.push(report);
    }
    let dispatch_count = pull_requests
        .iter()
        .filter(|report| {
            report
                .dispatch
                .as_ref()
                .is_some_and(|dispatch| !dispatch.reused_existing)
        })
        .count();
    Ok(WatchIterationReport {
        checked_at: Utc::now(),
        author: pr_list.author,
        owners: pr_list.owners,
        pull_requests,
        dispatch_count,
    })
}

#[derive(Debug, Default)]
struct ActiveRunCounts {
    global: usize,
    by_repo: HashMap<String, usize>,
}

fn active_run_counts() -> Result<ActiveRunCounts> {
    let mut counts = ActiveRunCounts::default();
    for run in list_runs()? {
        if run.status.is_active() {
            counts.global += 1;
            *counts.by_repo.entry(run.repository).or_default() += 1;
        }
    }
    Ok(counts)
}

fn dispatch_concurrency_skip(
    options: &WatchOptions,
    repository: &str,
    active: &ActiveRunCounts,
    started_global: usize,
    started_by_repo: &HashMap<String, usize>,
) -> Option<String> {
    if let Some(limit) = options.global_concurrency {
        let current = active.global + started_global;
        if current >= limit {
            return Some(format!(
                "global agent concurrency limit reached ({current}/{limit})"
            ));
        }
    }
    if let Some(limit) = options.repository_concurrency {
        let current = active.by_repo.get(repository).copied().unwrap_or(0)
            + started_by_repo.get(repository).copied().unwrap_or(0);
        if current >= limit {
            return Some(format!(
                "repository agent concurrency limit reached for {repository} ({current}/{limit})"
            ));
        }
    }
    None
}

fn inspect_and_maybe_dispatch(
    options: &WatchOptions,
    repository: &str,
    pr: u64,
    concurrency_skip_reason: Option<String>,
) -> Result<WatchPrReport> {
    let feedback = collect_feedback(FeedbackOptions {
        repo: repository.to_string(),
        pr,
        viewer_login: None,
        robot_logins: options.robot_logins.clone(),
        ignored_comment_patterns: options.ignored_comment_patterns.clone(),
        outdated_thread_mode: options.outdated_thread_mode,
        require_owned_pr: !options.allow_non_owned,
    })?;
    let checks = collect_checks(ChecksOptions {
        repo: repository.to_string(),
        pr,
    })?;
    let feedback_count = feedback.feedback.len();
    let failing_check_count = checks.repair_candidates.len();
    let ci_retry = decide_ci_retries(
        &feedback.repository,
        pr,
        &checks.pr.head_sha,
        &checks.repair_candidates,
        options.max_ci_retries,
    )?;
    let active_run = active_run_for_pr(&feedback.repository, pr)?;
    let active_skip_reason = active_run
        .as_ref()
        .map(|run| format!("active agent run already exists: {}", run.run_id));
    let ci_exhausted_reason = ci_retry.exhausted.then(|| {
        let exhausted_checks = ci_retry
            .exhausted_checks
            .iter()
            .map(|attempt| {
                format!(
                    "{} ({}/{})",
                    attempt.check_name, attempt.attempts, attempt.max_attempts
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "CI repair retry limit reached for head {}: {exhausted_checks}",
            checks.pr.head_sha
        )
    });
    if ci_retry.exhausted
        && !options.dry_run
        && needs_ci_retry_exhausted_notification(&ci_retry.exhausted_checks)?
    {
        let body = ci_exhausted_reason
            .as_deref()
            .unwrap_or("CI repair retry limit reached");
        let notification = notify_custom("tuicr CI repair blocked", body);
        record_ci_retry_exhausted_notification(&ci_retry.exhausted_checks, notification)?;
    }
    let dispatch_report = if let Some(active_run) = active_run {
        Some(WatchDispatchReport::reused(active_run))
    } else if concurrency_skip_reason.is_some() && (feedback_count > 0 || failing_check_count > 0) {
        None
    } else if feedback.skipped_reason.is_none()
        && !ci_retry.exhausted
        && (feedback_count > 0 || failing_check_count > 0)
    {
        if failing_check_count > 0 && !options.dry_run {
            let _attempts = record_ci_retry_attempts(
                &feedback.repository,
                pr,
                &checks.pr.head_sha,
                &checks.repair_candidates,
            )?;
        }
        Some(
            dispatch(DispatchOptions {
                repo: repository.to_string(),
                pr,
                dry_run: options.dry_run,
                allow_non_owned: options.allow_non_owned,
                agent_command: options.agent_command.clone(),
                workspace_root: options.workspace_root.clone(),
                worktree_root: options.worktree_root.clone(),
                robot_logins: options.robot_logins.clone(),
                ignored_comment_patterns: options.ignored_comment_patterns.clone(),
                outdated_thread_mode: options.outdated_thread_mode,
                feedback_thread_id: None,
            })?
            .into(),
        )
    } else {
        None
    };

    Ok(WatchPrReport {
        repository: repository.to_string(),
        pr,
        title: feedback.pr.title,
        url: feedback.pr.url,
        feedback_count: Some(feedback_count),
        failing_check_count: Some(failing_check_count),
        overall_check_state: Some(checks.overall_state),
        ci_retry: Some(ci_retry),
        dispatch: dispatch_report,
        skipped_reason: feedback
            .skipped_reason
            .or(active_skip_reason)
            .or(concurrency_skip_reason)
            .or(ci_exhausted_reason),
        error: None,
    })
}

impl From<DispatchReport> for WatchDispatchReport {
    fn from(report: DispatchReport) -> Self {
        Self {
            run_id: report.run_id,
            status: format!("{:?}", report.status).to_ascii_snake_case(),
            tmux_session: report.tmux_session,
            reused_existing: false,
            message: report.message,
        }
    }
}

impl WatchDispatchReport {
    fn reused(report: DispatchReport) -> Self {
        Self {
            run_id: report.run_id.clone(),
            status: format!("{:?}", report.status).to_ascii_snake_case(),
            tmux_session: report.tmux_session.clone(),
            reused_existing: true,
            message: format!("Active agent run already exists: {}", report.run_id),
        }
    }
}

trait ToAsciiSnakeCase {
    fn to_ascii_snake_case(&self) -> String;
}

fn warn_state_error(action: &str, result: Result<()>) {
    if let Err(err) = result {
        eprintln!("Warning: Failed to {action}: {err}");
    }
}

impl ToAsciiSnakeCase for str {
    fn to_ascii_snake_case(&self) -> String {
        let mut output = String::new();
        for (idx, ch) in self.chars().enumerate() {
            if ch.is_ascii_uppercase() {
                if idx > 0 {
                    output.push('_');
                }
                output.push(ch.to_ascii_lowercase());
            } else {
                output.push(ch);
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_honor_once_even_with_max_iterations() {
        assert!(reached_max_iterations(1, Some(1)));
        assert!(!reached_max_iterations(1, Some(2)));
    }

    #[test]
    fn should_format_dispatch_status_as_snake_case() {
        assert_eq!("DryRun".to_ascii_snake_case(), "dry_run");
        assert_eq!("NoAction".to_ascii_snake_case(), "no_action");
    }

    #[test]
    fn should_skip_when_global_concurrency_limit_is_reached() {
        let options = WatchOptions {
            owners: Vec::new(),
            repositories: Vec::new(),
            author: None,
            limit: 10,
            draft_filter: None,
            review_filter: None,
            dry_run: true,
            allow_non_owned: false,
            agent_command: None,
            workspace_root: None,
            worktree_root: None,
            repository_include: Vec::new(),
            repository_exclude: Vec::new(),
            global_concurrency: Some(2),
            repository_concurrency: None,
            robot_logins: Vec::new(),
            ignored_comment_patterns: Vec::new(),
            outdated_thread_mode: OutdatedThreadMode::Recheck,
            once: true,
            interval_seconds: 300,
            max_iterations: None,
            max_ci_retries: 2,
        };
        let active = ActiveRunCounts {
            global: 1,
            by_repo: HashMap::new(),
        };

        let skip =
            dispatch_concurrency_skip(&options, "squareup/java", &active, 1, &HashMap::new())
                .unwrap();

        assert!(skip.contains("global agent concurrency limit reached"));
    }

    #[test]
    fn should_skip_when_repository_concurrency_limit_is_reached() {
        let options = WatchOptions {
            owners: Vec::new(),
            repositories: Vec::new(),
            author: None,
            limit: 10,
            draft_filter: None,
            review_filter: None,
            dry_run: true,
            allow_non_owned: false,
            agent_command: None,
            workspace_root: None,
            worktree_root: None,
            repository_include: Vec::new(),
            repository_exclude: Vec::new(),
            global_concurrency: None,
            repository_concurrency: Some(1),
            robot_logins: Vec::new(),
            ignored_comment_patterns: Vec::new(),
            outdated_thread_mode: OutdatedThreadMode::Recheck,
            once: true,
            interval_seconds: 300,
            max_iterations: None,
            max_ci_retries: 2,
        };
        let active = ActiveRunCounts {
            global: 1,
            by_repo: HashMap::from([("squareup/java".to_string(), 1)]),
        };

        let skip =
            dispatch_concurrency_skip(&options, "squareup/java", &active, 0, &HashMap::new())
                .unwrap();

        assert!(skip.contains("repository agent concurrency limit reached"));
    }
}
