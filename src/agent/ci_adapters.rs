use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckLogReference {
    pub adapter: String,
    pub url: String,
    pub hint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitHubActionsJobReference {
    pub run_id: u64,
    pub job_id: u64,
}

pub fn detect_log_references(
    check_name: &str,
    check_url: Option<&str>,
    workflow_name: Option<&str>,
) -> Vec<CheckLogReference> {
    let mut references = Vec::new();
    if let Some(url) = check_url.filter(|url| !url.trim().is_empty()) {
        if is_buildkite_url(url) || looks_like_buildkite(check_name, workflow_name) {
            references.push(CheckLogReference {
                adapter: "buildkite".to_string(),
                url: url.to_string(),
                hint: "Inspect the Buildkite build/job log for the failing step before editing"
                    .to_string(),
            });
        } else if is_kochiku_url(url) || looks_like_kochiku(check_name, workflow_name) {
            references.push(CheckLogReference {
                adapter: "kochiku".to_string(),
                url: url.to_string(),
                hint: "Inspect the Kochiku build page and failing part logs before editing"
                    .to_string(),
            });
        } else if is_github_actions_url(url) {
            references.push(CheckLogReference {
                adapter: "github_actions".to_string(),
                url: url.to_string(),
                hint: "Inspect the GitHub Actions run log and annotations before editing"
                    .to_string(),
            });
        }
    }

    if references.is_empty() && looks_like_kochiku(check_name, workflow_name) {
        references.push(CheckLogReference {
            adapter: "kochiku".to_string(),
            url: String::new(),
            hint: "Find the Kochiku build for this PR/head SHA and inspect failing part logs before editing"
                .to_string(),
        });
    } else if references.is_empty() && looks_like_buildkite(check_name, workflow_name) {
        references.push(CheckLogReference {
            adapter: "buildkite".to_string(),
            url: String::new(),
            hint: "Find the Buildkite build for this PR/head SHA and inspect failing step logs before editing"
                .to_string(),
        });
    }

    references
}

pub fn github_actions_job_reference(url: &str) -> Option<GitHubActionsJobReference> {
    let mut parts = url.split('/');
    while let Some(part) = parts.next() {
        if part == "actions" && parts.next() == Some("runs") {
            let run_id = parts.next()?.parse::<u64>().ok()?;
            if parts.next() != Some("job") {
                return None;
            }
            let job_id = parts.next()?.parse::<u64>().ok()?;
            return Some(GitHubActionsJobReference { run_id, job_id });
        }
    }
    None
}

fn is_buildkite_url(url: &str) -> bool {
    let value = url.to_ascii_lowercase();
    value.contains("buildkite.com/")
}

fn is_kochiku_url(url: &str) -> bool {
    let value = url.to_ascii_lowercase();
    value.contains("kochiku")
}

fn is_github_actions_url(url: &str) -> bool {
    let value = url.to_ascii_lowercase();
    value.contains("github.com/") && value.contains("/actions/runs/")
}

fn looks_like_buildkite(check_name: &str, workflow_name: Option<&str>) -> bool {
    contains_ci_name(check_name, workflow_name, "buildkite")
}

fn looks_like_kochiku(check_name: &str, workflow_name: Option<&str>) -> bool {
    contains_ci_name(check_name, workflow_name, "kochiku")
}

fn contains_ci_name(check_name: &str, workflow_name: Option<&str>, needle: &str) -> bool {
    check_name.to_ascii_lowercase().contains(needle)
        || workflow_name
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_detect_buildkite_urls() {
        let references = detect_log_references(
            "pipeline",
            Some("https://buildkite.com/square/java/builds/1"),
            None,
        );
        assert_eq!(references[0].adapter, "buildkite");
        assert!(references[0].hint.contains("Buildkite"));
    }

    #[test]
    fn should_detect_kochiku_by_check_name_without_url() {
        let references = detect_log_references("Kochiku", None, None);
        assert_eq!(references[0].adapter, "kochiku");
        assert!(references[0].url.is_empty());
    }

    #[test]
    fn should_parse_github_actions_job_url() {
        let reference = github_actions_job_reference(
            "https://github.com/squareup/example/actions/runs/123456/job/987654",
        )
        .unwrap();
        assert_eq!(reference.run_id, 123456);
        assert_eq!(reference.job_id, 987654);
    }

    #[test]
    fn should_reject_github_actions_run_url_without_job() {
        assert!(
            github_actions_job_reference("https://github.com/squareup/example/actions/runs/123456")
                .is_none()
        );
    }
}
