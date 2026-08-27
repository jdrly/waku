//! GitHub surface types shared by the daemon and the clients.
//!
//! The daemon resolves GitHub state through the `gh` CLI (which owns auth and
//! API access); these structs are the wire contract for what it reports.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// GitHub access state as reported by the `gh` CLI.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubAuth {
    /// `available` when a signed-in `gh` is usable, `unauthenticated` when
    /// `gh` exists but no account is signed in, `unavailable` when `gh`
    /// itself is missing.
    pub status: GithubAuthStatus,
    /// Login of the active authenticated account, when known.
    pub login: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum GithubAuthStatus {
    #[default]
    Available,
    Unauthenticated,
    Unavailable,
}

/// One pull request in the list view. Flattened from `gh pr list --json`
/// output; `author` is the nested author's login.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestSummary {
    pub number: u64,
    pub title: String,
    /// `open`, `closed`, or `merged` as reported by `gh`.
    pub state: String,
    pub author: String,
    pub is_draft: bool,
    pub head_ref: String,
    pub base_ref: String,
    /// ISO-8601 timestamp as reported by `gh`.
    pub updated_at: String,
    /// Unix seconds for `updated_at`, precomputed so clients never parse
    /// dates. `0` when the timestamp did not parse.
    pub updated_at_unix: i64,
    pub additions: i64,
    pub deletions: i64,
    pub url: String,
}

/// Full detail behind the panel's detail view. Everything the summary has
/// plus the markdown body, changed-file count, and the conversation comments
/// (already ordered as GitHub reports them, newest thread first).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestDetail {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub is_draft: bool,
    pub head_ref: String,
    pub base_ref: String,
    pub updated_at: String,
    pub updated_at_unix: i64,
    pub additions: i64,
    pub deletions: i64,
    pub url: String,
    /// Markdown body as written by the PR author.
    pub body: String,
    pub changed_files: u64,
    pub comment_count: u64,
    pub comments: Vec<GithubPullRequestComment>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullRequestComment {
    pub author: String,
    /// Markdown body.
    pub body: String,
    /// ISO-8601 timestamp.
    pub created_at: String,
}
