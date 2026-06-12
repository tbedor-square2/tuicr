use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};

use crate::agent::feedback::{
    AGENT_PREFIX, FeedbackItem, FeedbackKind, FeedbackOptions, OutdatedThreadMode,
    collect_feedback_with_runner, map_gh_error, resolve_repo_selector,
};
use crate::agent::state;
use crate::error::{Result, TuicrError};
use crate::forge::github::gh::{GhCommandRunner, SystemGhRunner};
use crate::forge::traits::ForgeRepository;

const DEFAULT_GITHUB_HOST: &str = "github.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyOptions {
    pub repo: String,
    pub pr: u64,
    pub feedback_id: Option<String>,
    pub thread_id: Option<String>,
    pub body: String,
    pub resolve: bool,
    pub expected_head_sha: Option<String>,
    pub dry_run: bool,
    pub allow_non_owned: bool,
    pub robot_logins: Vec<String>,
    pub ignored_comment_patterns: Vec<String>,
    pub outdated_thread_mode: OutdatedThreadMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveOptions {
    pub repo: String,
    pub pr: u64,
    pub thread_id: String,
    pub expected_head_sha: Option<String>,
    pub dry_run: bool,
    pub allow_non_owned: bool,
    pub robot_logins: Vec<String>,
    pub ignored_comment_patterns: Vec<String>,
    pub outdated_thread_mode: OutdatedThreadMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplyReport {
    pub repository: String,
    pub pr: u64,
    pub target: ReplyTarget,
    pub body: String,
    pub dry_run: bool,
    pub reply: Option<GitHubReply>,
    pub resolved: Option<ResolveReport>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolveReport {
    pub repository: String,
    pub pr: u64,
    pub thread_id: String,
    pub dry_run: bool,
    pub is_resolved: Option<bool>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyTarget {
    ReviewThread {
        thread_id: String,
        feedback_id: Option<String>,
    },
    PullRequestComment {
        feedback_id: Option<String>,
        source_url: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubReply {
    pub id: Option<String>,
    pub url: Option<String>,
}

pub fn reply(options: ReplyOptions) -> Result<ReplyReport> {
    reply_with_runner(options, &SystemGhRunner)
}

pub fn resolve_thread(options: ResolveOptions) -> Result<ResolveReport> {
    resolve_thread_with_runner(options, &SystemGhRunner)
}

pub fn read_body_arg(body: Option<String>, input: Option<String>) -> Result<String> {
    match (body, input) {
        (Some(_), Some(_)) => Err(TuicrError::InvalidInput(
            "Use either --body or --input, not both".to_string(),
        )),
        (Some(body), None) => non_empty_body(body),
        (None, Some(input)) => read_input_body(&input).and_then(non_empty_body),
        (None, None) => Err(TuicrError::InvalidInput(
            "A reply body is required; pass --body or --input".to_string(),
        )),
    }
}

fn reply_with_runner<R: GhCommandRunner>(options: ReplyOptions, runner: &R) -> Result<ReplyReport> {
    let repository = resolve_repo_selector(&options.repo)?;
    let feedback = collect_feedback_with_runner(
        FeedbackOptions {
            repo: options.repo.clone(),
            pr: options.pr,
            viewer_login: None,
            robot_logins: options.robot_logins.clone(),
            ignored_comment_patterns: options.ignored_comment_patterns.clone(),
            outdated_thread_mode: options.outdated_thread_mode,
            require_owned_pr: !options.allow_non_owned,
        },
        runner,
    )?;
    if let Some(reason) = feedback.skipped_reason {
        return Err(TuicrError::InvalidInput(reason));
    }
    ensure_expected_head_sha(options.expected_head_sha.as_deref(), &feedback.pr.head_sha)?;

    let target = resolve_reply_target(&options, &feedback.feedback)?;
    let body = ensure_agent_prefix(&options.body);
    if options.dry_run {
        return Ok(ReplyReport {
            repository: repository.display_name(),
            pr: options.pr,
            target,
            body,
            dry_run: true,
            reply: None,
            resolved: None,
            message: "Dry run; no GitHub reply posted".to_string(),
        });
    }

    let reply = match &target {
        ReplyTarget::ReviewThread { thread_id, .. } => {
            post_review_thread_reply(runner, &repository, thread_id, &body)?
        }
        ReplyTarget::PullRequestComment { .. } => {
            post_issue_comment_reply(runner, &repository, options.pr, &body)?
        }
    };
    warn_state_error(
        "record handled feedback",
        state::record_handled_feedback(
            &repository.display_name(),
            options.pr,
            reply_feedback_id(&target),
            reply_thread_id(&target),
            Some(&reply),
        ),
    );

    let resolved = if options.resolve {
        match &target {
            ReplyTarget::ReviewThread { thread_id, .. } => Some(resolve_thread_with_runner(
                ResolveOptions {
                    repo: options.repo.clone(),
                    pr: options.pr,
                    thread_id: thread_id.clone(),
                    expected_head_sha: options.expected_head_sha.clone(),
                    dry_run: false,
                    allow_non_owned: options.allow_non_owned,
                    robot_logins: options.robot_logins.clone(),
                    ignored_comment_patterns: options.ignored_comment_patterns.clone(),
                    outdated_thread_mode: options.outdated_thread_mode,
                },
                runner,
            )?),
            ReplyTarget::PullRequestComment { .. } => None,
        }
    } else {
        None
    };

    Ok(ReplyReport {
        repository: repository.display_name(),
        pr: options.pr,
        target,
        body,
        dry_run: false,
        reply: Some(reply),
        resolved,
        message: "Posted GitHub reply".to_string(),
    })
}

fn resolve_thread_with_runner<R: GhCommandRunner>(
    options: ResolveOptions,
    runner: &R,
) -> Result<ResolveReport> {
    let repository = resolve_repo_selector(&options.repo)?;
    let feedback = collect_feedback_with_runner(
        FeedbackOptions {
            repo: options.repo.clone(),
            pr: options.pr,
            viewer_login: None,
            robot_logins: options.robot_logins.clone(),
            ignored_comment_patterns: options.ignored_comment_patterns.clone(),
            outdated_thread_mode: options.outdated_thread_mode,
            require_owned_pr: !options.allow_non_owned,
        },
        runner,
    )?;
    if let Some(reason) = feedback.skipped_reason {
        return Err(TuicrError::InvalidInput(reason));
    }
    ensure_expected_head_sha(options.expected_head_sha.as_deref(), &feedback.pr.head_sha)?;
    if options.dry_run {
        return Ok(ResolveReport {
            repository: repository.display_name(),
            pr: options.pr,
            thread_id: options.thread_id,
            dry_run: true,
            is_resolved: None,
            message: "Dry run; no GitHub thread resolved".to_string(),
        });
    }

    let is_resolved = resolve_review_thread(runner, &repository, &options.thread_id)?;
    Ok(ResolveReport {
        repository: repository.display_name(),
        pr: options.pr,
        thread_id: options.thread_id,
        dry_run: false,
        is_resolved: Some(is_resolved),
        message: "Resolved GitHub review thread".to_string(),
    })
}

fn ensure_expected_head_sha(expected: Option<&str>, actual: &str) -> Result<()> {
    let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    if expected == actual {
        Ok(())
    } else {
        Err(TuicrError::InvalidInput(format!(
            "PR head changed before GitHub mutation: expected {expected}, current {actual}. Re-read the PR before replying or resolving."
        )))
    }
}

fn resolve_reply_target(options: &ReplyOptions, feedback: &[FeedbackItem]) -> Result<ReplyTarget> {
    match (&options.feedback_id, &options.thread_id) {
        (Some(_), Some(_)) => Err(TuicrError::InvalidInput(
            "Use either --feedback-id or --thread-id, not both".to_string(),
        )),
        (Some(feedback_id), None) => {
            let item = feedback
                .iter()
                .find(|item| {
                    item.id == *feedback_id
                        || item.thread_id.as_deref() == Some(feedback_id.as_str())
                })
                .ok_or_else(|| {
                    TuicrError::InvalidInput(format!(
                        "No actionable feedback matches `{feedback_id}` on this PR"
                    ))
                })?;
            Ok(reply_target_for_feedback(item))
        }
        (None, Some(thread_id)) => Ok(ReplyTarget::ReviewThread {
            thread_id: thread_id.clone(),
            feedback_id: None,
        }),
        (None, None) => Err(TuicrError::InvalidInput(
            "A reply target is required; pass --feedback-id or --thread-id".to_string(),
        )),
    }
}

fn reply_target_for_feedback(item: &FeedbackItem) -> ReplyTarget {
    match item.kind {
        FeedbackKind::ReviewThread => ReplyTarget::ReviewThread {
            thread_id: item.thread_id.clone().unwrap_or_else(|| item.id.clone()),
            feedback_id: Some(item.id.clone()),
        },
        FeedbackKind::IssueComment => ReplyTarget::PullRequestComment {
            feedback_id: Some(item.id.clone()),
            source_url: Some(item.url.clone()),
        },
    }
}

fn reply_feedback_id(target: &ReplyTarget) -> Option<&str> {
    match target {
        ReplyTarget::ReviewThread { feedback_id, .. } => feedback_id.as_deref(),
        ReplyTarget::PullRequestComment { feedback_id, .. } => feedback_id.as_deref(),
    }
}

fn reply_thread_id(target: &ReplyTarget) -> Option<&str> {
    match target {
        ReplyTarget::ReviewThread { thread_id, .. } => Some(thread_id.as_str()),
        ReplyTarget::PullRequestComment { .. } => None,
    }
}

fn warn_state_error(action: &str, result: Result<()>) {
    if let Err(err) = result {
        eprintln!("Warning: Failed to {action}: {err}");
    }
}

fn post_review_thread_reply<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    thread_id: &str,
    body: &str,
) -> Result<GitHubReply> {
    let payload = serde_json::json!({
        "query": "mutation($threadId: ID!, $body: String!) { addPullRequestReviewThreadReply(input: { pullRequestReviewThreadId: $threadId, body: $body }) { comment { id url } } }",
        "variables": {
            "threadId": thread_id,
            "body": body,
        }
    });
    let output = run_graphql_mutation(runner, repository, &payload)?;
    let response: AddThreadReplyResponse = serde_json::from_str(&output)?;
    let comment = response
        .data
        .and_then(|data| data.add_pull_request_review_thread_reply)
        .and_then(|mutation| mutation.comment);
    Ok(GitHubReply {
        id: comment.as_ref().and_then(|comment| comment.id.clone()),
        url: comment.and_then(|comment| comment.url),
    })
}

fn post_issue_comment_reply<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    pr: u64,
    body: &str,
) -> Result<GitHubReply> {
    let endpoint = format!(
        "repos/{}/{}/issues/{}/comments",
        repository.owner, repository.name, pr
    );
    let mut args = vec![
        "api".to_string(),
        endpoint,
        "--method".to_string(),
        "POST".to_string(),
        "--input".to_string(),
        "-".to_string(),
    ];
    if repository.host != DEFAULT_GITHUB_HOST {
        args.push("--hostname".to_string());
        args.push(repository.host.clone());
    }
    let payload = serde_json::json!({ "body": body });
    let output = runner
        .run_with_stdin(&args, &payload.to_string())
        .map_err(|err| map_gh_error(err, &repository.host))?;
    let response: IssueCommentResponse = serde_json::from_str(&output)?;
    Ok(GitHubReply {
        id: response
            .node_id
            .or_else(|| response.id.map(|id| id.to_string())),
        url: response.html_url,
    })
}

fn resolve_review_thread<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    thread_id: &str,
) -> Result<bool> {
    let payload = serde_json::json!({
        "query": "mutation($threadId: ID!) { resolveReviewThread(input: { threadId: $threadId }) { thread { id isResolved } } }",
        "variables": {
            "threadId": thread_id,
        }
    });
    let output = run_graphql_mutation(runner, repository, &payload)?;
    let response: ResolveThreadResponse = serde_json::from_str(&output)?;
    Ok(response
        .data
        .and_then(|data| data.resolve_review_thread)
        .and_then(|mutation| mutation.thread)
        .map(|thread| thread.is_resolved)
        .unwrap_or(false))
}

fn run_graphql_mutation<R: GhCommandRunner>(
    runner: &R,
    repository: &ForgeRepository,
    payload: &serde_json::Value,
) -> Result<String> {
    let mut args = vec![
        "api".to_string(),
        "graphql".to_string(),
        "--input".to_string(),
        "-".to_string(),
    ];
    if repository.host != DEFAULT_GITHUB_HOST {
        args.push("--hostname".to_string());
        args.push(repository.host.clone());
    }
    runner
        .run_with_stdin(&args, &payload.to_string())
        .map_err(|err| map_gh_error(err, &repository.host))
}

fn ensure_agent_prefix(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.starts_with(AGENT_PREFIX) {
        trimmed.to_string()
    } else {
        format!("{AGENT_PREFIX} {trimmed}")
    }
}

fn read_input_body(input: &str) -> Result<String> {
    if input == "-" {
        let mut body = String::new();
        io::stdin().read_to_string(&mut body)?;
        return Ok(body);
    }
    if let Some(path) = input.strip_prefix('@') {
        return Ok(fs::read_to_string(path)?);
    }
    Ok(input.to_string())
}

fn non_empty_body(body: String) -> Result<String> {
    if body.trim().is_empty() {
        Err(TuicrError::InvalidInput(
            "Reply body cannot be empty".to_string(),
        ))
    } else {
        Ok(body)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddThreadReplyResponse {
    data: Option<AddThreadReplyData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddThreadReplyData {
    add_pull_request_review_thread_reply: Option<AddThreadReplyPayload>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddThreadReplyPayload {
    comment: Option<GraphqlComment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlComment {
    id: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveThreadResponse {
    data: Option<ResolveThreadData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveThreadData {
    resolve_review_thread: Option<ResolveThreadPayload>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveThreadPayload {
    thread: Option<ResolvedThread>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolvedThread {
    is_resolved: bool,
}

#[derive(Debug, Deserialize)]
struct IssueCommentResponse {
    id: Option<u64>,
    node_id: Option<String>,
    html_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::github::gh::{GhCommandError, GhCommandResult};
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingRunner {
        calls: RefCell<Vec<(Vec<String>, String)>>,
        response: String,
    }

    impl RecordingRunner {
        fn with_response(response: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                response: response.to_string(),
            }
        }
    }

    impl GhCommandRunner for RecordingRunner {
        fn run(&self, _args: &[String]) -> GhCommandResult<String> {
            Err(GhCommandError::Failed {
                status: Some(1),
                stderr: "unexpected run call".to_string(),
            })
        }

        fn run_with_stdin(&self, args: &[String], stdin: &str) -> GhCommandResult<String> {
            self.calls
                .borrow_mut()
                .push((args.to_vec(), stdin.to_string()));
            Ok(self.response.clone())
        }
    }

    #[test]
    fn should_prefix_agent_replies() {
        assert_eq!(ensure_agent_prefix("Done."), "🤖 Done.");
        assert_eq!(ensure_agent_prefix("🤖 Done."), "🤖 Done.");
    }

    #[test]
    fn should_reject_missing_reply_body() {
        let err = read_body_arg(None, None).unwrap_err();
        assert!(err.to_string().contains("reply body is required"));
    }

    #[test]
    fn should_reject_stale_expected_head_sha() {
        assert!(ensure_expected_head_sha(None, "current").is_ok());
        assert!(ensure_expected_head_sha(Some("current"), "current").is_ok());
        let err = ensure_expected_head_sha(Some("old"), "current").unwrap_err();
        assert!(err.to_string().contains("PR head changed"));
    }

    #[test]
    fn should_choose_review_thread_for_thread_feedback() {
        let item = FeedbackItem {
            kind: FeedbackKind::ReviewThread,
            id: "comment-1".to_string(),
            thread_id: Some("thread-1".to_string()),
            author: None,
            url: "https://example.com/thread".to_string(),
            path: None,
            line: None,
            original_line: None,
            is_outdated: false,
            requires_relevance_check: false,
            body: "Please fix".to_string(),
            diff_hunk: None,
            comments: Vec::new(),
        };
        assert_eq!(
            reply_target_for_feedback(&item),
            ReplyTarget::ReviewThread {
                thread_id: "thread-1".to_string(),
                feedback_id: Some("comment-1".to_string()),
            }
        );
    }

    #[test]
    fn should_choose_pr_comment_for_issue_feedback() {
        let item = FeedbackItem {
            kind: FeedbackKind::IssueComment,
            id: "comment-2".to_string(),
            thread_id: None,
            author: None,
            url: "https://example.com/comment".to_string(),
            path: None,
            line: None,
            original_line: None,
            is_outdated: false,
            requires_relevance_check: false,
            body: "Please fix".to_string(),
            diff_hunk: None,
            comments: Vec::new(),
        };
        assert_eq!(
            reply_target_for_feedback(&item),
            ReplyTarget::PullRequestComment {
                feedback_id: Some("comment-2".to_string()),
                source_url: Some("https://example.com/comment".to_string()),
            }
        );
    }

    #[test]
    fn should_post_review_thread_reply_with_graphql_input() {
        let runner = RecordingRunner::with_response(
            r#"{
              "data": {
                "addPullRequestReviewThreadReply": {
                  "comment": {
                    "id": "PRRC_reply",
                    "url": "https://github.com/squareup/example/pull/1#discussion_r1"
                  }
                }
              }
            }"#,
        );
        let repo = ForgeRepository::github("github.com", "squareup", "example");
        let reply = post_review_thread_reply(&runner, &repo, "PRRT_1", "🤖 Fixed.").unwrap();

        assert_eq!(reply.id.as_deref(), Some("PRRC_reply"));
        assert_eq!(
            reply.url.as_deref(),
            Some("https://github.com/squareup/example/pull/1#discussion_r1")
        );
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            vec![
                "api".to_string(),
                "graphql".to_string(),
                "--input".to_string(),
                "-".to_string()
            ]
        );
        let payload: serde_json::Value = serde_json::from_str(&calls[0].1).unwrap();
        assert!(
            payload["query"]
                .as_str()
                .unwrap()
                .contains("addPullRequestReviewThreadReply")
        );
        assert_eq!(payload["variables"]["threadId"], "PRRT_1");
        assert_eq!(payload["variables"]["body"], "🤖 Fixed.");
    }

    #[test]
    fn should_post_issue_comment_reply_with_rest_input() {
        let runner = RecordingRunner::with_response(
            r#"{
              "id": 123,
              "node_id": "IC_123",
              "html_url": "https://github.com/squareup/example/pull/1#issuecomment-123"
            }"#,
        );
        let repo = ForgeRepository::github("github.com", "squareup", "example");
        let reply = post_issue_comment_reply(&runner, &repo, 1, "🤖 Fixed.").unwrap();

        assert_eq!(reply.id.as_deref(), Some("IC_123"));
        assert_eq!(
            reply.url.as_deref(),
            Some("https://github.com/squareup/example/pull/1#issuecomment-123")
        );
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            vec![
                "api".to_string(),
                "repos/squareup/example/issues/1/comments".to_string(),
                "--method".to_string(),
                "POST".to_string(),
                "--input".to_string(),
                "-".to_string()
            ]
        );
        let payload: serde_json::Value = serde_json::from_str(&calls[0].1).unwrap();
        assert_eq!(payload["body"], "🤖 Fixed.");
    }

    #[test]
    fn should_resolve_review_thread_with_graphql_input() {
        let runner = RecordingRunner::with_response(
            r#"{
              "data": {
                "resolveReviewThread": {
                  "thread": {
                    "id": "PRRT_1",
                    "isResolved": true
                  }
                }
              }
            }"#,
        );
        let repo = ForgeRepository::github("github.com", "squareup", "example");
        let resolved = resolve_review_thread(&runner, &repo, "PRRT_1").unwrap();

        assert!(resolved);
        let calls = runner.calls.borrow();
        let payload: serde_json::Value = serde_json::from_str(&calls[0].1).unwrap();
        assert!(
            payload["query"]
                .as_str()
                .unwrap()
                .contains("resolveReviewThread")
        );
        assert_eq!(payload["variables"]["threadId"], "PRRT_1");
    }
}
