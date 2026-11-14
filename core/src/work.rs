//! What `work.events` holds, and the keys the tracker keeps beside it.
//!
//! The stream is the record: an item's state is a fold of it ([`crate::work_fold`]).
//! The keys are written by the tracker with `cache_put_if`, so a person and an
//! agent editing one item cannot overwrite each other. No record here is
//! written by a model call, and the fold makes none.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::codec::{Codec, DecodeError};
use crate::content::Content;
use crate::names::{InvalidName, ItemId, Key};

/// The one stream every change to work goes on.
pub const WORK_EVENTS: &str = "work.events";

/// The format every record had before records carried a version. A record
/// with no `v` field is this format. Frozen: a change an older reader would
/// fold differently gets a higher format instead. See `docs/upgrades.md`.
pub const FORMAT_V1: u32 = 1;

/// The format that added `returned_to_scope`.
pub const FORMAT_V2: u32 = 2;

/// The format that added a prerequisite posted from triage.
pub const FORMAT_V3: u32 = 3;

/// The highest record format this build reads and writes.
pub const FORMAT: u32 = FORMAT_V3;

/// The ready stream for a lane. Lanes `0..TIERS` are the priority tiers
/// `work_next` polls from `p0` up. The lanes after them are one per reviewer
/// tier, polled only by an agent that names that tier.
pub fn ready_stream(lane: u8) -> String {
    match lane.checked_sub(TIERS) {
        None => format!("work.ready.p{lane}"),
        Some(r) => format!("work.review.r{r}"),
    }
}

/// The consumer group on every ready stream. One group, so a claim on any tier
/// is a delivery to one agent, named as a member.
pub const READY_GROUP: &str = "agents";

/// How many ready tiers there are: `work.ready.p0` to `p3`.
pub const TIERS: u8 = 4;

/// How many reviewer tiers there are: `work.review.r0` to `r3`.
pub const REVIEW_LANES: u8 = 4;

/// Priority lanes and reviewer lanes together.
pub const LANES: u8 = TIERS + REVIEW_LANES;

/// What goes on a ready stream. `stamp` makes each publish distinct, so a
/// delivery can be told from an older record of the same item that a tier
/// change or a restart left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyRecord {
    pub item: ItemId,
    pub tier: u8,
    pub stamp: u64,
}

/// The head branch an agent opens an item's pull request from. Upstream sync
/// links a request to an item by this name (or a closing reference), so the
/// claim reply hands it to the agent.
pub fn pr_branch(item: &ItemId) -> String {
    format!("kbl/{item}")
}

/// The cache key of an item's fields.
pub fn item_key(item: &ItemId) -> Result<Key, InvalidName> {
    Key::new(format!("i-{item}"))
}

/// Longest rules text, in bytes. Rules are always inline.
pub const MAX_RULES_TEXT: usize = 8 * 1024;

/// Most checks one rules record may list.
pub const MAX_RULE_CHECKS: usize = 20;

/// Longest single check, in bytes.
pub const MAX_CHECK_BYTES: usize = 500;

/// Most distinct scopes (a repo, or a root in a repo) that may have rules.
pub const MAX_RULE_SCOPES: usize = 500;

/// The cache key of the edge "`from` waits on `to`".
pub fn edge_key(from: &ItemId, to: &ItemId) -> Result<Key, InvalidName> {
    Key::new(format!("e-{from}-{to}"))
}

/// The cache key of one context entry on an item.
pub fn context_key(item: &ItemId, entry: &Key) -> Result<Key, InvalidName> {
    Key::new(format!("c-{item}-{entry}"))
}

/// The cache key of a root's work budget. Compare-and-set on it enforces the
/// budget, since Felix has no bounded add.
pub fn budget_key(root: &ItemId) -> Result<Key, InvalidName> {
    Key::new(format!("b-{root}"))
}

/// The cache key that makes a child's creation idempotent: the parent plus the
/// agent's own key, written only if absent.
pub fn child_key(parent: &ItemId, key: &Key) -> Result<Key, InvalidName> {
    Key::new(format!("k-{parent}-{key}"))
}

/// The cache key that dedupes an upstream bug, written only if absent.
pub fn upstream_key(source: &str, id: &str) -> Result<Key, InvalidName> {
    Key::new(format!("u-{source}-{id}"))
}

/// Who a record is attributed to. The tracker sets it from the token that
/// made the call, never from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Agent,
    Person,
    Tracker,
}

/// What an item asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Fix,
    Triage,
    Augment,
    Split,
    Question,
    Test,
    /// Work that carries on from an answered question. Only the fold creates it.
    Resume,
    /// A first-pass or final review of an item's pull request. Only the
    /// tracker creates it, when the request opens.
    Review,
}

impl WorkKind {
    /// Kinds an agent may create live under an item it holds.
    pub fn is_live_child(self) -> bool {
        matches!(
            self,
            WorkKind::Split | WorkKind::Augment | WorkKind::Question | WorkKind::Test
        )
    }
}

/// Longest reviewer tier name, in bytes.
pub const MAX_REVIEW_TIER: usize = 32;

/// Most review items one root may have, across its pull requests.
pub const MAX_REVIEWS_PER_ROOT: usize = 8;

/// Most findings one review attempt may record.
pub const MAX_FINDINGS: u32 = 40;

/// Longest finding title, file path, scenario, verdict reason and tested
/// name, in bytes.
pub const MAX_FINDING_TITLE: usize = 200;
pub const MAX_FINDING_FILE: usize = 400;
pub const MAX_SCENARIO: usize = 4096;
pub const MAX_VERDICT_WHY: usize = 2048;
pub const MAX_REVERT_TEST: usize = 400;

/// Whether `tier` is a name a reviewer tier may have: lowercase letters,
/// digits and dashes. KBL never runs a model, so the tier only says which
/// kind of reviewer should claim the review.
pub fn valid_review_tier(tier: &str) -> bool {
    !tier.is_empty()
        && tier.len() <= MAX_REVIEW_TIER
        && tier
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Which review of a pull request an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPass {
    First,
    /// The last review. It ends with a [`MergeVerdict`].
    Final,
}

/// What a review item is: which pass, for which tier of reviewer, of which
/// pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSpec {
    pub pass: ReviewPass,
    pub tier: String,
    /// The `pr` of the root this review reads, so a review of an older
    /// request cannot speak for a newer one.
    pub pr: String,
}

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// What a finding leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingVerdict {
    /// Becomes a child of the pull request's root, for its author.
    Fix,
    /// Becomes a proposal a person accepts.
    Defer,
    /// Recorded and nothing more.
    Dismiss,
}

/// What a final review recommends. Merging stays a person's action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeVerdict {
    Merge,
    Block,
}

/// A revert check on a fix's decision: the test, and whether it failed with
/// the fix reverted and passed with it restored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertCheck {
    pub test: String,
    pub failed_without_fix: bool,
    pub passed_with_fix: bool,
}

impl RevertCheck {
    /// Whether the check shows the test guards the fix.
    pub fn proves(&self) -> bool {
        self.failed_without_fix && self.passed_with_fix
    }
}

/// Most options a question may offer.
pub const MAX_ASK_OPTIONS: usize = 8;

/// Longest option, default or chosen option, in bytes.
pub const MAX_ASK_OPTION_LEN: usize = 200;

/// Longest title a record carries, in bytes.
pub const MAX_TITLE: usize = 4096;

/// What triage could not settle by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unclear {
    /// What the item asks for is not clear.
    Scope,
    /// Which repository the work belongs in is not clear.
    Repo,
    /// What would count as done is not clear.
    Acceptance,
}

/// What an answer leads to. An answer without one only closes the question,
/// which is how every answer logged before this field existed folds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "then", rename_all = "snake_case")]
pub enum Then {
    /// Claimable work under the asking item, carrying the question, the answer
    /// and what the asker knew.
    Resume,
    /// The work belongs in another repo: a proposal in its Triage that the
    /// asking item waits on. Only a person may answer this way, and the
    /// answer's text is the reason shown with the proposal.
    Prerequisite { target: Target, title: String },
}

/// The part of a fix a decision belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Understand,
    Reproduce,
    Cause,
    Fix,
    Verify,
}

/// A step a decision rests on, by stream and offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub stream: String,
    pub offset: u64,
}

/// What a person added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanKind {
    Note,
    Answer,
    ReviewComment,
    PriorityChange,
}

/// Why an item was closed by upstream sync or a person.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseWhy {
    /// The fix landed upstream.
    Merged,
    /// Closed upstream as completed or fixed (GitHub `completed`, ADO resolved, Jira Done or Fixed).
    /// It counts as done, so dependents are freed.
    Completed,
    Dismissed,
    /// Closed upstream as not planned, a duplicate or won't fix, or for a reason this version does
    /// not know. It counts as dropped, so dependents go to Needs you.
    #[default]
    #[serde(other)]
    Upstream,
}

/// Where an item's work lands: a source and a repository in it, such as
/// `github` and `GetFelix/felix-gateway`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Target {
    pub source: String,
    pub repo: String,
}

/// What a dependent waits for before an edge stops holding it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoneWhen {
    /// The prerequisite is merged (or closed as completed).
    #[default]
    Merged,
    /// It is merged and a `shipped` record says which release carries it.
    Released,
}

/// The proposal a prerequisite creates in another repo's Triage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPrerequisite {
    pub target: Target,
    pub title: String,
    /// Why the held item cannot go on without it.
    pub why: Content,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// Why the tracker refused an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// Over the root's work budget. The root goes to Needs you.
    Budget,
    /// Over a per-attempt or per-agent cap.
    Limit,
    Invalid,
}

/// Where an agent's pull request stands upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrPhase {
    Open,
    Merged,
    /// Closed without a merge.
    Closed,
}

/// The reviews on a pull request, from people who may review it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrReview {
    Unreviewed,
    /// A reviewer was asked and has not answered.
    Requested,
    Approved,
    ChangesRequested,
}

/// The check results on a pull request's head commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrChecks {
    NoChecks,
    /// Some check has not finished, or there are more than were read.
    Pending,
    Green,
    Red,
}

/// A check that failed, as upstream names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCheck {
    pub name: String,
    pub url: String,
}

/// Longest pull request reference, check name, check URL, source name and
/// repository name kept in a record, in bytes.
pub const MAX_PR_REF: usize = 200;
pub const MAX_CHECK_NAME: usize = 200;
pub const MAX_CHECK_URL: usize = 400;

/// What upstream says about the pull request an item reports in `pr`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSnapshot {
    /// `owner/repo#number`, the `pr` it describes.
    pub pr: String,
    pub phase: PrPhase,
    pub review: PrReview,
    pub checks: PrChecks,
    /// Set exactly when `checks` is red.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing: Option<FailedCheck>,
}

impl PrSnapshot {
    /// Whether the text fields fit a record.
    pub fn fits(&self) -> bool {
        self.pr.len() <= MAX_PR_REF
            && self
                .failing
                .as_ref()
                .is_none_or(|f| f.name.len() <= MAX_CHECK_NAME && f.url.len() <= MAX_CHECK_URL)
    }

    /// Whether `failing` agrees with `checks`.
    pub fn consistent(&self) -> bool {
        self.failing.is_some() == (self.checks == PrChecks::Red)
    }
}

/// What a record says happened. `kind` is the variant name in snake case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkRecord {
    /// A root pulled from an upstream tracker. `(source, upstream_id)` is imported once.
    Imported {
        item: ItemId,
        source: String,
        upstream_id: String,
        title: String,
    },
    /// A new item. With a live kind, an agent needs to hold `parent`; an agent's
    /// `fix` is a proposal that a person accepts. `key` makes a retry a no-op.
    Proposed {
        item: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<ItemId>,
        key: Key,
        work: WorkKind,
        title: String,
        /// Set exactly on a review item, which only the tracker creates.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review: Option<ReviewSpec>,
    },
    /// The answer to an item's triage work: a proposal for a person.
    Triaged {
        item: ItemId,
        tier: u8,
        why: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duplicate_of: Option<ItemId>,
        /// Triage could not settle the item. It goes to a person as a question.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unclear: Option<Unclear>,
        /// Items this one should wait on, as advice: a person links them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        depends_on: Vec<ItemId>,
        /// Titles this one should be split into, as advice: nothing is created.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        split: Vec<String>,
    },
    /// The holder cannot go on until `prerequisite` is done. With `new`, the
    /// record also creates that prerequisite as a proposal in the target
    /// repo's Triage; without it, `prerequisite` is an existing item. Either
    /// way `item` then waits on it and the claim ends without costing a try.
    ///
    /// With `triage`, the writer holds that triage item instead: `item` is
    /// the item under triage, `attempt` is the triage item's, and the claim
    /// goes on.
    Prerequisite {
        item: ItemId,
        attempt: u32,
        prerequisite: ItemId,
        /// Makes a retry a no-op, like a proposal's key.
        key: Key,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new: Option<NewPrerequisite>,
        #[serde(default)]
        done_when: DoneWhen,
        /// What the attempt cost so far, charged like a release.
        #[serde(default)]
        cost_micros: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        triage: Option<ItemId>,
    },
    /// A person or upstream sync says a release carries a finished item, which
    /// frees dependents whose edge waits for a release.
    Shipped { item: ItemId, release: String },
    /// A person (or the tracker for them) accepts the item into the queue.
    Accepted {
        item: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier: Option<u8>,
    },
    /// `item` waits on `to`.
    Linked { item: ItemId, to: ItemId },
    /// A person removes the edge, for example after a dependency was dropped.
    Unlinked { item: ItemId, to: ItemId },
    /// A context entry; the same `entry` again replaces it.
    Context {
        item: ItemId,
        entry: Key,
        text: Content,
    },
    Claimed {
        item: ItemId,
        attempt: u32,
        deadline_ms: u64,
    },
    Progress {
        item: ItemId,
        attempt: u32,
        note: Content,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr: Option<String>,
    },
    /// `cost_micros` is what the agent reports and the tracker cannot verify.
    Done {
        item: ItemId,
        attempt: u32,
        #[serde(default)]
        cost_micros: u64,
    },
    Released {
        item: ItemId,
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        cost_micros: u64,
    },
    GaveUp {
        item: ItemId,
        attempt: u32,
        reason: String,
        #[serde(default)]
        cost_micros: u64,
    },
    /// The holder asks a person; the item waits in Needs you.
    Question {
        item: ItemId,
        attempt: u32,
        text: Content,
        /// Choices to pick from. A person may always answer with free text instead.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        /// The option taken when a person answers without choosing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
        /// What the attempt cost so far, charged when the question is asked: the
        /// answer ends the hold, so a later release cannot report it.
        #[serde(default, skip_serializing_if = "is_zero")]
        cost_micros: u64,
    },
    /// `question` is the offset of the `question` record it answers. An agent
    /// may answer only while it holds a `question` work item under `item`.
    Answer {
        item: ItemId,
        question: u64,
        text: Content,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<Evidence>,
        /// The option picked, one of the question's. Free text goes in `text` and is never read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chosen: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<Then>,
    },
    Decision {
        item: ItemId,
        attempt: u32,
        phase: Phase,
        so_far: String,
        question: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        chosen: String,
        why: String,
        commit: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<Evidence>,
        /// Whether a test was shown to fail without this fix and pass with it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revert_check: Option<RevertCheck>,
    },
    /// One finding of a review, written by the agent holding the review item.
    /// A `fix` becomes a child of the root, a `defer` a proposal.
    Finding {
        item: ItemId,
        attempt: u32,
        severity: Severity,
        file: String,
        line: u32,
        title: String,
        scenario: String,
        verdict: FindingVerdict,
    },
    /// How a final review ends. A recommendation: a person merges.
    ReviewVerdict {
        item: ItemId,
        attempt: u32,
        verdict: MergeVerdict,
        why: String,
    },
    Step {
        item: ItemId,
        attempt: u32,
        command: String,
        exit_code: i32,
        output: Content,
    },
    /// `decision` is the offset of the decision this edit carries out.
    Edit {
        item: ItemId,
        attempt: u32,
        file: String,
        hunk: Content,
        decision: u64,
    },
    /// `attempt` names the path it steers; none means the item as a whole.
    HumanInput {
        item: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<u32>,
        input: HumanKind,
        text: Content,
    },
    /// A redo: a new attempt starts from the decision at `at` on `from_attempt`.
    /// `note` is the offset of the `human_input` that steers it; it may have
    /// been written after the decision.
    ForkedFrom {
        item: ItemId,
        from_attempt: u32,
        at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<u64>,
    },
    /// A person or upstream sync closes the item and cancels its open descendants.
    Closed {
        item: ItemId,
        #[serde(default)]
        why: CloseWhy,
    },
    /// A person rejects a proposal that was never accepted.
    Rejected { item: ItemId },
    /// The upstream issue's title, body or labels changed. The item shows
    /// "changed upstream since triage" until its next `triaged`; no new work
    /// follows. `title` is the new title when it changed.
    Updated {
        item: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// The upstream issue no longer matches the rule that admitted it. A
    /// holder finishes or releases, then the item stops in Needs you.
    LeftScope { item: ItemId },
    /// The upstream issue matches its rule again, or a person says the item
    /// still belongs: `left_scope` is cleared and the item goes back to its
    /// stage. Format 2.
    ReturnedToScope { item: ItemId },
    /// The upstream issue was reopened: the same root goes back to Triage with
    /// its history and its remaining budget.
    Reopened { item: ItemId },
    /// A person closes or abandons one attempt, which frees one of the two open slots.
    AttemptClosed { item: ItemId, attempt: u32 },
    /// A person moves an item to another tier.
    TierChanged { item: ItemId, tier: u8 },
    /// A person edits a proposal before accepting it.
    Edited {
        item: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier: Option<u8>,
    },
    /// Sets a root's budget outright. Only a person may; this is how it is raised.
    BudgetSet {
        root: ItemId,
        items_left: u32,
        max_depth: u8,
        cost_left_micros: u64,
        redos_per_item: u8,
    },
    /// Upstream's view of the pull request in the item's `pr`: its phase,
    /// reviews and checks. Only upstream sync writes it, on roots.
    PrStatus { item: ItemId, status: PrSnapshot },
    /// A source cannot read a repository (`status` is the HTTP code) or can
    /// again (`None`). The rest of the source keeps going; a person sees the
    /// stall in Needs you. Only the tracker writes it.
    SourceStalled {
        source: String,
        repo: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
    },
    /// Sets the standing rules for a repo, or for one root in it. The latest
    /// record for a scope replaces the earlier one; `text` and `checks` are
    /// separate keys, and a root's key overrides the repo's. Only a person may.
    RulesSet {
        target: Target,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<ItemId>,
        /// What the agent must follow, inline and at most [`MAX_RULES_TEXT`] bytes.
        /// Empty text with no checks clears the scope.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Commands to run before pushing. The agent runs them; the tracker never does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checks: Option<Vec<String>>,
    },
    /// Raises the log's format: records of formats up to `to` may follow.
    /// Only a person may, once every reader of the log is a build that reads
    /// `to`. It never goes down.
    FormatRaised { to: u32 },
    /// An operation the tracker refused and logged. Only the tracker writes it.
    Refused {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<ItemId>,
        op: String,
        reason: RefusalReason,
    },
    /// A kind this build does not know, in a format it reads. A fold skips
    /// it. A new kind raises the format, so only a log from before records
    /// had a version can hold one.
    #[serde(other)]
    Unknown,
}

impl WorkRecord {
    /// The `kind` field this record is written with.
    pub fn kind(&self) -> &'static str {
        match self {
            WorkRecord::Imported { .. } => "imported",
            WorkRecord::Proposed { .. } => "proposed",
            WorkRecord::Triaged { .. } => "triaged",
            WorkRecord::Accepted { .. } => "accepted",
            WorkRecord::Prerequisite { .. } => "prerequisite",
            WorkRecord::Shipped { .. } => "shipped",
            WorkRecord::Linked { .. } => "linked",
            WorkRecord::Unlinked { .. } => "unlinked",
            WorkRecord::Context { .. } => "context",
            WorkRecord::Claimed { .. } => "claimed",
            WorkRecord::Progress { .. } => "progress",
            WorkRecord::Done { .. } => "done",
            WorkRecord::Released { .. } => "released",
            WorkRecord::GaveUp { .. } => "gave_up",
            WorkRecord::Question { .. } => "question",
            WorkRecord::Answer { .. } => "answer",
            WorkRecord::Decision { .. } => "decision",
            WorkRecord::Finding { .. } => "finding",
            WorkRecord::ReviewVerdict { .. } => "review_verdict",
            WorkRecord::Step { .. } => "step",
            WorkRecord::Edit { .. } => "edit",
            WorkRecord::HumanInput { .. } => "human_input",
            WorkRecord::ForkedFrom { .. } => "forked_from",
            WorkRecord::Closed { .. } => "closed",
            WorkRecord::Rejected { .. } => "rejected",
            WorkRecord::Updated { .. } => "updated",
            WorkRecord::LeftScope { .. } => "left_scope",
            WorkRecord::ReturnedToScope { .. } => "returned_to_scope",
            WorkRecord::Reopened { .. } => "reopened",
            WorkRecord::AttemptClosed { .. } => "attempt_closed",
            WorkRecord::TierChanged { .. } => "tier_changed",
            WorkRecord::Edited { .. } => "edited",
            WorkRecord::BudgetSet { .. } => "budget_set",
            WorkRecord::PrStatus { .. } => "pr_status",
            WorkRecord::SourceStalled { .. } => "source_stalled",
            WorkRecord::RulesSet { .. } => "rules_set",
            WorkRecord::FormatRaised { .. } => "format_raised",
            WorkRecord::Refused { .. } => "refused",
            WorkRecord::Unknown => "unknown",
        }
    }

    /// The item the record is about, if it names one. A budget or rules record
    /// for a root answers with that root.
    pub fn item(&self) -> Option<&ItemId> {
        // No wildcard: a new kind must say which item it is about.
        match self {
            WorkRecord::Imported { item, .. }
            | WorkRecord::Proposed { item, .. }
            | WorkRecord::Triaged { item, .. }
            | WorkRecord::Accepted { item, .. }
            | WorkRecord::Prerequisite { item, .. }
            | WorkRecord::Shipped { item, .. }
            | WorkRecord::Linked { item, .. }
            | WorkRecord::Unlinked { item, .. }
            | WorkRecord::Context { item, .. }
            | WorkRecord::Claimed { item, .. }
            | WorkRecord::Progress { item, .. }
            | WorkRecord::Done { item, .. }
            | WorkRecord::Released { item, .. }
            | WorkRecord::GaveUp { item, .. }
            | WorkRecord::Question { item, .. }
            | WorkRecord::Answer { item, .. }
            | WorkRecord::Decision { item, .. }
            | WorkRecord::Finding { item, .. }
            | WorkRecord::ReviewVerdict { item, .. }
            | WorkRecord::Step { item, .. }
            | WorkRecord::Edit { item, .. }
            | WorkRecord::HumanInput { item, .. }
            | WorkRecord::ForkedFrom { item, .. }
            | WorkRecord::Closed { item, .. }
            | WorkRecord::Rejected { item }
            | WorkRecord::Updated { item, .. }
            | WorkRecord::LeftScope { item }
            | WorkRecord::ReturnedToScope { item }
            | WorkRecord::Reopened { item }
            | WorkRecord::AttemptClosed { item, .. }
            | WorkRecord::TierChanged { item, .. }
            | WorkRecord::Edited { item, .. }
            | WorkRecord::BudgetSet { root: item, .. }
            | WorkRecord::PrStatus { item, .. } => Some(item),
            WorkRecord::RulesSet { root, .. } | WorkRecord::Refused { item: root, .. } => {
                root.as_ref()
            }
            WorkRecord::SourceStalled { .. }
            | WorkRecord::FormatRaised { .. }
            | WorkRecord::Unknown => None,
        }
    }

    /// The evidence the record cites.
    pub fn evidence(&self) -> &[Evidence] {
        match self {
            WorkRecord::Decision { evidence, .. } | WorkRecord::Answer { evidence, .. } => evidence,
            WorkRecord::Prerequisite { new: Some(n), .. } => &n.evidence,
            _ => &[],
        }
    }

    /// The lowest format whose readers fold this record as this build does.
    /// A kind, or a use of a field, that an older fold would skip or ignore
    /// answers with the format that introduced it.
    pub fn format(&self) -> u32 {
        // No wildcard: a new kind does not compile until it names its format.
        // A new field the fold reads on a kind below needs a guard here too,
        // which only review catches (docs/upgrades.md).
        match self {
            WorkRecord::FormatRaised { to } => (*to).max(FORMAT_V2),
            WorkRecord::ReturnedToScope { .. } => FORMAT_V2,
            WorkRecord::Prerequisite {
                triage: Some(_), ..
            } => FORMAT_V3,
            WorkRecord::Imported { .. }
            | WorkRecord::Proposed { .. }
            | WorkRecord::Triaged { .. }
            | WorkRecord::Accepted { .. }
            | WorkRecord::Prerequisite { .. }
            | WorkRecord::Shipped { .. }
            | WorkRecord::Linked { .. }
            | WorkRecord::Unlinked { .. }
            | WorkRecord::Context { .. }
            | WorkRecord::Claimed { .. }
            | WorkRecord::Progress { .. }
            | WorkRecord::Done { .. }
            | WorkRecord::Released { .. }
            | WorkRecord::GaveUp { .. }
            | WorkRecord::Question { .. }
            | WorkRecord::Answer { .. }
            | WorkRecord::Decision { .. }
            | WorkRecord::Finding { .. }
            | WorkRecord::ReviewVerdict { .. }
            | WorkRecord::Step { .. }
            | WorkRecord::Edit { .. }
            | WorkRecord::HumanInput { .. }
            | WorkRecord::ForkedFrom { .. }
            | WorkRecord::Closed { .. }
            | WorkRecord::Rejected { .. }
            | WorkRecord::Updated { .. }
            | WorkRecord::LeftScope { .. }
            | WorkRecord::Reopened { .. }
            | WorkRecord::AttemptClosed { .. }
            | WorkRecord::TierChanged { .. }
            | WorkRecord::Edited { .. }
            | WorkRecord::BudgetSet { .. }
            | WorkRecord::PrStatus { .. }
            | WorkRecord::SourceStalled { .. }
            | WorkRecord::RulesSet { .. }
            | WorkRecord::Refused { .. }
            | WorkRecord::Unknown => FORMAT_V1,
        }
    }
}

/// A record as it sits on `work.events`: its format, when, which principal,
/// and in what role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLogged {
    /// The record's format. Left out at [`FORMAT_V1`], so a record of the
    /// first format is the bytes it always was. Decoding fails above
    /// [`FORMAT`]: use [`read`] to tell that from a malformed record.
    #[serde(
        default = "format_v1",
        skip_serializing_if = "is_format_v1",
        deserialize_with = "readable_format"
    )]
    pub v: u32,
    pub at_ms: u64,
    pub by: Key,
    pub actor: Actor,
    #[serde(flatten)]
    pub record: WorkRecord,
}

impl WorkLogged {
    /// A record to append, stamped with the lowest format that reads it.
    pub fn new(at_ms: u64, by: Key, actor: Actor, record: WorkRecord) -> Self {
        Self {
            v: record.format(),
            at_ms,
            by,
            actor,
            record,
        }
    }
}

/// Why a payload on `work.events` was not read.
#[derive(Debug)]
pub enum ReadError {
    /// The record is of a format above [`FORMAT`]. The reader must stop here:
    /// skipping it would fold a state its writer did not mean.
    Newer { v: u32 },
    /// Not a record of any format this build reads.
    Malformed(DecodeError),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Newer { v } => write!(
                f,
                "the record has format {v} and this build reads up to {FORMAT}: upgrade this reader"
            ),
            ReadError::Malformed(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ReadError {}

/// Decodes a payload of `work.events`, and says when it failed because the
/// record is newer than this build.
pub fn read(payload: &[u8]) -> Result<WorkLogged, ReadError> {
    #[derive(Deserialize)]
    struct Envelope {
        v: u32,
    }
    WorkLogged::decode(payload).map_err(|e| match serde_json::from_slice::<Envelope>(payload) {
        Ok(Envelope { v }) if v > FORMAT => ReadError::Newer { v },
        _ => ReadError::Malformed(e),
    })
}

fn format_v1() -> u32 {
    FORMAT_V1
}

fn is_format_v1(v: &u32) -> bool {
    *v == FORMAT_V1
}

fn readable_format<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(d)?;
    if (FORMAT_V1..=FORMAT).contains(&v) {
        Ok(v)
    } else {
        Err(serde::de::Error::custom(format_args!(
            "record format {v} is not one this build reads ({FORMAT_V1} to {FORMAT})"
        )))
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}
