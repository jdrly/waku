//! GitHub access through the `gh` CLI for the pull-requests panel.
//!
//! The daemon owns these spawns so the GPUI app never touches a frame with a
//! subprocess; handlers run on the daemon's request threads where blocking is
//! the established pattern (see `usage.rs`). Auth is entirely `gh`'s business:
//! we never read or store tokens, we only report what `gh auth status` says.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, anyhow};
use serde::Deserialize;
use waku_protocol::github::{
    GithubAuth, GithubAuthStatus, GithubPullRequestComment, GithubPullRequestDetail,
    GithubPullRequestSummary,
};

use crate::command_env;

const PR_LIST_FIELDS: &str =
    "number,title,state,author,isDraft,headRefName,baseRefName,updatedAt,additions,deletions,url";
const PR_DETAIL_FIELDS: &str = "number,title,state,author,body,isDraft,headRefName,baseRefName,updatedAt,additions,deletions,url,changedFiles,comments";

/// `gh` nests the author under `{login}`; the protocol flattens it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAuthor {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    author: RawAuthor,
    body: String,
    created_at: String,
}

/// One `gh pr list/view --json` row. List output omits the detail-only
/// fields; serde defaults cover the difference so both shapes decode here.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPullRequest {
    number: u64,
    title: String,
    state: String,
    author: RawAuthor,
    #[serde(default)]
    is_draft: bool,
    #[serde(rename = "headRefName")]
    head_ref: String,
    #[serde(rename = "baseRefName")]
    base_ref: String,
    updated_at: String,
    #[serde(default)]
    additions: i64,
    #[serde(default)]
    deletions: i64,
    url: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    changed_files: u64,
    #[serde(default)]
    comments: Vec<RawComment>,
}

impl RawPullRequest {
    fn into_summary(self) -> GithubPullRequestSummary {
        GithubPullRequestSummary {
            number: self.number,
            title: self.title,
            state: self.state,
            author: self.author.login,
            is_draft: self.is_draft,
            head_ref: self.head_ref,
            base_ref: self.base_ref,
            updated_at: self.updated_at,
            additions: self.additions,
            deletions: self.deletions,
            url: self.url,
        }
    }

    fn into_detail(self) -> GithubPullRequestDetail {
        let comment_count = self.comments.len() as u64;
        let comments = self
            .comments
            .into_iter()
            .map(|comment| GithubPullRequestComment {
                author: comment.author.login,
                body: comment.body,
                created_at: comment.created_at,
            })
            .collect();
        GithubPullRequestDetail {
            number: self.number,
            title: self.title,
            state: self.state,
            author: self.author.login,
            is_draft: self.is_draft,
            head_ref: self.head_ref,
            base_ref: self.base_ref,
            updated_at: self.updated_at,
            additions: self.additions,
            deletions: self.deletions,
            url: self.url,
            body: self.body,
            changed_files: self.changed_files,
            comment_count,
            comments,
        }
    }
}

/// Build a `gh` invocation the same way every other provider spawn does, so
/// the CLI is found even when the daemon was launched without a login shell's
/// PATH.
fn gh() -> Command {
    command_env::command("gh")
}

fn gh_in(cwd: &Path) -> Command {
    let mut command = gh();
    command.current_dir(cwd);
    command
}

/// Run `gh auth status --json hosts` and reduce it to the protocol state.
/// Missing binary → `unavailable`; installed but signed out → `unauthenticated`.
pub fn probe_auth() -> GithubAuth {
    let Ok(output) = gh().args(["auth", "status", "--json", "hosts"]).output() else {
        return GithubAuth {
            status: GithubAuthStatus::Unavailable,
            login: None,
        };
    };
    if !output.status.success() {
        return GithubAuth {
            status: GithubAuthStatus::Unauthenticated,
            login: None,
        };
    }
    match serde_json::from_slice::<RawAuthStatus>(&output.stdout) {
        Ok(status) => {
            let login = status
                .active_login("github.com")
                .or_else(|| status.any_authenticated_login());
            let status = if login.is_some() {
                GithubAuthStatus::Available
            } else {
                GithubAuthStatus::Unauthenticated
            };
            GithubAuth { status, login }
        }
        // Signed in on an unexpected gh version: stay optimistic rather than
        // blocking the panel behind a schema we did not anticipate.
        Err(_) => GithubAuth {
            status: GithubAuthStatus::Available,
            login: None,
        },
    }
}

#[derive(Deserialize)]
struct RawAuthStatus {
    #[serde(default)]
    hosts: serde_json::Map<String, serde_json::Value>,
}

impl RawAuthStatus {
    fn accounts(&self, host: &str) -> Vec<RawAuthAccount> {
        self.hosts
            .get(host)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    fn active_login(&self, host: &str) -> Option<String> {
        self.accounts(host)
            .into_iter()
            .find(|account| account.active && account.state == "success")
            .map(|account| account.login)
    }

    fn any_authenticated_login(&self) -> Option<String> {
        self.accounts("github.com")
            .into_iter()
            .find(|account| account.state == "success")
            .map(|account| account.login)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAuthAccount {
    login: String,
    /// `success` when signed in and usable.
    #[serde(default)]
    state: String,
    #[serde(default)]
    active: bool,
}

/// Open pull requests for the repository at `cwd`, newest activity first.
pub fn list_pull_requests(cwd: &Path) -> anyhow::Result<Vec<GithubPullRequestSummary>> {
    let output = gh_in(cwd)
        .args(["pr", "list", "--json", PR_LIST_FIELDS, "--limit", "100"])
        .output()
        .context("could not run the gh CLI")?;
    let stdout = std::str::from_utf8(&output.stdout)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "gh pr list failed: {}",
            first_line(&stderr).unwrap_or("unknown gh error")
        ));
    }
    parse_pull_requests(stdout)
}

/// Full detail for one pull request, including its conversation.
pub fn pull_request_detail(cwd: &Path, number: u64) -> anyhow::Result<GithubPullRequestDetail> {
    let output = gh_in(cwd)
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--json",
            PR_DETAIL_FIELDS,
        ])
        .output()
        .context("could not run the gh CLI")?;
    let stdout = std::str::from_utf8(&output.stdout)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "gh pr view failed: {}",
            first_line(&stderr).unwrap_or("unknown gh error")
        ));
    }
    parse_pull_request_detail(stdout)
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().find(|line| !line.trim().is_empty())
}

fn parse_pull_requests(stdout: &str) -> anyhow::Result<Vec<GithubPullRequestSummary>> {
    let rows: Vec<RawPullRequest> = serde_json::from_str(stdout)
        .map_err(|error| anyhow!("could not parse gh pr list output: {error}"))?;
    Ok(rows.into_iter().map(RawPullRequest::into_summary).collect())
}

fn parse_pull_request_detail(stdout: &str) -> anyhow::Result<GithubPullRequestDetail> {
    let row: RawPullRequest = serde_json::from_str(stdout)
        .map_err(|error| anyhow!("could not parse gh pr view output: {error}"))?;
    Ok(row.into_detail())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_list_rows() {
        let output = r#"[
            {
                "number": 192,
                "title": "fix(app): buffer live thinking text by default",
                "state": "OPEN",
                "author": {"id": "MDQ6", "is_bot": false, "login": "jdrly", "name": "Jan"},
                "isDraft": false,
                "headRefName": "fix/thinking-text-parsing",
                "baseRefName": "main",
                "updatedAt": "2026-08-27T18:12:00Z",
                "additions": 75,
                "deletions": 1,
                "url": "https://github.com/egoist/waku/pull/192"
            },
            {
                "number": 190,
                "title": "Draft: migrate telemetry",
                "state": "OPEN",
                "author": {"login": "someone"},
                "isDraft": true,
                "headRefName": "wip",
                "baseRefName": "main",
                "updatedAt": "2026-08-26T10:00:00Z",
                "additions": 10,
                "deletions": 40,
                "url": "https://github.com/egoist/waku/pull/190"
            }
        ]"#;
        let rows = parse_pull_requests(output).expect("list parses");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].number, 192);
        assert_eq!(rows[0].author, "jdrly");
        assert_eq!(rows[0].state, "OPEN");
        assert!(!rows[0].is_draft);
        assert_eq!(rows[0].head_ref, "fix/thinking-text-parsing");
        assert_eq!(rows[1].is_draft, true);
        assert_eq!(rows[1].additions, 10);
    }

    #[test]
    fn parses_pr_detail_with_comments() {
        let output = r#"{
            "number": 192,
            "title": "fix(app): buffer live thinking text by default",
            "state": "OPEN",
            "author": {"login": "jdrly"},
            "body": "Buffered delivery, T3 parity.",
            "isDraft": false,
            "headRefName": "fix/thinking-text-parsing",
            "baseRefName": "main",
            "updatedAt": "2026-08-27T18:12:00Z",
            "additions": 75,
            "deletions": 1,
            "url": "https://github.com/egoist/waku/pull/192",
            "changedFiles": 6,
            "comments": [
                {"author": {"login": "egoist"}, "body": "Looks good", "createdAt": "2026-08-27T19:00:00Z"},
                {"author": {"login": "reviewer"}, "body": "One nit", "createdAt": "2026-08-27T18:30:00Z"}
            ]
        }"#;
        let detail = parse_pull_request_detail(output).expect("detail parses");
        assert_eq!(detail.number, 192);
        assert_eq!(detail.body, "Buffered delivery, T3 parity.");
        assert_eq!(detail.changed_files, 6);
        assert_eq!(detail.comment_count, 2);
        assert_eq!(detail.comments[0].author, "egoist");
    }

    #[test]
    fn list_output_missing_detail_fields_still_decodes_into_detail_shape() {
        // A list row (no body/changedFiles/comments) must decode through the
        // same raw shape — serde defaults cover the difference.
        let output = r#"{"number": 1, "title": "t", "state": "OPEN", "author": {"login": "a"},
            "headRefName": "h", "baseRefName": "b", "updatedAt": "2026-01-01T00:00:00Z",
            "additions": 1, "deletions": 0, "url": "u"}"#;
        let detail = parse_pull_request_detail(output).expect("decodes");
        assert_eq!(detail.body, "");
        assert_eq!(detail.changed_files, 0);
        assert!(detail.comments.is_empty());
    }

    #[test]
    fn gh_errors_surface_the_first_stderr_line() {
        assert_eq!(
            first_line("\ngh: Not Found (HTTP 404)\n"),
            Some("gh: Not Found (HTTP 404)")
        );
        assert_eq!(first_line("   \n\t\n"), None);
    }
}
