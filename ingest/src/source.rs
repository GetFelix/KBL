//! The adapter interface. The tracker, the tools and the dashboard never see
//! which system an item came from: an adapter lists what changed since a
//! cursor, fetches one issue, maps its links, and writes back.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// An issue in an upstream tracker: `GetFelix/felix#812` on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct IssueRef {
    pub(crate) repo: String,
    pub(crate) number: u64,
}

impl IssueRef {
    pub(crate) fn new(repo: &str, number: u64) -> Self {
        Self {
            repo: repo.to_owned(),
            number,
        }
    }

    /// `owner/repo#number`, exactly: one slash, names of letters, digits,
    /// `.`, `_` and `-`, and a number without sign or leading zero.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        let (repo, number) = s.split_once('#')?;
        let (owner, name) = repo.split_once('/')?;
        let part = |p: &str| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        };
        let plain = !number.starts_with('0') && number.bytes().all(|b| b.is_ascii_digit());
        (part(owner) && part(name) && plain).then_some(())?;
        Some(Self::new(repo, number.parse().ok()?))
    }
}

impl fmt::Display for IssueRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

/// How an issue was closed upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Closed {
    /// Closed as completed or fixed: the work is done.
    Completed,
    /// Closed as not planned, a duplicate, won't fix, or for a reason this
    /// version does not know: the work is dropped.
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    pub(crate) id: IssueRef,
    pub(crate) title: String,
    pub(crate) body: String,
    /// None while open.
    pub(crate) closed: Option<Closed>,
    pub(crate) labels: Vec<String>,
    pub(crate) milestone: Option<String>,
    pub(crate) assignees: Vec<String>,
    /// The issue type (Bug, Feature), where the system has one.
    pub(crate) kind: Option<String>,
    pub(crate) author: String,
    pub(crate) url: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Comment {
    pub(crate) id: u64,
    pub(crate) issue: IssueRef,
    pub(crate) author: String,
    pub(crate) body: String,
    pub(crate) updated_at: String,
}

/// Where polling resumes. Opaque to the engine except that it is saved with
/// the rest of ingest's state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cursor {
    /// Issues updated at or after this are listed again. Re-listing is safe:
    /// every step the engine takes from a listing is idempotent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) issues_since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) comments_since: Option<String>,
    /// Validators of the last answer per request, so an unchanged listing
    /// costs no rate limit.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) etags: BTreeMap<String, String>,
}

/// What changed since a cursor, and the cursor to resume from.
#[derive(Debug, Default)]
pub(crate) struct Changes {
    /// Oldest change first.
    pub(crate) issues: Vec<Issue>,
    pub(crate) comments: Vec<Comment>,
    pub(crate) cursor: Cursor,
}

/// What a fetch of one issue found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fetched {
    Issue(Box<Issue>),
    /// Transferred: the same issue under a new id.
    Moved(IssueRef),
    /// Deleted, or no longer visible to the credential.
    Gone,
}

/// The issues an issue is linked to.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Links {
    /// The issue waits on these.
    pub(crate) blocked_by: Vec<IssueRef>,
    /// The issue waits on its sub-issues.
    pub(crate) sub_issues: Vec<IssueRef>,
}

/// A change to make upstream. Always through the ingest worker, which holds the
/// credentials; agents never do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteBack {
    /// The issue was dropped upstream while an agent's pull request is open:
    /// make the pull request a draft and say why on it.
    DraftPullRequest { pr: IssueRef, note: String },
}

#[derive(Debug)]
pub(crate) enum SourceError {
    /// The system asked us to wait.
    RateLimited {
        wait: Duration,
    },
    Failed(anyhow::Error),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::RateLimited { wait } => {
                write!(f, "rate limited; retry in {}s", wait.as_secs())
            }
            SourceError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for SourceError {}

impl From<anyhow::Error> for SourceError {
    fn from(e: anyhow::Error) -> Self {
        SourceError::Failed(e)
    }
}

pub(crate) trait Source: Send + Sync {
    /// The `source` field of the records, such as `github`.
    fn name(&self) -> &'static str;

    /// Logins the worker's own credential writes as. Their comments are not
    /// imported back.
    fn own_logins(&self) -> Vec<String>;

    /// Issues and comments changed since `cursor` in `repos`.
    fn changed(
        &self,
        repos: &[String],
        cursor: &Cursor,
    ) -> impl Future<Output = Result<Changes, SourceError>> + Send;

    fn fetch(&self, id: &IssueRef) -> impl Future<Output = Result<Fetched, SourceError>> + Send;

    fn links(&self, id: &IssueRef) -> impl Future<Output = Result<Links, SourceError>> + Send;

    fn write_back(&self, what: &WriteBack) -> impl Future<Output = Result<(), SourceError>> + Send;
}
