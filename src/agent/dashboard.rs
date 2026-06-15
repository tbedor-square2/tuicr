use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::PathBuf;
use std::thread;

use crate::agent::ci::{CheckCounts, CheckState, ChecksOptions, collect_checks};
use crate::agent::dispatch::{DispatchReport, DispatchStatus, list_runs};
use crate::agent::feedback::{FeedbackOptions, OutdatedThreadMode, collect_feedback};
use crate::agent::pr_list::{PrListOptions, list_prs};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardOptions {
    pub owners: Vec<String>,
    pub repositories: Vec<String>,
    pub author: Option<String>,
    pub limit: usize,
    pub draft_filter: Option<bool>,
    pub review_filter: Option<String>,
    pub needs_action: bool,
    pub allow_non_owned: bool,
    pub agent_command: Option<String>,
    pub workspace_root: Option<PathBuf>,
    pub worktree_root: Option<PathBuf>,
    pub repository_include: Vec<String>,
    pub repository_exclude: Vec<String>,
    pub robot_logins: Vec<String>,
    pub ignored_comment_patterns: Vec<String>,
    pub outdated_thread_mode: OutdatedThreadMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardReport {
    pub author: String,
    pub owners: Vec<String>,
    pub generated_at: DateTime<Utc>,
    pub pull_requests: Vec<DashboardPr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardPr {
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub head_sha: String,
    pub updated_at: Option<DateTime<Utc>>,
    pub feedback_count: Option<usize>,
    pub check_state: Option<CheckState>,
    pub check_counts: Option<CheckCounts>,
    pub failing_check_count: Option<usize>,
    pub latest_run: Option<DashboardRun>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardRun {
    pub run_id: String,
    pub status: DispatchStatus,
    pub created_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub tmux_session: Option<String>,
    pub feedback_count: usize,
    pub failing_check_count: usize,
}

const ENRICH_CONCURRENCY: usize = 4;

pub fn dashboard(options: DashboardOptions) -> Result<DashboardReport> {
    let mut report = dashboard_overview(options.clone())?;
    enrich_dashboard_report(&mut report, &options);
    Ok(report)
}

pub(crate) fn dashboard_overview(options: DashboardOptions) -> Result<DashboardReport> {
    let pr_list = list_prs(PrListOptions {
        owners: options.owners,
        repositories: options.repositories,
        author: options.author,
        limit: options.limit,
        draft_filter: options.draft_filter,
        review_filter: options.review_filter,
        repository_include: options.repository_include,
        repository_exclude: options.repository_exclude,
    })?;
    let runs = list_runs()?;
    let pull_requests = pr_list
        .pull_requests
        .into_iter()
        .map(|pr| {
            let latest_run = latest_run_for_pr(&runs, &pr.repository, pr.number);
            DashboardPr {
                repository: pr.repository,
                number: pr.number,
                title: pr.title,
                url: pr.url,
                state: pr.state,
                is_draft: pr.is_draft,
                head_ref_name: String::new(),
                base_ref_name: String::new(),
                head_sha: String::new(),
                updated_at: pr.updated_at,
                feedback_count: None,
                check_state: None,
                check_counts: None,
                failing_check_count: None,
                latest_run,
                error: None,
            }
        })
        .collect();
    Ok(DashboardReport {
        author: pr_list.author,
        owners: pr_list.owners,
        generated_at: Utc::now(),
        pull_requests,
    })
}

pub(crate) fn enrich_dashboard_report(report: &mut DashboardReport, options: &DashboardOptions) {
    let jobs = report
        .pull_requests
        .iter()
        .enumerate()
        .map(|(index, pr)| {
            (
                index,
                pr.repository.clone(),
                pr.number,
                pr.title.clone(),
                pr.url.clone(),
                pr.state.clone(),
                pr.is_draft,
                pr.updated_at,
                pr.latest_run.clone(),
            )
        })
        .collect::<Vec<_>>();

    for chunk in jobs.chunks(ENRICH_CONCURRENCY) {
        let handles = chunk
            .iter()
            .cloned()
            .map(
                |(
                    index,
                    repository,
                    number,
                    title,
                    url,
                    state,
                    is_draft,
                    updated_at,
                    latest_run,
                )| {
                    let robot_logins = options.robot_logins.clone();
                    let ignored_comment_patterns = options.ignored_comment_patterns.clone();
                    let allow_non_owned = options.allow_non_owned;
                    let outdated_thread_mode = options.outdated_thread_mode;
                    thread::spawn(move || {
                        let result = enrich_pr(
                            &repository,
                            number,
                            allow_non_owned,
                            &robot_logins,
                            &ignored_comment_patterns,
                            outdated_thread_mode,
                        )
                        .map(|mut row| {
                            row.repository = repository.clone();
                            row.number = number;
                            row.title = title.clone();
                            row.url = url.clone();
                            row.state = state.clone();
                            row.is_draft = is_draft;
                            row.updated_at = updated_at;
                            row.latest_run = latest_run.clone();
                            row
                        })
                        .unwrap_or_else(|err| DashboardPr {
                            repository,
                            number,
                            title,
                            url,
                            state,
                            is_draft,
                            head_ref_name: String::new(),
                            base_ref_name: String::new(),
                            head_sha: String::new(),
                            updated_at,
                            feedback_count: None,
                            check_state: None,
                            check_counts: None,
                            failing_check_count: None,
                            latest_run,
                            error: Some(err.to_string()),
                        });
                        (index, result)
                    })
                },
            )
            .collect::<Vec<_>>();

        for handle in handles {
            if let Ok((index, row)) = handle.join()
                && let Some(target) = report.pull_requests.get_mut(index)
            {
                *target = row;
            }
        }
    }

    if options.needs_action {
        report.pull_requests.retain(|row| {
            row.feedback_count.unwrap_or(0) > 0 || row.failing_check_count.unwrap_or(0) > 0
        });
    }
}

fn enrich_pr(
    repository: &str,
    pr: u64,
    allow_non_owned: bool,
    robot_logins: &[String],
    ignored_comment_patterns: &[String],
    outdated_thread_mode: OutdatedThreadMode,
) -> Result<DashboardPr> {
    let feedback = collect_feedback(FeedbackOptions {
        repo: repository.to_string(),
        pr,
        viewer_login: None,
        robot_logins: robot_logins.to_vec(),
        ignored_comment_patterns: ignored_comment_patterns.to_vec(),
        outdated_thread_mode,
        require_owned_pr: !allow_non_owned,
    })?;
    let checks = collect_checks(ChecksOptions {
        repo: repository.to_string(),
        pr,
    })?;
    Ok(DashboardPr {
        repository: repository.to_string(),
        number: pr,
        title: feedback.pr.title,
        url: feedback.pr.url,
        state: feedback.pr.state,
        is_draft: feedback.pr.is_draft,
        head_ref_name: feedback.pr.head_ref_name,
        base_ref_name: feedback.pr.base_ref_name,
        head_sha: feedback.pr.head_sha,
        updated_at: None,
        feedback_count: Some(feedback.feedback.len()),
        check_state: Some(checks.overall_state),
        check_counts: Some(checks.counts),
        failing_check_count: Some(checks.repair_candidates.len()),
        latest_run: None,
        error: feedback.skipped_reason,
    })
}

fn latest_run_for_pr(runs: &[DispatchReport], repository: &str, pr: u64) -> Option<DashboardRun> {
    runs.iter()
        .filter(|run| run.repository == repository && run.pr == pr)
        .max_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.run_id.cmp(&right.run_id))
        })
        .map(|run| DashboardRun {
            run_id: run.run_id.clone(),
            status: run.status,
            created_at: run.created_at,
            completed_at: run.completed_at,
            tmux_session: run.tmux_session.clone(),
            feedback_count: run.feedback_count,
            failing_check_count: run.failing_check_count,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn run(run_id: &str, repository: &str, pr: u64, created_at: &str) -> DispatchReport {
        DispatchReport {
            run_id: run_id.to_string(),
            created_at: Some(DateTime::parse_from_rfc3339(created_at).unwrap().to_utc()),
            status_updated_at: Some(DateTime::parse_from_rfc3339(created_at).unwrap().to_utc()),
            completed_at: None,
            status: DispatchStatus::Started,
            repository: repository.to_string(),
            pr,
            run_dir: PathBuf::from("/tmp/run"),
            prompt_path: PathBuf::from("/tmp/run/prompt.md"),
            run_record_path: PathBuf::from("/tmp/run/run.json"),
            log_path: Some(PathBuf::from("/tmp/run/run.log")),
            summary_path: Some(PathBuf::from("/tmp/run/summary.md")),
            workdir: PathBuf::from("/tmp/repo"),
            worktree_source: Some("reused_existing_worktree".to_string()),
            worktree_branch: Some("feature".to_string()),
            tmux_session: Some(format!("tuicr-{run_id}")),
            feedback_count: 1,
            failing_check_count: 2,
            command: None,
            message: "started".to_string(),
            exit_code: None,
            notification_attempts: Vec::new(),
        }
    }

    #[test]
    fn should_choose_latest_run_for_pr() {
        let runs = vec![
            run("old", "squareup/java", 123, "2026-06-11T10:00:00Z"),
            run("new", "squareup/java", 123, "2026-06-11T11:00:00Z"),
            run("other", "squareup/java", 124, "2026-06-11T12:00:00Z"),
        ];
        let latest = latest_run_for_pr(&runs, "squareup/java", 123).unwrap();
        assert_eq!(latest.run_id, "new");
        assert_eq!(latest.feedback_count, 1);
        assert_eq!(latest.failing_check_count, 2);
    }
}
