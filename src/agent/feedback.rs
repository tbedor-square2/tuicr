use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{Result, TuicrError};
use crate::forge::detect_github_repository;
use crate::forge::github::gh::{
    GhCommandError, GhCommandRunner, GitHubGhBackend, SystemGhRunner, parse_github_remote_url,
};
use crate::forge::remote_comments::{RemoteReviewComment, RemoteReviewThread};
use crate::forge::traits::{ForgeBackend, ForgeRepository, PullRequestDetails, PullRequestTarget};

const DEFAULT_GITHUB_HOST: &str = "github.com";
pub const AGENT_PREFIX: &str = "🤖";

const DEFAULT_ROBOT_LOGINS: &[&str] = &[
    "github-actions[bot]",
    "github-actions",
    "sq-renovate-bot",
    "renovate[bot]",
    "renovate",
    "dependabot[bot]",
    "dependabot",
    "coderabbitai[bot]",
    "coderabbitai",
    "cursor[bot]",
    "cursor",
    "chatgpt-codex-connector[bot]",
    "chatgpt-codex-connector",
];

const IGNORED_COMMENT_MARKERS: &[&str] = &[
    "static.graphite.dev",
    "<!-- Current dependencies on/for this PR:",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackOptions {
    pub repo: String,
    pub pr: u64,
    pub viewer_login: Option<String>,
    pub robot_logins: Vec<String>,
    pub ignored_comment_patterns: Vec<String>,
    pub outdated_thread_mode: OutdatedThreadMode,
    pub require_owned_pr: bool,
}

impl Default for FeedbackOptions {
    fn default() -> Self {
        Self {
            repo: ".".to_string(),
            pr: 0,
            viewer_login: None,
            robot_logins: DEFAULT_ROBOT_LOGINS
                .iter()
                .map(|login| login.to_string())
                .collect(),
            ignored_comment_patterns: Vec::new(),
            outdated_thread_mode: OutdatedThreadMode::Recheck,
            require_owned_pr: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutdatedThreadMode {
    Recheck,
    Include,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedbackReport {
    pub repository: String,
    pub pr: PrFeedbackSummary,
    pub viewer_login: String,
    pub owned_by_viewer: bool,
    pub skipped_reason: Option<String>,
    pub feedback: Vec<FeedbackItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrFeedbackSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    pub state: String,
    pub is_draft: bool,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    ReviewThread,
    IssueComment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedbackItem {
    pub kind: FeedbackKind,
    pub id: String,
    pub thread_id: Option<String>,
    pub author: Option<String>,
    pub url: String,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    pub is_outdated: bool,
    pub requires_relevance_check: bool,
    pub body: String,
    pub diff_hunk: Option<String>,
    pub comments: Vec<FeedbackComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedbackComment {
    pub id: String,
    pub author: Option<String>,
    pub url: String,
    pub body: String,
    pub created_at: Option<DateTime<Utc>>,
    pub is_agent_reply: bool,
}

pub fn collect_feedback(options: FeedbackOptions) -> Result<FeedbackReport> {
    collect_feedback_with_runner(options, &SystemGhRunner)
}

pub(crate) fn collect_feedback_with_runner<R: GhCommandRunner>(
    options: FeedbackOptions,
    runner: &R,
) -> Result<FeedbackReport> {
    let repository = resolve_repo_selector(&options.repo)?;
    let viewer_login = match options.viewer_login {
        Some(login) => login,
        None => fetch_viewer_login(runner, &repository.host)?,
    };
    let robot_logins = build_robot_logins(&options.robot_logins);
    let ignored_comment_patterns = options.ignored_comment_patterns;

    let backend = GitHubGhBackend::with_runner(Some(repository.clone()), runner);
    let pr = backend.get_pull_request(PullRequestTarget::with_repository(
        repository.clone(),
        options.pr,
        options.pr.to_string(),
    ))?;
    let owned_by_viewer = pr.author.as_deref() == Some(viewer_login.as_str());
    if options.require_owned_pr && !owned_by_viewer {
        return Ok(report(
            &repository,
            &pr,
            viewer_login,
            owned_by_viewer,
            Some("PR is not authored by the configured user".to_string()),
            Vec::new(),
        ));
    }

    let threads = backend.list_review_threads(&pr)?;
    let issue_comments = fetch_issue_comments(runner, &repository, pr.number)?;

    let mut feedback = collect_review_thread_feedback(
        &threads,
        &viewer_login,
        &robot_logins,
        &ignored_comment_patterns,
        options.outdated_thread_mode,
    );
    feedback.extend(collect_issue_comment_feedback(
        &issue_comments,
        &viewer_login,
        &robot_logins,
        &ignored_comment_patterns,
    ));

    Ok(report(
        &repository,
        &pr,
        viewer_login,
        owned_by_viewer,
        None,
        feedback,
    ))
}

fn report(
    repository: &ForgeRepository,
    pr: &PullRequestDetails,
    viewer_login: String,
    owned_by_viewer: bool,
    skipped_reason: Option<String>,
    feedback: Vec<FeedbackItem>,
) -> FeedbackReport {
    FeedbackReport {
        repository: repository.display_name(),
        pr: PrFeedbackSummary {
            number: pr.number,
            title: pr.title.clone(),
            url: pr.url.clone(),
            author: pr.author.clone(),
            state: pr.state.clone(),
            is_draft: pr.is_draft,
            head_ref_name: pr.head_ref_name.clone(),
            base_ref_name: pr.base_ref_name.clone(),
            head_sha: pr.head_sha.clone(),
        },
        viewer_login,
        owned_by_viewer,
        skipped_reason,
        feedback,
    }
}

fn collect_review_thread_feedback(
    threads: &[RemoteReviewThread],
    viewer_login: &str,
    robot_logins: &HashSet<String>,
    ignored_comment_patterns: &[String],
    outdated_thread_mode: OutdatedThreadMode,
) -> Vec<FeedbackItem> {
    let mut items = Vec::new();
    for thread in threads {
        if thread.is_resolved {
            continue;
        }
        if thread.is_outdated && outdated_thread_mode == OutdatedThreadMode::Ignore {
            continue;
        }
        let Some(latest_actor_comment) = latest_actor_review_comment(
            thread,
            viewer_login,
            robot_logins,
            ignored_comment_patterns,
        ) else {
            continue;
        };
        if has_newer_agent_review_reply(thread, latest_actor_comment) {
            continue;
        }
        items.push(FeedbackItem {
            kind: FeedbackKind::ReviewThread,
            id: latest_actor_comment.id.clone(),
            thread_id: Some(thread.id.clone()),
            author: latest_actor_comment.author.clone(),
            url: latest_actor_comment.url.clone(),
            path: if thread.path.is_empty() {
                None
            } else {
                Some(thread.path.clone())
            },
            line: thread.current_line,
            original_line: thread.original_line,
            is_outdated: thread.is_outdated,
            requires_relevance_check: outdated_thread_mode == OutdatedThreadMode::Recheck
                && (thread.is_outdated || thread.line != thread.original_line),
            body: latest_actor_comment.body.clone(),
            diff_hunk: latest_actor_comment.diff_hunk.clone(),
            comments: thread
                .comments
                .iter()
                .map(|comment| feedback_comment_from_review(comment))
                .collect(),
        });
    }
    items
}

fn collect_issue_comment_feedback(
    comments: &[IssueComment],
    viewer_login: &str,
    robot_logins: &HashSet<String>,
    ignored_comment_patterns: &[String],
) -> Vec<FeedbackItem> {
    let agent_replies = comments
        .iter()
        .filter(|comment| is_agent_body(&comment.body))
        .collect::<Vec<_>>();
    comments
        .iter()
        .filter(|comment| {
            is_actor(comment.author().as_deref(), viewer_login, robot_logins)
                && !is_agent_body(&comment.body)
                && !is_ignored_comment_body(&comment.body, ignored_comment_patterns)
                && !has_newer_agent_issue_reply(comment, &agent_replies)
        })
        .map(|comment| FeedbackItem {
            kind: FeedbackKind::IssueComment,
            id: comment
                .node_id
                .clone()
                .unwrap_or_else(|| comment.id.to_string()),
            thread_id: None,
            author: comment.author(),
            url: comment.url.clone(),
            path: None,
            line: None,
            original_line: None,
            is_outdated: false,
            requires_relevance_check: false,
            body: comment.body.clone(),
            diff_hunk: None,
            comments: vec![FeedbackComment {
                id: comment
                    .node_id
                    .clone()
                    .unwrap_or_else(|| comment.id.to_string()),
                author: comment.author(),
                url: comment.url.clone(),
                body: comment.body.clone(),
                created_at: comment.created_at,
                is_agent_reply: false,
            }],
        })
        .collect()
}

fn latest_actor_review_comment<'a>(
    thread: &'a RemoteReviewThread,
    viewer_login: &str,
    robot_logins: &HashSet<String>,
    ignored_comment_patterns: &[String],
) -> Option<&'a RemoteReviewComment> {
    thread
        .comments
        .iter()
        .filter(|comment| {
            is_actor(comment.author.as_deref(), viewer_login, robot_logins)
                && !is_agent_body(&comment.body)
                && !is_ignored_comment_body(&comment.body, ignored_comment_patterns)
        })
        .max_by(|left, right| left.created_at.cmp(&right.created_at))
}

fn has_newer_agent_review_reply(
    thread: &RemoteReviewThread,
    actor_comment: &RemoteReviewComment,
) -> bool {
    thread.comments.iter().any(|comment| {
        is_agent_body(&comment.body)
            && created_at_is_after_or_equal(comment.created_at, actor_comment.created_at)
    })
}

fn has_newer_agent_issue_reply(comment: &IssueComment, agent_replies: &[&IssueComment]) -> bool {
    agent_replies.iter().any(|reply| {
        created_at_is_after_or_equal(reply.created_at, comment.created_at)
            && !reply.body.trim().is_empty()
    })
}

fn feedback_comment_from_review(comment: &RemoteReviewComment) -> FeedbackComment {
    FeedbackComment {
        id: comment.id.clone(),
        author: comment.author.clone(),
        url: comment.url.clone(),
        body: comment.body.clone(),
        created_at: comment.created_at,
        is_agent_reply: is_agent_body(&comment.body),
    }
}

fn created_at_is_after_or_equal(
    maybe_later: Option<DateTime<Utc>>,
    maybe_earlier: Option<DateTime<Utc>>,
) -> bool {
    match (maybe_later, maybe_earlier) {
        (Some(later), Some(earlier)) => later >= earlier,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn build_robot_logins(configured: &[String]) -> HashSet<String> {
    DEFAULT_ROBOT_LOGINS
        .iter()
        .map(|login| login.to_ascii_lowercase())
        .chain(configured.iter().map(|login| login.to_ascii_lowercase()))
        .collect()
}

fn is_actor(login: Option<&str>, viewer_login: &str, robot_logins: &HashSet<String>) -> bool {
    let Some(login) = login else {
        return false;
    };
    login == viewer_login || is_robot_login(login, robot_logins)
}

fn is_robot_login(login: &str, robot_logins: &HashSet<String>) -> bool {
    let normalized = login.to_ascii_lowercase();
    robot_logins.contains(&normalized)
        || normalized.ends_with("[bot]")
        || normalized.ends_with("-bot")
        || normalized.contains("bot")
}

fn is_agent_body(body: &str) -> bool {
    body.trim_start().starts_with(AGENT_PREFIX)
}

fn is_ignored_comment_body(body: &str, configured_patterns: &[String]) -> bool {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.starts_with("r:") {
        return true;
    }
    if trimmed.contains("This stack of pull requests is managed by") && trimmed.contains("Graphite")
    {
        return true;
    }
    IGNORED_COMMENT_MARKERS
        .iter()
        .any(|marker| trimmed.contains(marker))
        || configured_patterns
            .iter()
            .any(|pattern| comment_pattern_matches(trimmed, pattern))
}

fn comment_pattern_matches(body: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    if pattern.starts_with('^') && pattern.ends_with('$') && pattern.len() >= 2 {
        body == &pattern[1..pattern.len() - 1]
    } else if let Some(prefix) = pattern.strip_prefix('^') {
        body.starts_with(prefix)
    } else if let Some(suffix) = pattern.strip_suffix('$') {
        body.ends_with(suffix)
    } else {
        body.contains(pattern)
    }
}

pub(crate) fn resolve_repo_selector(selector: &str) -> Result<ForgeRepository> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err(TuicrError::InvalidInput(
            "--repo cannot be empty".to_string(),
        ));
    }

    let path = PathBuf::from(trimmed);
    if is_path_like(trimmed) && path.exists() {
        return detect_github_repository(&path).ok_or_else(|| {
            TuicrError::Forge(format!(
                "Could not detect a GitHub remote for checkout `{}`",
                path.display()
            ))
        });
    }

    if let Some(repo) = parse_github_remote_url(trimmed) {
        return Ok(repo);
    }

    let parts = trimmed
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [owner, name] => Ok(ForgeRepository::github(DEFAULT_GITHUB_HOST, *owner, *name)),
        [host, owner, name] => Ok(ForgeRepository::github(*host, *owner, *name)),
        _ => Err(TuicrError::InvalidInput(format!(
            "Could not parse repository selector `{trimmed}`. Use a checkout path, owner/repo, host/owner/repo, or GitHub URL."
        ))),
    }
}

fn is_path_like(selector: &str) -> bool {
    selector == "."
        || selector == ".."
        || selector.starts_with('/')
        || selector.starts_with("./")
        || selector.starts_with("../")
        || Path::new(selector).exists()
}

fn fetch_viewer_login<R: GhCommandRunner>(runner: &R, host: &str) -> Result<String> {
    let mut args = vec![
        "api".to_string(),
        "user".to_string(),
        "--jq".to_string(),
        ".login".to_string(),
    ];
    if host != DEFAULT_GITHUB_HOST {
        args.push("--hostname".to_string());
        args.push(host.to_string());
    }
    runner
        .run(&args)
        .map(|output| output.trim().to_string())
        .map_err(|err| map_gh_error(err, host))
        .and_then(|login| {
            if login.is_empty() {
                Err(TuicrError::Forge(
                    "GitHub viewer login response was empty".to_string(),
                ))
            } else {
                Ok(login)
            }
        })
}

#[derive(Debug, Clone, Deserialize)]
struct IssueComment {
    id: u64,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    body: String,
    #[serde(default, rename = "html_url")]
    url: String,
    #[serde(default)]
    user: Option<IssueCommentUser>,
    #[serde(default)]
    created_at: Option<DateTime<Utc>>,
}

impl IssueComment {
    fn author(&self) -> Option<String> {
        self.user.as_ref().map(|user| user.login.clone())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct IssueCommentUser {
    login: String,
}

fn fetch_issue_comments<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    pr_number: u64,
) -> Result<Vec<IssueComment>> {
    let mut comments = Vec::new();
    for page in 1..=100 {
        let endpoint = format!(
            "repos/{}/{}/issues/{}/comments?per_page=100&page={}",
            repository.owner, repository.name, pr_number, page
        );
        let mut args = vec!["api".to_string()];
        if repository.host != DEFAULT_GITHUB_HOST {
            args.push("--hostname".to_string());
            args.push(repository.host.clone());
        }
        args.push(endpoint);
        let output = runner
            .run(&args)
            .map_err(|err| map_gh_error(err, &repository.host))?;
        let page_comments: Vec<IssueComment> = serde_json::from_str(&output)?;
        let received = page_comments.len();
        comments.extend(page_comments);
        if received < 100 {
            break;
        }
    }
    Ok(comments)
}

pub(crate) fn map_gh_error(error: GhCommandError, host: &str) -> TuicrError {
    match error {
        GhCommandError::MissingGh => {
            TuicrError::Forge("GitHub CLI `gh` is required for PR feedback discovery".to_string())
        }
        GhCommandError::Failed { status, stderr } => {
            let status_text = status
                .map(|status| format!("exit status {status}"))
                .unwrap_or_else(|| "unknown status".to_string());
            TuicrError::Forge(format!(
                "gh command failed for {host} ({status_text}): {stderr}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::remote_comments::{RemoteCommentSide, RemoteReviewComment};

    fn comment(id: &str, author: &str, body: &str, created_at: &str) -> RemoteReviewComment {
        RemoteReviewComment {
            id: id.to_string(),
            author: Some(author.to_string()),
            body: body.to_string(),
            created_at: Some(created_at.parse().unwrap()),
            diff_hunk: None,
            in_reply_to: None,
            url: format!("https://example.com/{id}"),
        }
    }

    fn thread(comments: Vec<RemoteReviewComment>) -> RemoteReviewThread {
        RemoteReviewThread {
            id: "thread-1".to_string(),
            path: "src/lib.rs".to_string(),
            line: Some(42),
            current_line: Some(42),
            original_line: Some(42),
            side: RemoteCommentSide::Right,
            is_resolved: false,
            is_outdated: false,
            comments,
        }
    }

    #[test]
    fn should_collect_latest_user_thread_comment_without_agent_reply() {
        let thread = thread(vec![comment(
            "c1",
            "alice",
            "Please fix this",
            "2026-06-11T12:00:00Z",
        )]);
        let items = collect_review_thread_feedback(
            &[thread],
            "alice",
            &HashSet::new(),
            &[],
            OutdatedThreadMode::Recheck,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "c1");
    }

    #[test]
    fn should_skip_thread_when_newer_agent_reply_exists() {
        let thread = thread(vec![
            comment("c1", "alice", "Please fix this", "2026-06-11T12:00:00Z"),
            comment(
                "c2",
                "agent",
                "🤖 Fixed in the latest push.",
                "2026-06-11T12:05:00Z",
            ),
        ]);
        let items = collect_review_thread_feedback(
            &[thread],
            "alice",
            &HashSet::new(),
            &[],
            OutdatedThreadMode::Recheck,
        );
        assert!(items.is_empty());
    }

    #[test]
    fn should_include_outdated_thread_for_relevance_check() {
        let mut thread = thread(vec![comment(
            "c1",
            "coderabbitai[bot]",
            "This may still matter",
            "2026-06-11T12:00:00Z",
        )]);
        thread.is_outdated = true;
        thread.line = None;
        thread.current_line = None;
        thread.original_line = Some(19);
        let items = collect_review_thread_feedback(
            &[thread],
            "alice",
            &HashSet::new(),
            &[],
            OutdatedThreadMode::Recheck,
        );
        assert_eq!(items.len(), 1);
        assert!(items[0].requires_relevance_check);
        assert_eq!(items[0].original_line, Some(19));
    }

    #[test]
    fn should_honor_outdated_thread_mode() {
        let mut thread = thread(vec![comment(
            "c1",
            "coderabbitai[bot]",
            "This may still matter",
            "2026-06-11T12:00:00Z",
        )]);
        thread.is_outdated = true;

        let included = collect_review_thread_feedback(
            &[thread.clone()],
            "alice",
            &HashSet::new(),
            &[],
            OutdatedThreadMode::Include,
        );
        assert_eq!(included.len(), 1);
        assert!(!included[0].requires_relevance_check);

        let ignored = collect_review_thread_feedback(
            &[thread],
            "alice",
            &HashSet::new(),
            &[],
            OutdatedThreadMode::Ignore,
        );
        assert!(ignored.is_empty());
    }

    #[test]
    fn should_ignore_graphite_metadata() {
        assert!(is_ignored_comment_body(
            "This stack of pull requests is managed by Graphite",
            &[]
        ));
        assert!(is_ignored_comment_body("r: some-reviewer", &[]));
        assert!(is_ignored_comment_body(
            "r: reviewer\ncc: @someone\n<details>",
            &[]
        ));
    }

    #[test]
    fn should_ignore_configured_comment_patterns() {
        let patterns = vec![
            "Generated by owner-owl".to_string(),
            "^route:".to_string(),
            "no action needed$".to_string(),
            "^exact body$".to_string(),
        ];

        assert!(is_ignored_comment_body(
            "Generated by owner-owl\nteam: risk",
            &patterns
        ));
        assert!(is_ignored_comment_body("route: ml-platform", &patterns));
        assert!(is_ignored_comment_body(
            "This is only metadata; no action needed",
            &patterns
        ));
        assert!(is_ignored_comment_body("exact body", &patterns));
        assert!(!is_ignored_comment_body("exact body plus more", &patterns));
    }
}
