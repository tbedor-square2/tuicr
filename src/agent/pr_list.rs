use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::agent::feedback::map_gh_error;
use crate::error::Result;
use crate::forge::github::gh::{GhCommandRunner, SystemGhRunner};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrListOptions {
    pub owners: Vec<String>,
    pub repositories: Vec<String>,
    pub author: Option<String>,
    pub limit: usize,
    pub draft_filter: Option<bool>,
    pub review_filter: Option<String>,
    pub repository_include: Vec<String>,
    pub repository_exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrListReport {
    pub author: String,
    pub owners: Vec<String>,
    pub pull_requests: Vec<PrListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrListItem {
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub updated_at: Option<DateTime<Utc>>,
}

pub fn list_prs(options: PrListOptions) -> Result<PrListReport> {
    list_prs_with_runner(options, &SystemGhRunner)
}

fn list_prs_with_runner<R: GhCommandRunner>(
    options: PrListOptions,
    runner: &R,
) -> Result<PrListReport> {
    let author = match options.author.as_deref() {
        Some("@me") | None => fetch_viewer_login(runner)?,
        Some(author) => author.to_string(),
    };
    let owners = if options.owners.is_empty() {
        vec!["squareup".to_string()]
    } else {
        options.owners
    };
    let mut deduped = BTreeMap::new();
    for owner in &owners {
        for item in fetch_owner_prs(
            runner,
            owner,
            &author,
            options.limit,
            options.review_filter.as_deref(),
        )? {
            if !options.repositories.is_empty()
                && !options
                    .repositories
                    .iter()
                    .any(|repository| repository == &item.repository)
            {
                continue;
            }
            if let Some(draft) = options.draft_filter
                && item.is_draft != draft
            {
                continue;
            }
            if !repo_allowed(
                &item.repository,
                &options.repository_include,
                &options.repository_exclude,
            ) {
                continue;
            }
            deduped.insert(format!("{}#{}", item.repository, item.number), item);
        }
    }
    Ok(PrListReport {
        author,
        owners,
        pull_requests: deduped.into_values().collect(),
    })
}

fn repo_allowed(repository: &str, includes: &[String], excludes: &[String]) -> bool {
    if !includes.is_empty()
        && !includes
            .iter()
            .any(|pattern| repo_matches_filter(repository, pattern))
    {
        return false;
    }
    !excludes
        .iter()
        .any(|pattern| repo_matches_filter(repository, pattern))
}

fn repo_matches_filter(repository: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    if let Some(owner) = pattern.strip_suffix("/*") {
        return repository
            .split_once('/')
            .is_some_and(|(repo_owner, _)| repo_owner == owner);
    }
    repository == pattern
}

fn fetch_owner_prs<R: GhCommandRunner>(
    runner: &R,
    owner: &str,
    author: &str,
    limit: usize,
    review_filter: Option<&str>,
) -> Result<Vec<PrListItem>> {
    let mut args = vec![
        "search".to_string(),
        "prs".to_string(),
        "--author".to_string(),
        author.to_string(),
        "--state".to_string(),
        "open".to_string(),
        "--owner".to_string(),
        owner.to_string(),
        "--limit".to_string(),
        limit.max(1).to_string(),
        "--json".to_string(),
        "repository,number,title,url,state,isDraft,updatedAt".to_string(),
    ];
    if let Some(review_filter) = review_filter {
        args.push("--review".to_string());
        args.push(review_filter.to_string());
    }
    let output = runner
        .run(&args)
        .map_err(|err| map_gh_error(err, "github.com"))?;
    let rows: Vec<RawPrListItem> = serde_json::from_str(&output)?;
    Ok(rows.into_iter().map(PrListItem::from).collect())
}

fn fetch_viewer_login<R: GhCommandRunner>(runner: &R) -> Result<String> {
    let args = vec![
        "api".to_string(),
        "user".to_string(),
        "--jq".to_string(),
        ".login".to_string(),
    ];
    let output = runner
        .run(&args)
        .map_err(|err| map_gh_error(err, "github.com"))?;
    Ok(output.trim().to_string())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPrListItem {
    repository: RawRepository,
    number: u64,
    title: String,
    url: String,
    state: String,
    is_draft: bool,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepository {
    name_with_owner: String,
}

impl From<RawPrListItem> for PrListItem {
    fn from(raw: RawPrListItem) -> Self {
        Self {
            repository: raw.repository.name_with_owner,
            number: raw.number,
            title: raw.title,
            url: raw.url,
            state: raw.state,
            is_draft: raw.is_draft,
            updated_at: raw.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_pr_list_rows() {
        let rows: Vec<RawPrListItem> = serde_json::from_str(
            r#"[
              {
                "repository": { "nameWithOwner": "squareup/java" },
                "number": 480718,
                "title": "Add operators",
                "url": "https://github.com/squareup/java/pull/480718",
                "state": "open",
                "isDraft": true,
                "updatedAt": "2026-06-11T20:15:43Z"
              }
            ]"#,
        )
        .unwrap();
        let item = PrListItem::from(rows.into_iter().next().unwrap());
        assert_eq!(item.repository, "squareup/java");
        assert_eq!(item.number, 480718);
        assert_eq!(item.state, "open");
        assert!(item.is_draft);
    }

    #[test]
    fn should_filter_repositories_by_exact_or_owner_wildcard() {
        assert!(repo_allowed(
            "squareup/java",
            &["squareup/*".to_string()],
            &[]
        ));
        assert!(repo_allowed(
            "squareup/java",
            &["squareup/java".to_string()],
            &[]
        ));
        assert!(!repo_allowed(
            "squareup/java",
            &["block/*".to_string()],
            &[]
        ));
        assert!(!repo_allowed(
            "squareup/java",
            &[],
            &["squareup/java".to_string()]
        ));
    }
}
