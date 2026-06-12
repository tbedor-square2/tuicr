use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::agent::ci::CheckItem;
use crate::agent::notification::NotificationAttempt;
use crate::error::{Result, TuicrError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CiRetryDecision {
    pub exhausted: bool,
    pub attempts: Vec<CiRetryAttemptView>,
    pub exhausted_checks: Vec<CiRetryAttemptView>,
    pub remaining_checks: Vec<CiRetryAttemptView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CiRetryAttemptView {
    pub check_name: String,
    pub attempts: u32,
    pub max_attempts: u32,
    pub key: String,
}

pub fn decide_ci_retries(
    repository: &str,
    pr: u64,
    head_sha: &str,
    failing_checks: &[CheckItem],
    max_attempts: u32,
) -> Result<CiRetryDecision> {
    let store = CiRetryStore::load()?;
    Ok(store.decision(repository, pr, head_sha, failing_checks, max_attempts))
}

pub fn record_ci_retry_attempts(
    repository: &str,
    pr: u64,
    head_sha: &str,
    failing_checks: &[CheckItem],
) -> Result<Vec<CiRetryAttemptView>> {
    let mut store = CiRetryStore::load()?;
    let attempts = store.record_attempts(repository, pr, head_sha, failing_checks);
    store.save()?;
    Ok(attempts)
}

pub fn needs_ci_retry_exhausted_notification(
    exhausted_checks: &[CiRetryAttemptView],
) -> Result<bool> {
    let store = CiRetryStore::load()?;
    Ok(exhausted_checks.iter().any(|check| {
        store
            .attempts
            .get(&check.key)
            .is_some_and(|attempt| attempt.exhausted_notification.is_none())
    }))
}

pub fn record_ci_retry_exhausted_notification(
    exhausted_checks: &[CiRetryAttemptView],
    notification: NotificationAttempt,
) -> Result<()> {
    let mut store = CiRetryStore::load()?;
    for check in exhausted_checks {
        if let Some(attempt) = store.attempts.get_mut(&check.key)
            && attempt.exhausted_notification.is_none()
        {
            attempt.exhausted_notification = Some(notification.clone());
        }
    }
    store.save()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CiRetryStore {
    #[serde(default)]
    attempts: BTreeMap<String, CiRetryAttempt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CiRetryAttempt {
    repository: String,
    pr: u64,
    head_sha: String,
    check_name: String,
    attempts: u32,
    first_attempted_at: DateTime<Utc>,
    last_attempted_at: DateTime<Utc>,
    #[serde(default)]
    exhausted_notification: Option<NotificationAttempt>,
}

impl CiRetryStore {
    fn load() -> Result<Self> {
        let path = retry_state_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    fn save(&self) -> Result<()> {
        let path = retry_state_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, format!("{}\n", serde_json::to_string_pretty(self)?))?;
        Ok(())
    }

    fn decision(
        &self,
        repository: &str,
        pr: u64,
        head_sha: &str,
        failing_checks: &[CheckItem],
        max_attempts: u32,
    ) -> CiRetryDecision {
        let attempts = failing_checks
            .iter()
            .map(|check| {
                let key = retry_key(repository, pr, head_sha, &check.name);
                let attempts = self
                    .attempts
                    .get(&key)
                    .map(|attempt| attempt.attempts)
                    .unwrap_or(0);
                CiRetryAttemptView {
                    check_name: check.name.clone(),
                    attempts,
                    max_attempts,
                    key,
                }
            })
            .collect::<Vec<_>>();
        let exhausted_checks = attempts
            .iter()
            .filter(|attempt| attempt.attempts >= max_attempts)
            .cloned()
            .collect::<Vec<_>>();
        let remaining_checks = attempts
            .iter()
            .filter(|attempt| attempt.attempts < max_attempts)
            .cloned()
            .collect::<Vec<_>>();
        CiRetryDecision {
            exhausted: !failing_checks.is_empty() && remaining_checks.is_empty(),
            attempts,
            exhausted_checks,
            remaining_checks,
        }
    }

    fn record_attempts(
        &mut self,
        repository: &str,
        pr: u64,
        head_sha: &str,
        failing_checks: &[CheckItem],
    ) -> Vec<CiRetryAttemptView> {
        let now = Utc::now();
        failing_checks
            .iter()
            .map(|check| {
                let key = retry_key(repository, pr, head_sha, &check.name);
                let entry = self
                    .attempts
                    .entry(key.clone())
                    .or_insert_with(|| CiRetryAttempt {
                        repository: repository.to_string(),
                        pr,
                        head_sha: head_sha.to_string(),
                        check_name: check.name.clone(),
                        attempts: 0,
                        first_attempted_at: now,
                        last_attempted_at: now,
                        exhausted_notification: None,
                    });
                entry.attempts += 1;
                entry.last_attempted_at = now;
                CiRetryAttemptView {
                    check_name: check.name.clone(),
                    attempts: entry.attempts,
                    max_attempts: 0,
                    key,
                }
            })
            .collect()
    }
}

fn retry_state_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| {
        TuicrError::Forge("HOME must be set to write tuicr CI retry state".to_string())
    })?;
    Ok(PathBuf::from(home).join(".local/state/tuicr/ci-retries.json"))
}

fn retry_key(repository: &str, pr: u64, head_sha: &str, check_name: &str) -> String {
    format!("{}#{}#{}#{}", repository, pr, head_sha, check_name.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ci::CheckState;

    fn failing_check(name: &str) -> CheckItem {
        CheckItem {
            name: name.to_string(),
            source_type: "CheckRun".to_string(),
            state: CheckState::Failing,
            raw_status: None,
            raw_conclusion: Some("FAILURE".to_string()),
            url: None,
            started_at: None,
            completed_at: None,
            workflow_name: None,
            needs_repair: true,
            details: None,
            log_references: Vec::new(),
        }
    }

    #[test]
    fn should_allow_checks_before_retry_limit() {
        let store = CiRetryStore::default();
        let decision = store.decision("squareup/java", 123, "abc", &[failing_check("build")], 2);
        assert!(!decision.exhausted);
        assert_eq!(decision.remaining_checks.len(), 1);
        assert_eq!(decision.attempts[0].attempts, 0);
    }

    #[test]
    fn should_exhaust_when_all_failing_checks_reach_limit() {
        let mut store = CiRetryStore::default();
        store.record_attempts("squareup/java", 123, "abc", &[failing_check("build")]);
        store.record_attempts("squareup/java", 123, "abc", &[failing_check("build")]);

        let decision = store.decision("squareup/java", 123, "abc", &[failing_check("build")], 2);
        assert!(decision.exhausted);
        assert_eq!(decision.exhausted_checks[0].attempts, 2);
    }

    #[test]
    fn should_scope_retries_by_head_sha() {
        let mut store = CiRetryStore::default();
        store.record_attempts("squareup/java", 123, "old", &[failing_check("build")]);
        store.record_attempts("squareup/java", 123, "old", &[failing_check("build")]);

        let decision = store.decision("squareup/java", 123, "new", &[failing_check("build")], 2);
        assert!(!decision.exhausted);
        assert_eq!(decision.attempts[0].attempts, 0);
    }

    #[test]
    fn should_track_exhausted_notification_once() {
        let mut store = CiRetryStore::default();
        store.record_attempts("squareup/java", 123, "abc", &[failing_check("build")]);
        store.record_attempts("squareup/java", 123, "abc", &[failing_check("build")]);
        let decision = store.decision("squareup/java", 123, "abc", &[failing_check("build")], 2);
        assert!(decision.exhausted);
        assert!(
            store.attempts[&decision.exhausted_checks[0].key]
                .exhausted_notification
                .is_none()
        );

        let notification = NotificationAttempt {
            delivered_at: Utc::now(),
            sink: "disabled".to_string(),
            success: true,
            message: "disabled".to_string(),
        };
        store
            .attempts
            .get_mut(&decision.exhausted_checks[0].key)
            .unwrap()
            .exhausted_notification = Some(notification);
        assert!(
            store.attempts[&decision.exhausted_checks[0].key]
                .exhausted_notification
                .is_some()
        );
    }
}
