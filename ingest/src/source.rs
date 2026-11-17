//! The adapter interface. The tracker, the tools and the dashboard never see
//! which system an item came from: an adapter lists what changed since a
//! cursor, fetches one issue, maps its links, and writes back.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use kbl_core::work::PrSnapshot;
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
    /// The upstream priority name, where the system has one (Jira).
    pub(crate) priority: Option<String>,
    /// Jira components; empty elsewhere.
    pub(crate) components: Vec<String>,
    /// Names of the rules whose upstream query (a JQL filter) selects this
    /// issue. Empty for sources that filter locally.
    pub(crate) scopes: Vec<String>,
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

/// What changed since the cursors, and the cursor each repo resumes from.
#[derive(Debug, Default)]
pub(crate) struct Changes {
    /// Oldest change first.
    pub(crate) issues: Vec<Issue>,
    pub(crate) comments: Vec<Comment>,
    /// A cursor per repo read this poll. A repo that was not read keeps the
    /// one it had, so it is absent here.
    pub(crate) cursors: BTreeMap<String, Cursor>,
    /// Repos that answered 403 or 404, with the status. Their cursors did not move.
    pub(crate) denied: BTreeMap<String, u16>,
}

/// A pull request as upstream describes it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PullRequest {
    pub(crate) author: String,
    /// The head branch and the description, which tie it to an issue.
    pub(crate) head_ref: String,
    pub(crate) body: String,
    /// When it was created, in milliseconds since the epoch.
    pub(crate) created_ms: Option<u64>,
    /// None when the author is not one of ours: nothing more is read.
    pub(crate) status: Option<PrSnapshot>,
    /// The commit a counted merge made, and when, in milliseconds since the epoch.
    pub(crate) merge: Option<(String, u64)>,
}

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) tag: String,
    pub(crate) published_ms: u64,
}

/// One listing of a repo's releases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Releases {
    /// Stable releases published at or after the time asked for, oldest first.
    pub(crate) list: Vec<Release>,
    /// The listing ran out before the page cap.
    pub(crate) whole: bool,
    /// The earliest publish time on any page read, drafts and prereleases included.
    pub(crate) oldest_ms: Option<u64>,
}

impl Releases {
    /// Whether every release after a merge at `merged_ms` is in `list`: the
    /// listing ran out, or it reached a release published before the merge.
    pub(crate) fn covers(&self, merged_ms: u64) -> bool {
        self.whole || self.oldest_ms.is_some_and(|at| at < merged_ms)
    }
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
    /// Both lists were read in full. Only then may a link missing from them
    /// be removed: a failed, cut short or redirected read says nothing.
    pub(crate) complete: bool,
}

/// A change to make upstream. Always through the ingest worker, which holds the
/// credentials; agents never do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteBack {
    /// The issue was dropped upstream while an agent's pull request is open:
    /// make the pull request a draft and say why on it.
    DraftPullRequest {
        pr: IssueRef,
        note: String,
        /// See [`mark`].
        mark: String,
    },
    /// Say something on the upstream issue itself (Jira).
    Comment {
        issue: IssueRef,
        body: String,
        /// See [`mark`].
        mark: String,
    },
    /// Link the agent's pull request (`owner/repo#n`) from the issue.
    LinkPullRequest { issue: IssueRef, pr: IssueRef },
    /// Move the issue through the workflow transition of this name, if the
    /// workflow offers it now.
    Transition { issue: IssueRef, name: String },
}

/// The hidden tag on a comment KBL writes about `item`. A post cut short has
/// an unknown outcome, so the retry looks for this before posting again.
pub(crate) fn mark(item: &str, kind: &str, key: &str) -> String {
    format!("<!-- kbl:{item}:{kind}:{key} -->")
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

    /// Whether pull requests can be read through this source. Jira holds no
    /// pull requests, so its worker skips that step; the worker of a source
    /// that does hold them records their state for every source's roots.
    fn reads_pull_requests(&self) -> bool {
        true
    }

    /// Logins the worker's own credential writes as. Their comments are not
    /// imported back.
    fn own_logins(&self) -> Vec<String>;

    /// Issues and comments changed in `repos` since each repo's own cursor.
    fn changed(
        &self,
        repos: &[String],
        cursors: &BTreeMap<String, Cursor>,
    ) -> impl Future<Output = Result<Changes, SourceError>> + Send;

    /// The pull request's phase, reviews and checks, or none if it is gone.
    fn pull_request(
        &self,
        id: &IssueRef,
    ) -> impl Future<Output = Result<Option<PullRequest>, SourceError>> + Send;

    fn fetch(&self, id: &IssueRef) -> impl Future<Output = Result<Fetched, SourceError>> + Send;

    /// Every comment on one issue, oldest first.
    fn issue_comments(
        &self,
        id: &IssueRef,
    ) -> impl Future<Output = Result<Vec<Comment>, SourceError>> + Send;

    fn links(&self, id: &IssueRef) -> impl Future<Output = Result<Links, SourceError>> + Send;

    fn write_back(&self, what: &WriteBack) -> impl Future<Output = Result<(), SourceError>> + Send;

    /// The repo's stable releases published at or after `since_ms`, oldest
    /// first, so the first that contains a merge is the oldest after it.
    /// Drafts and prereleases are left out. At most 5 pages of 100 are read,
    /// newest first. A merge the listing does not cover (see
    /// [`Releases::covers`]) gets no release at all, never one that may be
    /// later than the oldest that carries it.
    fn releases(
        &self,
        repo: &str,
        since_ms: u64,
    ) -> impl Future<Output = Result<Releases, SourceError>> + Send;

    /// Whether the release tagged `tag` contains the commit `sha`.
    fn contains(
        &self,
        repo: &str,
        tag: &str,
        sha: &str,
    ) -> impl Future<Output = Result<bool, SourceError>> + Send;
    /// Opens an issue in `repo` as our own credential and says which it is.
    fn open_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
    ) -> impl Future<Output = Result<IssueRef, SourceError>> + Send;
}
