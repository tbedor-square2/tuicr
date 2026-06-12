use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::agent::ci_adapters::{
    CheckLogReference, detect_log_references, github_actions_job_reference,
};
use crate::agent::feedback::{map_gh_error, resolve_repo_selector};
use crate::error::Result;
use crate::forge::github::gh::{GhCommandRunner, SystemGhRunner};
use crate::forge::traits::ForgeRepository;

const DEFAULT_GITHUB_HOST: &str = "github.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksOptions {
    pub repo: String,
    pub pr: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChecksReport {
    pub repository: String,
    pub pr: CheckPrSummary,
    pub overall_state: CheckState,
    pub counts: CheckCounts,
    pub checks: Vec<CheckItem>,
    pub repair_candidates: Vec<CheckItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckPrSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    pub state: String,
    pub is_draft: bool,
    pub head_sha: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pending,
    Passing,
    Failing,
    Cancelled,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CheckCounts {
    pub pending: usize,
    pub passing: usize,
    pub failing: usize,
    pub cancelled: usize,
    pub skipped: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckItem {
    pub name: String,
    pub source_type: String,
    pub state: CheckState,
    pub raw_status: Option<String>,
    pub raw_conclusion: Option<String>,
    pub url: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub workflow_name: Option<String>,
    pub needs_repair: bool,
    pub details: Option<CheckDetails>,
    pub log_references: Vec<CheckLogReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckDetails {
    pub check_run_id: u64,
    pub html_url: Option<String>,
    pub details_url: Option<String>,
    pub summary: Option<String>,
    pub text: Option<String>,
    pub annotations_count: usize,
    pub annotations: Vec<CheckAnnotation>,
    pub log_excerpt: Option<CheckLogExcerpt>,
    pub log_fetch_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckLogExcerpt {
    pub adapter: String,
    pub run_id: Option<u64>,
    pub job_id: Option<u64>,
    pub truncated: bool,
    pub line_count: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckAnnotation {
    pub path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub annotation_level: Option<String>,
    pub title: Option<String>,
    pub message: String,
    pub raw_details: Option<String>,
}

pub fn collect_checks(options: ChecksOptions) -> Result<ChecksReport> {
    collect_checks_with_runner(options, &SystemGhRunner)
}

fn collect_checks_with_runner<R: GhCommandRunner>(
    options: ChecksOptions,
    runner: &R,
) -> Result<ChecksReport> {
    let repository = resolve_repo_selector(&options.repo)?;
    let raw = fetch_pr_checks(runner, &repository, options.pr)?;
    let mut checks = raw
        .status_check_rollup
        .into_iter()
        .map(CheckItem::from)
        .collect::<Vec<_>>();
    enrich_failing_check_runs(runner, &repository, &raw.head_ref_oid, &mut checks)?;
    let counts = count_checks(&checks);
    let overall_state = overall_state(counts);
    let repair_candidates = checks
        .iter()
        .filter(|check| check.needs_repair)
        .cloned()
        .collect::<Vec<_>>();

    Ok(ChecksReport {
        repository: repository.display_name(),
        pr: CheckPrSummary {
            number: options.pr,
            title: raw.title,
            url: raw.url,
            author: raw.author.map(|author| author.login),
            state: raw.state,
            is_draft: raw.is_draft,
            head_sha: raw.head_ref_oid,
        },
        overall_state,
        counts,
        checks,
        repair_candidates,
    })
}

fn fetch_pr_checks<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    pr_number: u64,
) -> Result<RawPrChecks> {
    let mut args = vec![
        "pr".to_string(),
        "view".to_string(),
        pr_number.to_string(),
        "--repo".to_string(),
        format!("{}/{}", repository.owner, repository.name),
        "--json".to_string(),
        "statusCheckRollup,headRefOid,url,title,author,state,isDraft".to_string(),
    ];
    if repository.host != DEFAULT_GITHUB_HOST {
        args.push("--hostname".to_string());
        args.push(repository.host.clone());
    }
    let output = runner
        .run(&args)
        .map_err(|err| map_gh_error(err, &repository.host))?;
    Ok(serde_json::from_str(&output)?)
}

fn enrich_failing_check_runs<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    head_sha: &str,
    checks: &mut [CheckItem],
) -> Result<()> {
    if !checks.iter().any(|check| {
        check.needs_repair && check.source_type == "CheckRun" && check.details.is_none()
    }) {
        return Ok(());
    }
    let raw_details = fetch_check_runs_for_head(runner, repository, head_sha)?;
    for check in checks
        .iter_mut()
        .filter(|check| check.needs_repair && check.source_type == "CheckRun")
    {
        let Some(raw_detail) = raw_details
            .iter()
            .find(|detail| detail.name.as_deref() == Some(check.name.as_str()))
        else {
            continue;
        };
        let annotations = fetch_check_run_annotations(runner, repository, raw_detail.id)?;
        let (log_excerpt, log_fetch_error) = fetch_check_log_excerpt(
            runner,
            repository,
            raw_detail,
            &check.log_references,
            head_sha,
            &check.name,
        );
        check.details = Some(raw_detail.to_details(annotations, log_excerpt, log_fetch_error));
    }
    Ok(())
}

fn fetch_check_runs_for_head<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    head_sha: &str,
) -> Result<Vec<RawCheckRunDetail>> {
    let mut all = Vec::new();
    for page in 1..=10 {
        let endpoint = format!(
            "repos/{}/{}/commits/{}/check-runs?per_page=100&page={}",
            repository.owner, repository.name, head_sha, page
        );
        let mut args = vec![
            "api".to_string(),
            "-H".to_string(),
            "Accept: application/vnd.github+json".to_string(),
        ];
        if repository.host != DEFAULT_GITHUB_HOST {
            args.push("--hostname".to_string());
            args.push(repository.host.clone());
        }
        args.push(endpoint);
        let output = runner
            .run(&args)
            .map_err(|err| map_gh_error(err, &repository.host))?;
        let response: RawCheckRunsResponse = serde_json::from_str(&output)?;
        let received = response.check_runs.len();
        all.extend(response.check_runs);
        if received < 100 {
            break;
        }
    }
    Ok(all)
}

fn fetch_check_run_annotations<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    check_run_id: u64,
) -> Result<Vec<CheckAnnotation>> {
    let mut all = Vec::new();
    for page in 1..=5 {
        let endpoint = format!(
            "repos/{}/{}/check-runs/{}/annotations?per_page=100&page={}",
            repository.owner, repository.name, check_run_id, page
        );
        let mut args = vec![
            "api".to_string(),
            "-H".to_string(),
            "Accept: application/vnd.github+json".to_string(),
        ];
        if repository.host != DEFAULT_GITHUB_HOST {
            args.push("--hostname".to_string());
            args.push(repository.host.clone());
        }
        args.push(endpoint);
        let output = runner
            .run(&args)
            .map_err(|err| map_gh_error(err, &repository.host))?;
        let annotations = serde_json::from_str::<Vec<RawCheckAnnotation>>(&output)?;
        let received = annotations.len();
        all.extend(annotations.into_iter().map(CheckAnnotation::from));
        if received < 100 {
            break;
        }
    }
    Ok(all)
}

fn fetch_check_log_excerpt<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    raw_detail: &RawCheckRunDetail,
    log_references: &[CheckLogReference],
    head_sha: &str,
    check_name: &str,
) -> (Option<CheckLogExcerpt>, Option<String>) {
    let Some(details_url) = raw_detail.details_url.as_deref() else {
        return (
            None,
            Some("No check details URL was available for log lookup".to_string()),
        );
    };
    let Some(reference) = github_actions_job_reference(details_url) else {
        return fetch_external_ci_log_excerpt(
            repository,
            raw_detail,
            log_references,
            head_sha,
            check_name,
        );
    };
    match fetch_github_actions_failed_log(runner, repository, reference.job_id) {
        Ok(log) => log_excerpt(
            "github_actions",
            Some(reference.run_id),
            Some(reference.job_id),
            log,
        ),
        Err(err) => (None, Some(err.to_string())),
    }
}

fn fetch_external_ci_log_excerpt(
    repository: &ForgeRepository,
    raw_detail: &RawCheckRunDetail,
    log_references: &[CheckLogReference],
    head_sha: &str,
    check_name: &str,
) -> (Option<CheckLogExcerpt>, Option<String>) {
    let Some(reference) = log_references
        .iter()
        .find(|reference| matches!(reference.adapter.as_str(), "buildkite" | "kochiku"))
    else {
        return (None, None);
    };
    let env_key = match reference.adapter.as_str() {
        "buildkite" => "TUICR_BUILDKITE_LOG_COMMAND",
        "kochiku" => "TUICR_KOCHIKU_LOG_COMMAND",
        _ => return (None, None),
    };
    let Ok(command) = std::env::var(env_key) else {
        return (
            None,
            Some(format!(
                "No {} log command configured; set {env_key} to fetch raw logs",
                reference.adapter
            )),
        );
    };
    if command.trim().is_empty() {
        return (
            None,
            Some(format!(
                "No {} log command configured; set {env_key} to fetch raw logs",
                reference.adapter
            )),
        );
    }
    match run_external_ci_log_command(
        command.trim(),
        ExternalCiLogContext {
            adapter: &reference.adapter,
            repository: &repository.display_name(),
            head_sha,
            check_name,
            check_url: reference.url.as_str(),
            check_run_id: raw_detail.id,
            html_url: raw_detail.html_url.as_deref().unwrap_or_default(),
            details_url: raw_detail.details_url.as_deref().unwrap_or_default(),
        },
    ) {
        Ok(log) => log_excerpt(&reference.adapter, None, None, log),
        Err(err) => (None, Some(err)),
    }
}

struct ExternalCiLogContext<'a> {
    adapter: &'a str,
    repository: &'a str,
    head_sha: &'a str,
    check_name: &'a str,
    check_url: &'a str,
    check_run_id: u64,
    html_url: &'a str,
    details_url: &'a str,
}

fn run_external_ci_log_command(
    command: &str,
    context: ExternalCiLogContext<'_>,
) -> std::result::Result<String, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("TUICR_CI_ADAPTER", context.adapter)
        .env("TUICR_REPOSITORY", context.repository)
        .env("TUICR_HEAD_SHA", context.head_sha)
        .env("TUICR_CHECK_NAME", context.check_name)
        .env("TUICR_CHECK_URL", context.check_url)
        .env("TUICR_CHECK_RUN_ID", context.check_run_id.to_string())
        .env("TUICR_CHECK_HTML_URL", context.html_url)
        .env("TUICR_CHECK_DETAILS_URL", context.details_url)
        .output()
        .map_err(|err| format!("Failed to run {} log command: {err}", context.adapter))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "{} log command exited with status {}: {}",
            context.adapter,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn log_excerpt(
    adapter: &str,
    run_id: Option<u64>,
    job_id: Option<u64>,
    log: String,
) -> (Option<CheckLogExcerpt>, Option<String>) {
    let (text, truncated, line_count) = clip_log_excerpt(&log, 240, 30_000);
    (
        Some(CheckLogExcerpt {
            adapter: adapter.to_string(),
            run_id,
            job_id,
            truncated,
            line_count,
            text,
        }),
        None,
    )
}

fn fetch_github_actions_failed_log<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    job_id: u64,
) -> Result<String> {
    let mut args = vec![
        "run".to_string(),
        "view".to_string(),
        "--repo".to_string(),
        repository.display_name(),
        "--job".to_string(),
        job_id.to_string(),
        "--log-failed".to_string(),
    ];
    if repository.host != DEFAULT_GITHUB_HOST {
        args.push("--hostname".to_string());
        args.push(repository.host.clone());
    }
    runner
        .run(&args)
        .map_err(|err| map_gh_error(err, &repository.host))
}

fn clip_log_excerpt(log: &str, max_lines: usize, max_chars: usize) -> (String, bool, usize) {
    let total_lines = log.lines().count();
    let mut clipped = log.lines().rev().take(max_lines).collect::<Vec<_>>();
    clipped.reverse();
    let mut text = clipped.join("\n");
    let line_truncated = total_lines > max_lines;
    let char_truncated = text.chars().count() > max_chars;
    if char_truncated {
        let keep = max_chars.saturating_sub(3);
        text = format!("{}...", text.chars().take(keep).collect::<String>());
    }
    (text, line_truncated || char_truncated, total_lines)
}

fn count_checks(checks: &[CheckItem]) -> CheckCounts {
    let mut counts = CheckCounts::default();
    for check in checks {
        match check.state {
            CheckState::Pending => counts.pending += 1,
            CheckState::Passing => counts.passing += 1,
            CheckState::Failing => counts.failing += 1,
            CheckState::Cancelled => counts.cancelled += 1,
            CheckState::Skipped => counts.skipped += 1,
            CheckState::Unknown => counts.unknown += 1,
        }
    }
    counts
}

fn overall_state(counts: CheckCounts) -> CheckState {
    if counts.failing > 0 {
        CheckState::Failing
    } else if counts.pending > 0 {
        CheckState::Pending
    } else if counts.cancelled > 0 {
        CheckState::Cancelled
    } else if counts.unknown > 0 {
        CheckState::Unknown
    } else if counts.passing > 0 {
        CheckState::Passing
    } else if counts.skipped > 0 {
        CheckState::Skipped
    } else {
        CheckState::Unknown
    }
}

impl From<RawCheck> for CheckItem {
    fn from(raw: RawCheck) -> Self {
        let state = normalize_state(&raw);
        let name = raw.name();
        let url = raw.details_url.or(raw.target_url);
        let log_references =
            detect_log_references(&name, url.as_deref(), raw.workflow_name.as_deref());
        Self {
            name,
            source_type: raw.kind,
            state,
            raw_status: raw.status,
            raw_conclusion: raw.conclusion,
            url,
            started_at: raw.started_at,
            completed_at: raw.completed_at,
            workflow_name: raw.workflow_name.filter(|name| !name.trim().is_empty()),
            needs_repair: state == CheckState::Failing,
            details: None,
            log_references,
        }
    }
}

fn normalize_state(raw: &RawCheck) -> CheckState {
    let status = raw.status.as_deref().unwrap_or("").to_ascii_uppercase();
    let conclusion = raw.conclusion.as_deref().unwrap_or("").to_ascii_uppercase();
    let state = raw.state.as_deref().unwrap_or("").to_ascii_uppercase();

    match raw.kind.as_str() {
        "CheckRun" => match (status.as_str(), conclusion.as_str()) {
            (_, "SUCCESS") | (_, "NEUTRAL") => CheckState::Passing,
            (_, "SKIPPED") => CheckState::Skipped,
            (_, "CANCELLED") => CheckState::Cancelled,
            (_, "FAILURE") | (_, "TIMED_OUT") | (_, "ACTION_REQUIRED") => CheckState::Failing,
            ("QUEUED", _) | ("IN_PROGRESS", _) | ("WAITING", _) | ("REQUESTED", _) => {
                CheckState::Pending
            }
            ("COMPLETED", "") => CheckState::Unknown,
            _ => CheckState::Unknown,
        },
        "StatusContext" => match state.as_str() {
            "SUCCESS" => CheckState::Passing,
            "PENDING" | "EXPECTED" => CheckState::Pending,
            "FAILURE" | "ERROR" => CheckState::Failing,
            _ => CheckState::Unknown,
        },
        _ => CheckState::Unknown,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPrChecks {
    title: String,
    url: String,
    #[serde(default)]
    author: Option<RawAuthor>,
    state: String,
    is_draft: bool,
    head_ref_oid: String,
    #[serde(default)]
    status_check_rollup: Vec<RawCheck>,
}

#[derive(Debug, Deserialize)]
struct RawAuthor {
    login: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCheck {
    #[serde(rename = "__typename")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
    #[serde(default)]
    started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    workflow_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCheckRunsResponse {
    #[serde(default)]
    check_runs: Vec<RawCheckRunDetail>,
}

#[derive(Debug, Deserialize)]
struct RawCheckRunDetail {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    output: Option<RawCheckRunOutput>,
}

impl RawCheckRunDetail {
    fn to_details(
        &self,
        annotations: Vec<CheckAnnotation>,
        log_excerpt: Option<CheckLogExcerpt>,
        log_fetch_error: Option<String>,
    ) -> CheckDetails {
        CheckDetails {
            check_run_id: self.id,
            html_url: self.html_url.clone(),
            details_url: self.details_url.clone(),
            summary: self
                .output
                .as_ref()
                .and_then(|output| non_empty_optional(output.summary.clone())),
            text: self
                .output
                .as_ref()
                .and_then(|output| non_empty_optional(output.text.clone())),
            annotations_count: self
                .output
                .as_ref()
                .and_then(|output| output.annotations_count)
                .unwrap_or(annotations.len()),
            annotations,
            log_excerpt,
            log_fetch_error,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawCheckRunOutput {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    annotations_count: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RawCheckAnnotation {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    end_line: Option<u32>,
    #[serde(default)]
    annotation_level: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    raw_details: Option<String>,
}

impl From<RawCheckAnnotation> for CheckAnnotation {
    fn from(raw: RawCheckAnnotation) -> Self {
        Self {
            path: raw.path,
            start_line: raw.start_line,
            end_line: raw.end_line,
            annotation_level: raw.annotation_level,
            title: raw.title,
            message: raw.message,
            raw_details: raw.raw_details,
        }
    }
}

impl RawCheck {
    fn name(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.context.clone())
            .unwrap_or_else(|| "<unknown>".to_string())
    }
}

fn non_empty_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        if value.trim().is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::github::gh::{GhCommandError, GhCommandResult};
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingRunner {
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl GhCommandRunner for RecordingRunner {
        fn run(&self, args: &[String]) -> GhCommandResult<String> {
            self.calls.borrow_mut().push(args.to_vec());
            let joined = args.join(" ");
            if joined.contains("commits/head-sha/check-runs") {
                Ok(r#"{
                      "total_count": 1,
                      "check_runs": [
                        {
                          "id": 42,
                          "name": "build",
                          "html_url": "https://github.com/squareup/example/runs/42",
                          "details_url": "https://github.com/squareup/example/actions/runs/42/job/4242",
                          "output": {
                            "summary": "Build failed",
                            "text": "cargo test failed",
                            "annotations_count": 1
                          }
                        }
                      ]
                    }"#
                .to_string())
            } else if joined.contains("check-runs/42/annotations") {
                Ok(r#"[
                      {
                        "path": "src/lib.rs",
                        "start_line": 12,
                        "end_line": 12,
                        "annotation_level": "failure",
                        "title": "compile",
                        "message": "missing semicolon",
                        "raw_details": "rustc details"
                      }
                    ]"#
                .to_string())
            } else if joined.contains("run view --repo squareup/example --job 4242 --log-failed") {
                Ok(
                    "build\tRun tests\tcargo test failed\nbuild\tRun tests\tthread panicked"
                        .to_string(),
                )
            } else {
                Err(GhCommandError::Failed {
                    status: Some(1),
                    stderr: format!("unexpected args: {joined}"),
                })
            }
        }
    }

    fn check_run(status: &str, conclusion: &str) -> RawCheck {
        RawCheck {
            kind: "CheckRun".to_string(),
            name: Some("build".to_string()),
            context: None,
            status: Some(status.to_string()),
            conclusion: if conclusion.is_empty() {
                None
            } else {
                Some(conclusion.to_string())
            },
            state: None,
            details_url: None,
            target_url: None,
            started_at: None,
            completed_at: None,
            workflow_name: None,
        }
    }

    fn status_context(state: &str) -> RawCheck {
        RawCheck {
            kind: "StatusContext".to_string(),
            name: None,
            context: Some("Kochiku".to_string()),
            status: None,
            conclusion: None,
            state: Some(state.to_string()),
            details_url: None,
            target_url: None,
            started_at: None,
            completed_at: None,
            workflow_name: None,
        }
    }

    #[test]
    fn should_normalize_check_run_states() {
        assert_eq!(
            normalize_state(&check_run("COMPLETED", "SUCCESS")),
            CheckState::Passing
        );
        assert_eq!(
            normalize_state(&check_run("COMPLETED", "FAILURE")),
            CheckState::Failing
        );
        assert_eq!(
            normalize_state(&check_run("IN_PROGRESS", "")),
            CheckState::Pending
        );
        assert_eq!(
            normalize_state(&check_run("COMPLETED", "CANCELLED")),
            CheckState::Cancelled
        );
        assert_eq!(
            normalize_state(&check_run("COMPLETED", "SKIPPED")),
            CheckState::Skipped
        );
    }

    #[test]
    fn should_normalize_status_context_states() {
        assert_eq!(
            normalize_state(&status_context("SUCCESS")),
            CheckState::Passing
        );
        assert_eq!(
            normalize_state(&status_context("PENDING")),
            CheckState::Pending
        );
        assert_eq!(
            normalize_state(&status_context("FAILURE")),
            CheckState::Failing
        );
        assert_eq!(
            normalize_state(&status_context("ERROR")),
            CheckState::Failing
        );
    }

    #[test]
    fn should_mark_only_failing_checks_as_repair_candidates() {
        let item = CheckItem::from(check_run("COMPLETED", "FAILURE"));
        assert!(item.needs_repair);
        let item = CheckItem::from(check_run("COMPLETED", "SUCCESS"));
        assert!(!item.needs_repair);
    }

    #[test]
    fn should_enrich_failing_check_runs_with_annotations() {
        let runner = RecordingRunner::default();
        let repository = ForgeRepository::github("github.com", "squareup", "example");
        let mut checks = vec![CheckItem::from(check_run("COMPLETED", "FAILURE"))];

        enrich_failing_check_runs(&runner, &repository, "head-sha", &mut checks).unwrap();

        let details = checks[0].details.as_ref().expect("details should be set");
        assert_eq!(details.check_run_id, 42);
        assert_eq!(details.summary.as_deref(), Some("Build failed"));
        assert_eq!(details.text.as_deref(), Some("cargo test failed"));
        assert_eq!(details.annotations_count, 1);
        assert_eq!(details.annotations[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(details.annotations[0].message, "missing semicolon");
        let log = details.log_excerpt.as_ref().expect("log excerpt");
        assert_eq!(log.adapter, "github_actions");
        assert_eq!(log.run_id, Some(42));
        assert_eq!(log.job_id, Some(4242));
        assert!(log.text.contains("cargo test failed"));
        assert_eq!(details.log_fetch_error, None);
    }

    #[test]
    fn should_clip_log_excerpt_to_tail() {
        let log = (1..=300)
            .map(|idx| format!("line {idx}"))
            .collect::<Vec<_>>()
            .join("\n");

        let (excerpt, truncated, line_count) = clip_log_excerpt(&log, 10, 10_000);

        assert!(truncated);
        assert_eq!(line_count, 300);
        assert!(!excerpt.contains("line 1"));
        assert!(excerpt.contains("line 291"));
        assert!(excerpt.contains("line 300"));
    }

    #[test]
    fn should_run_external_ci_log_command_with_context_env() {
        let output = run_external_ci_log_command(
            "printf '%s %s %s' \"$TUICR_CI_ADAPTER\" \"$TUICR_CHECK_NAME\" \"$TUICR_HEAD_SHA\"",
            ExternalCiLogContext {
                adapter: "buildkite",
                repository: "squareup/example",
                head_sha: "head-sha",
                check_name: "pipeline",
                check_url: "https://buildkite.com/square/example/builds/1",
                check_run_id: 42,
                html_url: "https://github.com/squareup/example/runs/42",
                details_url: "https://buildkite.com/square/example/builds/1",
            },
        )
        .unwrap();

        assert_eq!(output, "buildkite pipeline head-sha");
    }

    #[test]
    fn should_report_external_ci_log_command_failures() {
        let error = run_external_ci_log_command(
            "echo nope >&2; exit 7",
            ExternalCiLogContext {
                adapter: "kochiku",
                repository: "squareup/example",
                head_sha: "head-sha",
                check_name: "Kochiku",
                check_url: "https://kochiku.example/builds/1",
                check_run_id: 42,
                html_url: "",
                details_url: "https://kochiku.example/builds/1",
            },
        )
        .unwrap_err();

        assert!(error.contains("kochiku log command exited"));
        assert!(error.contains("nope"));
    }
}
