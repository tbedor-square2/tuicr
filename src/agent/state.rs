use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::agent::ci::{CheckItem, CheckState, ChecksReport};
use crate::agent::feedback::{FeedbackItem, FeedbackReport};
use crate::agent::github_actions::GitHubReply;
use crate::agent::pr_list::{PrListItem, PrListReport};
use crate::error::{Result, TuicrError};

const STATE_LOCK_RETRIES: usize = 100;
const STATE_LOCK_SLEEP: Duration = Duration::from_millis(20);

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentState {
    #[serde(default)]
    pub watched_prs: BTreeMap<String, WatchedPrState>,
    #[serde(default)]
    pub pending_feedback: BTreeMap<String, FeedbackState>,
    #[serde(default)]
    pub handled_feedback: BTreeMap<String, HandledFeedbackState>,
    #[serde(default)]
    pub check_snapshots: BTreeMap<String, CheckSnapshotState>,
    #[serde(default)]
    pub repo_worktrees: BTreeMap<String, RepoWorktreeState>,
    #[serde(default)]
    pub agent_replies: BTreeMap<String, AgentReplyState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchedPrState {
    pub repository: String,
    pub pr: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub author: String,
    pub owners: Vec<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackState {
    pub repository: String,
    pub pr: u64,
    pub feedback_id: String,
    pub thread_id: Option<String>,
    pub author: Option<String>,
    pub url: String,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandledFeedbackState {
    pub repository: String,
    pub pr: u64,
    pub feedback_id: String,
    pub thread_id: Option<String>,
    pub reply_id: Option<String>,
    pub reply_url: Option<String>,
    pub handled_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSnapshotState {
    pub repository: String,
    pub pr: u64,
    pub head_sha: String,
    pub overall_state: CheckState,
    pub failing_checks: Vec<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoWorktreeState {
    pub repository: String,
    pub pr: u64,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub source: Option<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReplyState {
    pub repository: String,
    pub pr: u64,
    pub feedback_id: Option<String>,
    pub thread_id: Option<String>,
    pub reply_id: Option<String>,
    pub reply_url: Option<String>,
    pub posted_at: DateTime<Utc>,
}

pub fn record_watched_prs(report: &PrListReport) -> Result<()> {
    let observed_at = Utc::now();
    with_state(|state| {
        for pr in &report.pull_requests {
            state.watched_prs.insert(
                pr_key(&pr.repository, pr.number),
                watched_pr_state(report, pr, observed_at),
            );
        }
        Ok(())
    })
}

pub fn record_pending_feedback(report: &FeedbackReport) -> Result<()> {
    let observed_at = Utc::now();
    with_state(|state| {
        for item in &report.feedback {
            state.pending_feedback.insert(
                feedback_key(&report.repository, report.pr.number, &item.id),
                feedback_state(report, item, observed_at),
            );
        }
        Ok(())
    })
}

pub fn record_handled_feedback(
    repository: &str,
    pr: u64,
    feedback_id: Option<&str>,
    thread_id: Option<&str>,
    reply: Option<&GitHubReply>,
) -> Result<()> {
    let Some(feedback_id) = feedback_id.or(thread_id) else {
        return Ok(());
    };
    let handled_at = Utc::now();
    let reply_id = reply.and_then(|reply| reply.id.clone());
    let reply_url = reply.and_then(|reply| reply.url.clone());
    with_state(|state| {
        state.handled_feedback.insert(
            feedback_key(repository, pr, feedback_id),
            HandledFeedbackState {
                repository: repository.to_string(),
                pr,
                feedback_id: feedback_id.to_string(),
                thread_id: thread_id.map(str::to_string),
                reply_id: reply_id.clone(),
                reply_url: reply_url.clone(),
                handled_at,
            },
        );
        state.agent_replies.insert(
            format!(
                "{}#{}",
                feedback_key(repository, pr, feedback_id),
                handled_at.timestamp_millis()
            ),
            AgentReplyState {
                repository: repository.to_string(),
                pr,
                feedback_id: Some(feedback_id.to_string()),
                thread_id: thread_id.map(str::to_string),
                reply_id,
                reply_url,
                posted_at: handled_at,
            },
        );
        Ok(())
    })
}

pub fn record_check_snapshot(report: &ChecksReport) -> Result<()> {
    let observed_at = Utc::now();
    with_state(|state| {
        state.check_snapshots.insert(
            pr_key(&report.repository, report.pr.number),
            CheckSnapshotState {
                repository: report.repository.clone(),
                pr: report.pr.number,
                head_sha: report.pr.head_sha.clone(),
                overall_state: report.overall_state,
                failing_checks: report
                    .repair_candidates
                    .iter()
                    .map(check_name)
                    .collect::<Vec<_>>(),
                observed_at,
            },
        );
        Ok(())
    })
}

pub fn record_repo_worktree(
    repository: &str,
    pr: u64,
    path: &Path,
    branch: Option<&str>,
    source: Option<&str>,
) -> Result<()> {
    let observed_at = Utc::now();
    with_state(|state| {
        state.repo_worktrees.insert(
            pr_key(repository, pr),
            RepoWorktreeState {
                repository: repository.to_string(),
                pr,
                path: path.to_path_buf(),
                branch: branch.map(str::to_string),
                source: source.map(str::to_string),
                observed_at,
            },
        );
        Ok(())
    })
}

fn with_state<F>(update: F) -> Result<()>
where
    F: FnOnce(&mut AgentState) -> Result<()>,
{
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let _lock = StateLock::acquire(&lock_path)?;
    let mut state = read_state(&path)?;
    update(&mut state)?;
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&state)?))?;
    Ok(())
}

fn read_state(path: &Path) -> Result<AgentState> {
    if !path.exists() {
        return Ok(AgentState::default());
    }
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn state_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| {
        TuicrError::Forge("HOME must be set to write tuicr agent state".to_string())
    })?;
    Ok(PathBuf::from(home).join(".local/state/tuicr/agent-state.json"))
}

struct StateLock {
    path: PathBuf,
}

impl StateLock {
    fn acquire(path: &Path) -> Result<Self> {
        for _ in 0..STATE_LOCK_RETRIES {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    writeln!(file, "pid={}", std::process::id())?;
                    writeln!(file, "created_at={}", Utc::now().to_rfc3339())?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    thread::sleep(STATE_LOCK_SLEEP);
                }
                Err(err) => return Err(err.into()),
            }
        }
        Err(TuicrError::Forge(format!(
            "Timed out waiting for agent state lock {}",
            path.display()
        )))
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn watched_pr_state(
    report: &PrListReport,
    pr: &PrListItem,
    observed_at: DateTime<Utc>,
) -> WatchedPrState {
    WatchedPrState {
        repository: pr.repository.clone(),
        pr: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        state: pr.state.clone(),
        author: report.author.clone(),
        owners: report.owners.clone(),
        updated_at: pr.updated_at,
        observed_at,
    }
}

fn feedback_state(
    report: &FeedbackReport,
    item: &FeedbackItem,
    observed_at: DateTime<Utc>,
) -> FeedbackState {
    FeedbackState {
        repository: report.repository.clone(),
        pr: report.pr.number,
        feedback_id: item.id.clone(),
        thread_id: item.thread_id.clone(),
        author: item.author.clone(),
        url: item.url.clone(),
        observed_at,
    }
}

fn check_name(check: &CheckItem) -> String {
    check.name.clone()
}

fn pr_key(repository: &str, pr: u64) -> String {
    format!("{repository}#{pr}")
}

fn feedback_key(repository: &str, pr: u64, feedback_id: &str) -> String {
    format!("{}#{}", pr_key(repository, pr), feedback_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_build_stable_state_keys() {
        assert_eq!(pr_key("squareup/java", 480718), "squareup/java#480718");
        assert_eq!(
            feedback_key("squareup/java", 480718, "PRRC_1"),
            "squareup/java#480718#PRRC_1"
        );
    }
}
