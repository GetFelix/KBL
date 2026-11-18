//! One poll: read what changed upstream, compare it with what ingest
//! imported, and write the difference to the tracker.
//!
//! Every step is idempotent, because a poll can stop anywhere (a rate limit,
//! a crash) and the next one starts from the saved cursor, listing some
//! issues again. The tracker's fold refuses a second `imported` for an
//! upstream id, which is what makes an issue import once; ingest's own state
//! only avoids asking again.
//!
//! The upstream-change table is in docs/work-tracker.md.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context;
use kbl_core::content::Content;
use kbl_core::names::{ItemId, Key};
use kbl_core::work::{Actor, CloseWhy, PrPhase, PrSnapshot, WorkKind, WorkRecord};
use sha2::{Digest, Sha256};
use tracing::Instrument as _;

use crate::config::{Mode, Rule, Triage};
use crate::rules::first_match;
use crate::source::{
    Changes, Closed, Comment, Fetched, Issue, IssueRef, Releases, Source, SourceError, WriteBack,
};
use crate::state::{Awaiting, State, Tracked};
use crate::tracker_api::{Appended, Prerequisite, Root, Tracker};

/// Longest comment or body kept in a record, in bytes. Longer text is cut
/// and says so; the upstream issue has the rest.
const TEXT_LIMIT: usize = 3500;
/// Longest title kept in a record, in bytes.
const TITLE_LIMIT: usize = 200;
/// Failed write-backs per issue before ingest stops trying.
const MAX_WRITE_BACK_TRIES: u8 = 3;

pub(crate) struct Settings {
    pub(crate) rules: Vec<Rule>,
    pub(crate) repos: Vec<String>,
    /// How long one pass over every open imported issue takes, to notice a
    /// transfer or a deletion no listing shows. Each poll re-reads its share.
    /// None never reconciles on its own; a webhook still forces one.
    pub(crate) reconcile_period: Option<Duration>,
    /// Release-step requests one poll may make: pull request reads and compares.
    pub(crate) release_compares: u32,
}

pub(crate) struct Ingest<S, T> {
    source: S,
    tracker: T,
    settings: Settings,
    /// Lower-cased logins of our own credential.
    own: HashSet<String>,
    /// Seconds between polls now, which sizes the reconcile slice.
    interval_s: AtomicU64,
}

/// What one poll knows about the tracker's side, kept current as it writes.
struct Poll {
    state: State,
    /// Imported roots by upstream id.
    roots: HashMap<String, Root>,
    /// Roots another source imported, whose pull requests this worker reads
    /// when it can: see [`Ingest::foreign_pull_requests`].
    foreign: Vec<Root>,
    /// Issues seen this poll that are open and in scope, whose links to read.
    touched: Vec<String>,
    /// Accepted prerequisites with no issue, from the same listing as `roots`.
    prerequisites: Vec<Prerequisite>,
    /// The saved state's version, moved on by a save mid-poll.
    version: Option<u64>,
}

impl<S: Source, T: Tracker> Ingest<S, T> {
    pub(crate) fn new(source: S, tracker: T, settings: Settings) -> Self {
        let own = source
            .own_logins()
            .iter()
            .map(|l| l.to_ascii_lowercase())
            .collect();
        Self {
            source,
            tracker,
            settings,
            own,
            interval_s: AtomicU64::new(60),
        }
    }

    /// Tells the reconcile slice how long a poll will wait before the next.
    pub(crate) fn set_interval(&self, every: Duration) {
        self.interval_s
            .store(every.as_secs().max(1), Ordering::Relaxed);
    }

    /// Polls once. `reconcile` forces a full reconcile now (a webhook said an
    /// issue was transferred or deleted).
    pub(crate) async fn poll(&self, reconcile: bool) -> Result<(), SourceError> {
        let started = std::time::Instant::now();
        // The source is a kind word (`github`, `jira`), as in the metrics.
        let span = tracing::info_span!(
            "ingest.poll",
            source = self.source.name(),
            reconcile,
            outcome = tracing::field::Empty,
        );
        let result = self.poll_once(reconcile).instrument(span.clone()).await;
        span.record("outcome", if result.is_ok() { "ok" } else { "failed" });
        crate::metrics::poll_finished(self.source.name(), started.elapsed(), &result);
        result
    }

    async fn poll_once(&self, reconcile: bool) -> Result<(), SourceError> {
        let name = self.source.name();
        let (saved, version) = self.tracker.load(name).await?;
        let state: State = match saved {
            Some(v) => serde_json::from_value(v).context("the saved ingest state is unreadable")?,
            None => State::default(),
        };
        let mut state = state;
        state.migrate(&self.settings.repos);
        state.reset_changed(&self.settings.rules);
        let listed = self.tracker.roots().await?;
        let (own, foreign): (Vec<Root>, Vec<Root>) =
            listed.roots.into_iter().partition(|r| r.source == name);
        let mut roots: HashMap<String, Root> = own
            .into_iter()
            .map(|r| (r.upstream_id.clone(), r))
            .collect();
        alias_moved(&state, &mut roots);
        let mut poll = Poll {
            state,
            roots,
            foreign,
            touched: Vec::new(),
            prerequisites: listed
                .prerequisites
                .into_iter()
                .filter(|pre| pre.source == name)
                .collect(),
            version,
        };
        let done = self.run(&mut poll, reconcile).await;
        // Saved on failure too, so imports already made are remembered. The
        // cursors moved only if the poll got as far as the release step.
        let saved = self
            .tracker
            .save(
                name,
                &serde_json::to_value(&poll.state).map_err(anyhow::Error::from)?,
                poll.version,
            )
            .await;
        match (done, saved) {
            (Err(run), Err(save)) => {
                eprintln!("kbl-ingest: the state was not saved either: {save:#}");
                Err(run)
            }
            (Err(run), Ok(_)) => Err(run),
            (Ok(()), Err(save)) => Err(save.into()),
            (Ok(()), Ok(_)) => Ok(()),
        }
    }

    async fn run(&self, p: &mut Poll, reconcile: bool) -> Result<(), SourceError> {
        let Changes {
            issues,
            comments,
            cursors,
            denied,
        } = self
            .source
            .changed(&self.settings.repos, &p.state.cursors)
            .await?;
        crate::metrics::denied(self.source.name(), denied.len());
        self.report_stalls(p, &denied).await?;
        // A first sync, a lost cursor or a forced reconcile reads the whole
        // backlog; its age is not how far behind ingest is.
        let live = tracks_lag(reconcile, &p.state.cursors);
        for issue in issues {
            self.on_issue(p, &issue).await?;
            if live {
                crate::metrics::recorded(self.source.name(), &issue.updated_at);
            }
        }
        for c in comments {
            self.on_comment(p, &c).await?;
            if live {
                crate::metrics::recorded(self.source.name(), &c.updated_at);
            }
        }
        self.retry_waiting(p).await?;
        if self.source.reads_pull_requests() {
            self.pull_requests(p).await?;
            self.foreign_pull_requests(p).await?;
        }
        let (keys, reconciled) = self.reconcile_keys(p, reconcile);
        if !keys.is_empty() {
            // The other writer may have unlinked since, so a reconcile lets
            // the next touch try once more.
            for k in &keys {
                if let Some(t) = p.state.items.get_mut(k) {
                    t.not_ours.clear();
                }
            }
            self.reconcile(p, keys).await?;
        }
        self.link(p).await?;
        let span = tracing::info_span!("ingest.write_back", source = self.source.name());
        self.write_back(p).instrument(span).await?;
        self.announce(p).await?;
        // Last, so a crash after the issue is opened leaves it to the next
        // listing, which finds it by its marker.
        self.open_prerequisites(p, reconciled).await?;
        prune(p);
        p.state.cursors.extend(cursors);
        // Last, so a rate limit here holds up nothing else.
        self.releases(p).await
    }

    /// Tells the tracker which repos cannot be read, when that changes. Only
    /// those repos stall: the others' cursors move on.
    async fn report_stalls(
        &self,
        p: &mut Poll,
        denied: &BTreeMap<String, u16>,
    ) -> Result<(), SourceError> {
        let source = self.source.name().to_owned();
        for (repo, code) in denied {
            if p.state.stalled.get(repo) != Some(code) {
                self.write(&WorkRecord::SourceStalled {
                    source: source.clone(),
                    repo: repo.clone(),
                    status: Some(*code),
                })
                .await?;
                p.state.stalled.insert(repo.clone(), *code);
            }
        }
        let cleared: Vec<String> = p
            .state
            .stalled
            .keys()
            .filter(|r| !denied.contains_key(*r))
            .cloned()
            .collect();
        for repo in cleared {
            self.write(&WorkRecord::SourceStalled {
                source: source.clone(),
                repo: repo.clone(),
                status: None,
            })
            .await?;
            p.state.stalled.remove(&repo);
        }
        Ok(())
    }

    // ---- issues ----

    async fn on_issue(&self, p: &mut Poll, issue: &Issue) -> Result<(), SourceError> {
        // Before any rule: one of our own prerequisite issues is never a new root.
        if let Some(item) = self.prerequisite_marker(issue) {
            return self.on_prerequisite_issue(p, issue, item).await;
        }
        let key = issue.id.to_string();
        match p.state.items.get(&key).cloned() {
            None => match p.roots.get(&key).cloned() {
                // Pruned after it closed, and open again now.
                Some(root) if root.closed && issue.closed.is_none() => {
                    let rule = first_match(&self.settings.rules, issue).map_or("", |r| &r.name);
                    // Stands in for the close that was forgotten, so `follow`
                    // sees the issue reopen.
                    let mut t = tracked_as(&root.item, rule, issue);
                    t.closed = Some(Closed::Dropped);
                    self.follow(p, issue, t).await
                }
                Some(root) if !root.closed && issue.closed.is_some() => {
                    let rule = first_match(&self.settings.rules, issue).map_or("", |r| &r.name);
                    let mut t = tracked_as(&root.item, rule, issue);
                    // Only a person's reopen keeps the root open. Otherwise
                    // ingest's last save was lost (after an import or its own
                    // reopen) and the issue closed since, so `follow` closes it.
                    if root.reopened_by != Some(Actor::Person) {
                        return self.follow(p, issue, t).await;
                    }
                    t.closed = issue.closed;
                    p.state.items.insert(key, t);
                    Ok(())
                }
                _ => self.consider(p, issue).await,
            },
            Some(t) if t.moved_to.is_some() => Ok(()),
            Some(t) => self.follow(p, issue, t).await,
        }
    }

    /// An issue ingest has not imported: import it if a rule takes it and the
    /// rule has room, or leave it waiting upstream.
    async fn consider(&self, p: &mut Poll, issue: &Issue) -> Result<(), SourceError> {
        let key = issue.id.to_string();
        let rule = match first_match(&self.settings.rules, issue) {
            Some(r) if issue.closed.is_none() => r.clone(),
            _ => {
                p.state.waiting.remove(&key);
                return Ok(());
            }
        };
        // Left in `waiting` until its comments are in, so a poll that stops
        // in between does one more pass.
        let was_waiting = p.state.waiting.contains(&key);
        if let Some(root) = p.roots.get(&key).cloned() {
            // Imported by a poll that stopped before it saved.
            let item = root.item.clone();
            // The writes below are idempotent, so each step the crashed poll
            // may not have reached is redone. The root is tracked only once
            // they land: a saved item makes later polls follow it and skip them.
            self.context(&item, "issue", issue_text(issue)).await?;
            self.context(&item, "intake", intake_text(&rule, issue))
                .await?;
            // A reopened root is back in Triage for a person to accept again,
            // and a reopen through `follow` never admits, so neither does this.
            if rule.mode == Mode::Admit && !root.closed && root.reopened_by.is_none() {
                self.admit(
                    &item,
                    &rule,
                    issue,
                    root.stage == "triage" && !root.has_children,
                )
                .await?;
            }
            p.state
                .items
                .insert(key.clone(), tracked(&item, &rule, issue));
            self.park_unmapped(p, &rule, issue).await?;
            if was_waiting {
                self.import_comments(p, issue).await?;
            }
            p.state.waiting.remove(&key);
            p.touched.push(key);
            return Ok(());
        }
        if let Some(cap) = rule.cap
            && open_roots(p, &rule.name) >= cap as usize
        {
            p.state.waiting.insert(key);
            return Ok(());
        }
        let item = item_id(&key);
        let labels = kbl_core::work_fold::clean_labels(&issue.labels);
        let milestone = issue
            .milestone
            .as_deref()
            .and_then(kbl_core::work_fold::clean_milestone);
        let mut imported = WorkRecord::Imported {
            item: item.clone(),
            source: self.source.name().to_owned(),
            upstream_id: key.clone(),
            title: cut_title(&issue.title),
            labels: labels.clone(),
            milestone: milestone.clone(),
        };
        let mut appended = self.tracker.append(&imported).await?;
        // A log not yet raised to format 19 refuses the facts. The root is
        // imported without them, and the next poll writes them as an edit.
        let mut facts_written = true;
        if matches!(&appended, Appended::Refused(why) if why == "format")
            && let WorkRecord::Imported {
                labels, milestone, ..
            } = &mut imported
        {
            labels.clear();
            *milestone = None;
            facts_written = false;
            appended = self.tracker.append(&imported).await?;
        }
        match appended {
            Appended::Written => {}
            // Someone else's root has this id: it is imported, so leave it.
            Appended::Refused(why) if why == "duplicate" => {}
            Appended::Refused(why) => {
                eprintln!("kbl-ingest: {key} was not imported: {why}");
                return Ok(());
            }
        }
        self.context(&item, "issue", issue_text(issue)).await?;
        self.context(&item, "intake", intake_text(&rule, issue))
            .await?;
        if rule.mode == Mode::Admit {
            self.admit(&item, &rule, issue, true).await?;
        }
        self.map_class(&item, &rule, issue).await?;
        p.roots.insert(
            key.clone(),
            Root {
                item: item.clone(),
                source: self.source.name().to_owned(),
                upstream_id: key.clone(),
                stage: if rule.mode == Mode::Admit {
                    "ready"
                } else {
                    "triage"
                }
                .to_owned(),
                closed: false,
                closed_by_sync: false,
                closed_by_merge: Some(false),
                left_scope_by_sync: false,
                reopened_by: None,
                has_children: rule.mode == Mode::Admit && rule.triage == Triage::Run,
                pr: None,
                claimed_at_ms: None,
                pr_status: None,
                release: None,
                awaits_release: false,
            },
        );
        let mut t = tracked(&item, &rule, issue);
        if !facts_written {
            t.labels.clear();
            t.milestone = None;
        }
        p.state.items.insert(key.clone(), t);
        self.park_unmapped(p, &rule, issue).await?;
        if was_waiting {
            self.import_comments(p, issue).await?;
        }
        p.state.waiting.remove(&key);
        p.touched.push(key);
        Ok(())
    }

    /// Writes the type and area the rule's label maps give. The tracker
    /// refuses a value outside the team's vocabulary, which is logged and left.
    async fn map_class(
        &self,
        item: &ItemId,
        rule: &Rule,
        issue: &Issue,
    ) -> Result<(), SourceError> {
        let (item_type, area) = rule.mapped_class(issue);
        if item_type.is_none() && area.is_none() {
            return Ok(());
        }
        self.write(&WorkRecord::Edited {
            item: item.clone(),
            title: None,
            tier: None,
            labels: None,
            milestone: None,
            item_type,
            area,
            size: None,
            depends_on: None,
            proposal: None,
            split: None,
        })
        .await?;
        Ok(())
    }

    /// An issue whose rule names no repository for it goes to Needs you as
    /// out of scope, and returns to its stage once a mapping covers it.
    async fn park_unmapped(
        &self,
        p: &mut Poll,
        rule: &Rule,
        issue: &Issue,
    ) -> Result<(), SourceError> {
        if rule.target_for(issue).is_some() {
            return Ok(());
        }
        let key = issue.id.to_string();
        let Some(t) = p.state.items.get_mut(&key) else {
            return Ok(());
        };
        if t.in_scope {
            let item = t.item.clone();
            t.in_scope = false;
            self.write(&WorkRecord::LeftScope { item }).await?;
        }
        Ok(())
    }

    /// Comments made while the issue waited were not imported, since only
    /// imported issues take comments. They are read now that it is in.
    async fn import_comments(&self, p: &mut Poll, issue: &Issue) -> Result<(), SourceError> {
        for c in self.source.issue_comments(&issue.id).await? {
            self.on_comment(p, &c).await?;
        }
        Ok(())
    }

    /// An admitted root starts accepted, with one triage item and one augment
    /// item, or with neither when the rule skips triage. `accept` is false once a person may have acted on the root, so a
    /// retry does not undo their tier.
    async fn admit(
        &self,
        item: &ItemId,
        rule: &Rule,
        issue: &Issue,
        accept: bool,
    ) -> Result<(), SourceError> {
        if accept {
            self.write(&WorkRecord::Accepted {
                item: item.clone(),
                tier: Some(rule.tier_for(issue)),
                triage_skipped: (rule.triage == Triage::Skip).then(|| rule.name.clone()),
            })
            .await?;
        }
        if rule.triage == Triage::Skip {
            return Ok(());
        }
        for (suffix, work, key, title) in [
            ("t", WorkKind::Triage, "triage", "Triage"),
            ("a", WorkKind::Augment, "augment", "Augment"),
        ] {
            self.write(&WorkRecord::Proposed {
                item: ItemId::new(format!("{item}-{suffix}")).map_err(anyhow::Error::from)?,
                parent: Some(item.clone()),
                key: Key::new(key).map_err(anyhow::Error::from)?,
                work,
                title: cut_title(&format!("{title}: {}", issue.title)),
                review: None,
            })
            .await?;
        }
        Ok(())
    }

    /// Brings the item's labels and milestone up to upstream's. Until the
    /// tracker takes the edit, `t` keeps the old values and the next poll tries again.
    async fn reconcile_facts(
        &self,
        t: &mut Tracked,
        rules: &[Rule],
        issue: &Issue,
    ) -> Result<(), SourceError> {
        let labels = kbl_core::work_fold::clean_labels(&issue.labels);
        let milestone = issue
            .milestone
            .as_deref()
            .and_then(kbl_core::work_fold::clean_milestone);
        let same_labels = labels.len() == t.labels.len()
            && labels
                .iter()
                .zip(&t.labels)
                .all(|(a, b)| a.eq_ignore_ascii_case(b));
        if same_labels && milestone == t.milestone {
            return Ok(());
        }
        let edit = WorkRecord::Edited {
            item: t.item.clone(),
            title: None,
            tier: None,
            labels: Some(labels.clone()),
            milestone: Some(milestone.clone().unwrap_or_default()),
            item_type: None,
            area: None,
            size: None,
            depends_on: None,
            proposal: None,
            split: None,
        };
        // The tracker already holding these values is as good as a write.
        let written = match self.tracker.append(&edit).await? {
            Appended::Written => true,
            Appended::Refused(why) if why == "no_change" => true,
            Appended::Refused(why) => {
                eprintln!("kbl-ingest: labels and milestone were not updated: {why}");
                false
            }
        };
        if written {
            t.labels = labels;
            t.milestone = milestone;
            if let Some(rule) = rules.iter().find(|r| r.name == t.rule) {
                self.map_class(&t.item, rule, issue).await?;
            }
        }
        Ok(())
    }

    /// An issue ingest imported before.
    async fn follow(&self, p: &mut Poll, issue: &Issue, mut t: Tracked) -> Result<(), SourceError> {
        let key = issue.id.to_string();
        let Some(root) = p.roots.get(&key).cloned() else {
            eprintln!("kbl-ingest: {key} is tracked but the tracker has no root for it");
            return Ok(());
        };
        match (t.closed, issue.closed) {
            (None, Some(how)) => {
                if !root.closed {
                    let why = match how {
                        Closed::Completed => CloseWhy::Completed,
                        Closed::Dropped => CloseWhy::Upstream,
                    };
                    self.write(&WorkRecord::Closed {
                        item: t.item.clone(),
                        why,
                    })
                    .await?;
                    set_closed(p, &key, true);
                }
                t.closed = Some(how);
                p.state.items.insert(key, t);
                return Ok(());
            }
            (Some(_), None) => {
                // A root a person dismissed stays closed.
                if root.closed && root.closed_by_sync {
                    self.write(&WorkRecord::Reopened {
                        item: t.item.clone(),
                    })
                    .await?;
                    set_closed(p, &key, false);
                }
                t.closed = None;
                t.written_back = false;
                t.in_scope = true;
            }
            (Some(_), Some(how)) => {
                t.closed = Some(how);
                p.state.items.insert(key, t);
                return Ok(());
            }
            // The close was written but the save that would have recorded it
            // was lost, and the issue reopened before the next poll.
            (None, None) => {
                if root.closed && root.closed_by_sync && root.closed_by_merge == Some(false) {
                    self.write(&WorkRecord::Reopened {
                        item: t.item.clone(),
                    })
                    .await?;
                    set_closed(p, &key, false);
                }
            }
        }
        let open = p.roots.get(&key).filter(|r| !r.closed);
        if let Some(root) = open {
            let left_by_sync = root.left_scope_by_sync;
            let rules = &self.settings.rules;
            let in_rule = match rules.iter().find(|r| r.name == t.rule) {
                _ if t.prerequisite => true,
                Some(r) => r.filter(issue) && r.target_for(issue).is_some(),
                // Rebuilt after a prune while no rule took it: the rule that
                // takes it now is its rule, if that rule has room.
                None if t.rule.is_empty() => match first_match(rules, issue) {
                    Some(r) if r.cap.is_none_or(|c| open_roots(p, &r.name) < c as usize) => {
                        t.rule.clone_from(&r.name);
                        true
                    }
                    _ => false,
                },
                // Its rule was renamed or removed, which takes its items out.
                None => false,
            };
            if !in_rule {
                if t.in_scope {
                    self.write(&WorkRecord::LeftScope {
                        item: t.item.clone(),
                    })
                    .await?;
                    t.in_scope = false;
                }
            } else {
                // The fold's flag decides too: a saved state from before the
                // record, or one rebuilt by a crash retry, says in scope.
                // Until the log is raised to the record's format the tracker
                // refuses it, so the item stays in Needs you and this is tried
                // again on the next change or reconcile.
                if !t.in_scope || left_by_sync {
                    t.in_scope = self
                        .write(&WorkRecord::ReturnedToScope {
                            item: t.item.clone(),
                        })
                        .await?;
                }
                self.reconcile_facts(&mut t, rules, issue).await?;
                let fp = fingerprint(issue);
                if fp != t.fingerprint {
                    self.write(&WorkRecord::Updated {
                        item: t.item.clone(),
                        title: (issue.title != t.title).then(|| cut_title(&issue.title)),
                    })
                    .await?;
                    self.context(&t.item, "issue", issue_text(issue)).await?;
                    t.fingerprint = fp;
                    t.title.clone_from(&issue.title);
                }
                p.touched.push(key.clone());
            }
        }
        p.state.items.insert(key, t);
        Ok(())
    }

    // ---- comments ----

    async fn on_comment(&self, p: &mut Poll, c: &Comment) -> Result<(), SourceError> {
        // Our own comments (a pull request note) are not new information.
        if self.own.contains(&c.author.to_ascii_lowercase()) {
            return Ok(());
        }
        let key = c.issue.to_string();
        let Some(t) = p.state.items.get(&key) else {
            return Ok(());
        };
        if p.roots.get(&key).is_none_or(|r| r.closed)
            || t.comments.get(&c.id) == Some(&c.updated_at)
        {
            return Ok(());
        }
        let item = t.item.clone();
        let text = format!("@{} commented:\n{}", c.author, cut(&c.body));
        self.context(&item, &format!("gh-c{}", c.id), text).await?;
        if let Some(t) = p.state.items.get_mut(&key) {
            t.comments.insert(c.id, c.updated_at.clone());
        }
        Ok(())
    }

    // ---- waiting, reconcile, links, write back ----

    /// Matches that were over their rule's cap, oldest first, as roots close.
    async fn retry_waiting(&self, p: &mut Poll) -> Result<(), SourceError> {
        let mut waiting: Vec<IssueRef> = p
            .state
            .waiting
            .iter()
            .filter_map(|k| IssueRef::parse(k))
            .collect();
        waiting.sort();
        for id in waiting {
            match self.source.fetch(&id).await? {
                Fetched::Issue(issue) => self.consider(p, &issue).await?,
                Fetched::Moved(_) | Fetched::Gone => {
                    p.state.waiting.remove(&id.to_string());
                }
            }
        }
        Ok(())
    }

    /// Reads the pull request each open root reports and records its phase,
    /// reviews and checks when they changed. Only a request our credential
    /// opened, in the repo of its issue, is read; see [`Self::may_write_to`].
    async fn pull_requests(&self, p: &mut Poll) -> Result<(), SourceError> {
        let mut due: Vec<(String, IssueRef)> = p
            .roots
            .iter()
            .filter(|(k, r)| {
                !r.closed && p.state.items.get(*k).is_some_and(|t| t.moved_to.is_none())
            })
            .filter_map(|(k, r)| {
                let pr = IssueRef::parse(r.pr.as_deref()?)?;
                self.may_write_to(k, &pr).then(|| (k.clone(), pr))
            })
            .collect();
        due.sort();
        for (key, pr) in due {
            let item = p.roots.get(&key).map(|r| r.item.clone());
            let claimed = p.roots.get(&key).and_then(|r| r.claimed_at_ms);
            let Some(status) = self
                .linked_request(&key, &pr, item.as_ref(), claimed, Some(&key))
                .await?
            else {
                continue;
            };
            self.on_pull_request(p, &key, status).await?;
        }
        Ok(())
    }

    /// Reads a request an agent named and returns its state, if it is ours
    /// and tied to the item. The agent names the request, so it must be tied
    /// by more than the agent's word: the branch name the tracker derives
    /// from the item id, or, when `closes` names the issue, a closing
    /// reference to it. A request made before the first claim cannot be the
    /// item's work, whatever branch the agent names. With no claim time, only
    /// a closing reference will do.
    async fn linked_request(
        &self,
        key: &str,
        pr: &IssueRef,
        item: Option<&ItemId>,
        claimed: Option<u64>,
        closes: Option<&str>,
    ) -> Result<Option<PrSnapshot>, SourceError> {
        let pull = match self.source.pull_request(pr).await {
            Ok(Some(pull)) => pull,
            Ok(None) => return Ok(None),
            Err(e @ SourceError::RateLimited { .. }) => return Err(e),
            // The name comes from an agent; one bad request must not stop the poll.
            Err(e) => {
                eprintln!("kbl-ingest: could not read {pr} for {key}: {e}");
                return Ok(None);
            }
        };
        let ours = self.own.contains(&pull.author.to_ascii_lowercase());
        let Some(status) = pull.status.filter(|_| ours) else {
            eprintln!("kbl-ingest: {pr} was not opened by us; not reading its state");
            return Ok(None);
        };
        let closes = closes.is_some_and(|issue| closes_issue(&pull.body, issue));
        let branch = item.is_some_and(|i| pull.head_ref == kbl_core::work::pr_branch(i));
        let linked = match claimed {
            Some(at) => pull.created_ms.is_some_and(|c| c >= at) && (closes || branch),
            None => closes,
        };
        if !linked {
            eprintln!(
                "kbl-ingest: {pr} is not linked to {key} (branch, reference or age); not reading its state"
            );
            return Ok(None);
        }
        Ok(Some(status))
    }

    /// Records the state of the request an agent opened for a root another
    /// source imported, such as a Jira issue, so that source's worker can act
    /// on a merge. It writes the state and nothing else: the root's own worker
    /// decides what upstream says about it. Only a request in a repo this
    /// worker's rules name is read, and only one tied to the item by its
    /// branch, since a closing reference names issues of this source.
    async fn foreign_pull_requests(&self, p: &mut Poll) -> Result<(), SourceError> {
        let mut due: Vec<(String, Root, IssueRef)> = p
            .foreign
            .iter()
            .filter(|r| !r.closed)
            // A merge is final, and the root's own worker closes it soon.
            .filter(|r| {
                r.pr_status
                    .as_ref()
                    .is_none_or(|s| s.phase != PrPhase::Merged)
            })
            .filter_map(|r| {
                let pr = IssueRef::parse(r.pr.as_deref()?)?;
                let named = self
                    .settings
                    .repos
                    .iter()
                    .any(|repo| repo.eq_ignore_ascii_case(&pr.repo));
                named.then(|| (format!("{}:{}", r.source, r.upstream_id), r.clone(), pr))
            })
            .collect();
        due.sort_by(|a, b| a.0.cmp(&b.0));
        for (label, root, pr) in due {
            let Some(status) = self
                .linked_request(&label, &pr, Some(&root.item), root.claimed_at_ms, None)
                .await?
            else {
                continue;
            };
            if root.pr_status.as_ref() == Some(&status) {
                continue;
            }
            let record = WorkRecord::PrStatus {
                item: root.item.clone(),
                status,
            };
            match self.tracker.append(&record).await? {
                Appended::Written => {}
                Appended::Refused(why) if why == "duplicate" => {}
                Appended::Refused(why) => {
                    eprintln!(
                        "kbl-ingest: the state of the pull request for {label} was refused: {why}"
                    );
                }
            }
        }
        Ok(())
    }

    async fn on_pull_request(
        &self,
        p: &mut Poll,
        key: &str,
        status: PrSnapshot,
    ) -> Result<(), SourceError> {
        let Some(t) = p.state.items.get(key) else {
            return Ok(());
        };
        let item = t.item.clone();
        // Compared with what the fold holds, which a change of `pr` clears.
        if p.roots.get(key).and_then(|r| r.pr_status.as_ref()) != Some(&status) {
            let record = WorkRecord::PrStatus {
                item: item.clone(),
                status: status.clone(),
            };
            match self.tracker.append(&record).await? {
                Appended::Written => {}
                Appended::Refused(why) if why == "duplicate" => {}
                Appended::Refused(why) => {
                    eprintln!(
                        "kbl-ingest: the state of the pull request for {key} was refused: {why}"
                    );
                    return Ok(());
                }
            }
            if let Some(r) = p.roots.get_mut(key) {
                r.pr_status = Some(status.clone());
            }
        }
        // A merge is the work done; the tracker records it and never merges.
        if status.phase == PrPhase::Merged && p.roots.get(key).is_some_and(|r| !r.closed) {
            self.write(&WorkRecord::Closed {
                item,
                why: CloseWhy::Merged,
            })
            .await?;
            set_closed(p, key, true);
        }
        Ok(())
    }

    /// Records the oldest release after the merge that carries a merged root
    /// a link waits on, which frees that link. Only such roots are looked at,
    /// so an ordinary merge costs no requests here, and each poll makes at
    /// most `release_compares` pull request reads and compares together.
    async fn releases(&self, p: &mut Poll) -> Result<(), SourceError> {
        let mut due: Vec<(String, ItemId, IssueRef)> = p
            .roots
            .iter()
            .filter(|(_, r)| {
                r.closed && r.stage == "done" && r.awaits_release && r.release.is_none()
            })
            .filter_map(|(k, r)| {
                let status = r.pr_status.as_ref()?;
                let pr = IssueRef::parse(&status.pr)?;
                (status.phase == PrPhase::Merged && r.pr.as_deref() == Some(status.pr.as_str()))
                    .then(|| (k.clone(), r.item.clone(), pr))
            })
            .filter(|(k, _, pr)| self.may_write_to(k, pr))
            .collect();
        due.sort();
        let keys: HashSet<&str> = due.iter().map(|(k, _, _)| k.as_str()).collect();
        p.state.awaiting.retain(|k, _| keys.contains(k.as_str()));
        let mut budget = self.settings.release_compares;
        for (key, _, pr) in &due {
            let pr_id = pr.to_string();
            if p.state.awaiting.get(key).is_some_and(|a| a.pr == pr_id) {
                continue;
            }
            if budget == 0 {
                break;
            }
            budget -= 1;
            match self.source.pull_request(pr).await {
                Ok(Some(pull)) => {
                    p.state.awaiting.insert(
                        key.clone(),
                        Awaiting {
                            pr: pr_id,
                            merge: pull.merge,
                            misses: BTreeSet::new(),
                            skip: 0,
                        },
                    );
                }
                // Gone for now; it may answer again, so nothing is cached.
                Ok(None) => {}
                Err(e @ SourceError::RateLimited { .. }) => return Err(e),
                Err(e) => eprintln!("kbl-ingest: could not read {pr} for {key}: {e}"),
            }
        }
        // A root backing off sits this poll out.
        due.retain(|(key, _, _)| match p.state.awaiting.get_mut(key) {
            Some(a) if a.skip > 0 => {
                a.skip -= 1;
                false
            }
            _ => true,
        });
        // One listing per repo, back to the earliest merge it is needed for.
        let mut since: BTreeMap<String, u64> = BTreeMap::new();
        for (key, _, pr) in &due {
            if let Some((_, at)) = p.state.awaiting.get(key).and_then(|a| a.merge.as_ref()) {
                let s = since.entry(pr.repo.clone()).or_insert(*at);
                *s = (*s).min(*at);
            }
        }
        let mut listed: HashMap<String, Releases> = HashMap::new();
        for (repo, at) in since {
            match self.source.releases(&repo, at).await {
                Ok(releases) => {
                    listed.insert(repo, releases);
                }
                Err(e @ SourceError::RateLimited { .. }) => return Err(e),
                Err(e) => eprintln!("kbl-ingest: could not list the releases of {repo}: {e}"),
            }
        }
        for (key, item, pr) in due {
            let Some(releases) = listed.get(&pr.repo) else {
                continue;
            };
            let Some(Awaiting {
                merge: Some((sha, merged_ms)),
                misses,
                ..
            }) = p.state.awaiting.get(&key).cloned()
            else {
                continue;
            };
            if !releases.covers(merged_ms) {
                eprintln!(
                    "kbl-ingest: the release pages of {} end before the merge for {key}; a person records its release",
                    pr.repo
                );
                back_off(p, &key);
                continue;
            }
            let untried = releases
                .list
                .iter()
                .filter(|r| r.published_ms >= merged_ms && !misses.contains(&r.tag));
            for release in untried {
                if budget == 0 {
                    return Ok(());
                }
                budget -= 1;
                match self.source.contains(&pr.repo, &release.tag, &sha).await {
                    Ok(true) => {
                        let shipped = WorkRecord::Shipped {
                            item: item.clone(),
                            release: release.tag.clone(),
                        };
                        if self.write(&shipped).await? {
                            p.state.awaiting.remove(&key);
                            if let Some(r) = p.roots.get_mut(&key) {
                                r.release = Some(release.tag.clone());
                            }
                        } else {
                            back_off(p, &key);
                        }
                        break;
                    }
                    Ok(false) => {
                        if let Some(a) = p.state.awaiting.get_mut(&key) {
                            a.misses.insert(release.tag.clone());
                        }
                    }
                    Err(e @ SourceError::RateLimited { .. }) => return Err(e),
                    Err(e) => {
                        eprintln!(
                            "kbl-ingest: could not compare {} with {sha} for {key}: {e}",
                            release.tag
                        );
                        back_off(p, &key);
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Fetches each open imported issue, which finds what no listing shows.
    /// The open imported issues this poll re-reads, and whether that
    /// completes a pass over all of them. A forced reconcile reads every one;
    /// otherwise a poll reads its share, so each is read once per period.
    /// The position is the last key read, so removals never make a poll skip one.
    fn reconcile_keys(&self, p: &mut Poll, forced: bool) -> (Vec<String>, bool) {
        let open: Vec<String> = p
            .state
            .items
            .iter()
            .filter(|(_, t)| t.closed.is_none() && t.moved_to.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        if forced {
            p.state.reconcile_after = None;
            return (open, true);
        }
        let Some(period) = self.settings.reconcile_period else {
            return (Vec::new(), false);
        };
        let interval = self.interval_s.load(Ordering::Relaxed);
        let polls = (period.as_secs().div_ceil(interval)).max(1) as usize;
        let (keys, wrapped) = next_slice(&open, p.state.reconcile_after.as_deref(), polls);
        if let Some(last) = keys.last() {
            p.state.reconcile_after = Some(last.clone());
        }
        (keys, wrapped)
    }

    async fn reconcile(&self, p: &mut Poll, keys: Vec<String>) -> Result<(), SourceError> {
        for key in keys {
            let Some(id) = IssueRef::parse(&key) else {
                continue;
            };
            match self.source.fetch(&id).await? {
                Fetched::Issue(issue) => self.on_issue(p, &issue).await?,
                Fetched::Moved(to) => self.moved(p, &key, &to).await?,
                Fetched::Gone => self.gone(p, &key).await?,
            }
        }
        Ok(())
    }

    /// Transferred: the same item under its new id, with a record of the move.
    async fn moved(&self, p: &mut Poll, old: &str, to: &IssueRef) -> Result<(), SourceError> {
        let new = to.to_string();
        let Some(t) = p.state.items.get(old).cloned() else {
            return Ok(());
        };
        if p.state.items.contains_key(&new) {
            return Ok(());
        }
        let text = format!("Moved upstream from {old} to {new}.");
        self.context(&t.item, "moved", text).await?;
        if let Some(root) = p.roots.get(old).cloned() {
            p.roots.insert(new.clone(), root);
        }
        p.state.items.insert(new.clone(), t);
        if let Some(o) = p.state.items.get_mut(old) {
            o.moved_to = Some(new.clone());
        }
        match self.source.fetch(to).await? {
            Fetched::Issue(issue) => self.on_issue(p, &issue).await,
            _ => Ok(()),
        }
    }

    /// Deleted upstream, or no longer visible: dropped.
    async fn gone(&self, p: &mut Poll, key: &str) -> Result<(), SourceError> {
        let (Some(t), Some(root)) = (p.state.items.get(key).cloned(), p.roots.get(key)) else {
            return Ok(());
        };
        if !root.closed {
            self.write(&WorkRecord::Closed {
                item: t.item.clone(),
                why: CloseWhy::Upstream,
            })
            .await?;
            set_closed(p, key, true);
        }
        if let Some(t) = p.state.items.get_mut(key) {
            t.closed = Some(Closed::Dropped);
        }
        Ok(())
    }

    /// Blocked-by and sub-issue links become edges once both ends are imported.
    async fn link(&self, p: &mut Poll) -> Result<(), SourceError> {
        for key in std::mem::take(&mut p.touched) {
            let Some(id) = IssueRef::parse(&key) else {
                continue;
            };
            let links = self.source.links(&id).await?;
            let upstream: BTreeSet<String> = links
                .blocked_by
                .iter()
                .chain(&links.sub_issues)
                .map(ToString::to_string)
                .collect();
            let Some(t) = p.state.items.get_mut(&key) else {
                continue;
            };
            let found: Vec<String> = upstream
                .iter()
                .filter(|l| !t.linked.contains(*l) && !t.not_ours.contains(*l))
                .cloned()
                .collect();
            t.pending_links.extend(found);
            if links.complete {
                t.pending_links.retain(|l| upstream.contains(l));
                t.not_ours.retain(|l| upstream.contains(l));
                self.unlink_removed(p, &key, &upstream).await?;
            }
        }
        let waiting: Vec<(String, ItemId, Vec<String>)> = p
            .state
            .items
            .iter()
            .filter(|(_, t)| !t.pending_links.is_empty() && t.closed.is_none())
            .map(|(k, t)| {
                (
                    k.clone(),
                    t.item.clone(),
                    t.pending_links.iter().cloned().collect(),
                )
            })
            .collect();
        for (key, item, pending) in waiting {
            for to in pending {
                let Some(other) = item_of(p, &to) else {
                    continue;
                };
                let refused = match self
                    .tracker
                    .append(&WorkRecord::Linked {
                        item: item.clone(),
                        to: other,
                    })
                    .await?
                {
                    Appended::Written => None,
                    Appended::Refused(why) => {
                        eprintln!("kbl-ingest: link {key} -> {to} refused: {why}");
                        Some(why)
                    }
                };
                if let Some(t) = p.state.items.get_mut(&key) {
                    t.pending_links.remove(&to);
                    match refused.as_deref() {
                        // Only an edge ingest made is its to unlink later.
                        None => {
                            t.linked.insert(to);
                        }
                        Some("already_linked") => {
                            t.not_ours.insert(to);
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// Removes the edges ingest made for `key` whose dependency is gone from
    /// the complete upstream listing `upstream`. The tracker refuses an edge
    /// ingest's own link did not make, which then stays.
    async fn unlink_removed(
        &self,
        p: &mut Poll,
        key: &str,
        upstream: &BTreeSet<String>,
    ) -> Result<(), SourceError> {
        let Some(t) = p.state.items.get(key) else {
            return Ok(());
        };
        let item = t.item.clone();
        let gone: Vec<String> = t.linked.difference(upstream).cloned().collect();
        // A transferred dependency is listed under its new id, which names the same item.
        let still: HashSet<ItemId> = upstream.iter().filter_map(|u| item_of(p, u)).collect();
        // An id that names no item may be a transfer ingest has not seen yet.
        let unknown = upstream.iter().any(|u| item_of(p, u).is_none());
        for to in gone {
            let Some(id) = IssueRef::parse(&to) else {
                continue;
            };
            // A repo ingest cannot read now may simply be hidden from the listing.
            if p.state.stalled.contains_key(&id.repo) {
                continue;
            }
            let Some(other) = item_of(p, &to).filter(|o| !still.contains(o)) else {
                if let Some(t) = p.state.items.get_mut(key) {
                    t.linked.remove(&to);
                }
                continue;
            };
            if unknown && matches!(self.source.fetch(&id).await?, Fetched::Moved(_)) {
                continue;
            }
            match self
                .tracker
                .append(&WorkRecord::Unlinked {
                    item: item.clone(),
                    to: other,
                })
                .await?
            {
                Appended::Written => {}
                Appended::Refused(why) => {
                    eprintln!("kbl-ingest: unlink {key} -> {to} refused: {why}");
                }
            }
            if let Some(t) = p.state.items.get_mut(key) {
                t.linked.remove(&to);
            }
        }
        Ok(())
    }

    /// An issue dropped while an agent's pull request is open: the request
    /// becomes a draft with a note. Retried until it succeeds.
    async fn write_back(&self, p: &mut Poll) -> Result<(), SourceError> {
        let due: Vec<String> = p
            .state
            .items
            .iter()
            .filter(|(_, t)| {
                t.closed == Some(Closed::Dropped) && !t.written_back && t.moved_to.is_none()
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in due {
            let pr = p
                .roots
                .get(&key)
                .and_then(|r| r.pr.as_deref())
                .and_then(IssueRef::parse)
                .filter(|pr| self.may_write_to(&key, pr));
            if let Some(pr) = pr {
                let note = format!(
                    "{key} was closed upstream as not planned, so this pull request is now a draft. \
                     Nothing further will be done on it until a person decides."
                );
                let mark = crate::source::mark(&key, "draft", &pr.to_string());
                match self
                    .source
                    .write_back(&WriteBack::DraftPullRequest { pr, note, mark })
                    .await
                {
                    Ok(()) => {}
                    // The wait applies to the whole poll; try again after it.
                    Err(e @ SourceError::RateLimited { .. }) => return Err(e),
                    // Cut short, it may have landed: not a failed try. The
                    // retry finds the mark and does not post twice.
                    Err(e) if crate::lease::is_fenced(&e) => return Err(e),
                    // An agent names the pull request, so a refusal must not hold the cursor.
                    Err(e) => {
                        eprintln!("kbl-ingest: write-back for {key} failed: {e}");
                        if let Some(t) = p.state.items.get_mut(&key) {
                            t.write_back_tries += 1;
                            if t.write_back_tries < MAX_WRITE_BACK_TRIES {
                                continue;
                            }
                        }
                    }
                }
            }
            if let Some(t) = p.state.items.get_mut(&key) {
                t.written_back = true;
            }
        }
        Ok(())
    }

    /// Tells the upstream issue about an agent's pull request, as each rule
    /// asks: a comment, a link and a workflow transition. Each is done once
    /// per pull request, kept in `announced`, so a retry or a re-poll never
    /// repeats it. The request is an agent's word, so it must be in a
    /// repository the rule maps issues to.
    async fn announce(&self, p: &mut Poll) -> Result<(), SourceError> {
        let mut due: Vec<(String, IssueRef, IssueRef, &'static str, Rule)> = Vec::new();
        for (key, root) in &p.roots {
            let Some(t) = p.state.items.get(key) else {
                continue;
            };
            let (Some(rule), Some(pr), Some(issue)) = (
                self.settings.rules.iter().find(|r| r.name == t.rule),
                root.pr.as_deref().and_then(IssueRef::parse),
                IssueRef::parse(key),
            ) else {
                continue;
            };
            // A GitLab request lives in the rule's own project, or the target.
            let allowed = rule
                .target_repo
                .iter()
                .chain(rule.components.values())
                .chain(rule.area_paths.values())
                .chain(rule.gitlab.then_some(&rule.repo));
            if t.moved_to.is_some()
                || !t.in_scope
                || !allowed
                    .into_iter()
                    .any(|r| r.eq_ignore_ascii_case(&pr.repo))
            {
                continue;
            }
            // A GitLab request is announced once it was read and tied to the
            // item, so a number an agent made up names nothing on the issue.
            if !(rule.gitlab && root.pr_status.is_none()) {
                due.push((
                    key.clone(),
                    issue.clone(),
                    pr.clone(),
                    "linked",
                    rule.clone(),
                ));
            }
            if root
                .pr_status
                .as_ref()
                .is_some_and(|s| s.phase == PrPhase::Merged)
            {
                due.push((key.clone(), issue, pr, "merged", rule.clone()));
            }
        }
        due.sort_by(|a, b| (&a.0, a.3).cmp(&(&b.0, b.3)));
        for (key, issue, pr, stage, rule) in due {
            let mark = format!("{stage}:{pr}");
            if p.state
                .items
                .get(&key)
                .is_some_and(|t| t.announced.contains(&mark))
            {
                continue;
            }
            let mut calls = Vec::new();
            if stage == "linked" {
                if rule.link_pr {
                    calls.push(WriteBack::LinkPullRequest {
                        issue: issue.clone(),
                        pr: pr.clone(),
                    });
                }
                if rule.comment {
                    calls.push(WriteBack::Comment {
                        issue: issue.clone(),
                        body: format!("An agent opened {pr} for this issue."),
                        mark: crate::source::mark(&key, stage, &pr.to_string()),
                    });
                }
                if let Some(name) = &rule.transition_on_pr {
                    calls.push(WriteBack::Transition {
                        issue: issue.clone(),
                        name: name.clone(),
                    });
                }
            } else {
                if rule.comment {
                    calls.push(WriteBack::Comment {
                        issue: issue.clone(),
                        body: format!("{pr} was merged."),
                        mark: crate::source::mark(&key, stage, &pr.to_string()),
                    });
                }
                if let Some(name) = &rule.transition_on_merge {
                    calls.push(WriteBack::Transition {
                        issue: issue.clone(),
                        name: name.clone(),
                    });
                }
            }
            let mut failed = false;
            for call in calls {
                match self.source.write_back(&call).await {
                    Ok(()) => {}
                    Err(e @ SourceError::RateLimited { .. }) => return Err(e),
                    // An unknown outcome, not a failure: retried without
                    // counting a try, and every call checks before it writes.
                    Err(e) if crate::lease::is_fenced(&e) => return Err(e),
                    Err(e) => {
                        eprintln!("kbl-ingest: write-back for {key} failed: {e}");
                        failed = true;
                        break;
                    }
                }
            }
            if let Some(t) = p.state.items.get_mut(&key) {
                t.announce_tries += u8::from(failed);
                // Every call checks upstream before it writes, so a retry
                // after a failure is safe; after a few it is given up on.
                if !failed || t.announce_tries >= MAX_WRITE_BACK_TRIES {
                    t.announced.insert(mark);
                    t.announce_tries = 0;
                }
            }
        }
        Ok(())
    }

    /// The pull request comes from an agent, so it is trusted only in the
    /// repo of the issue it works on, and only if a rule takes that repo. The
    /// adapter also checks its author against our own logins.
    fn may_write_to(&self, key: &str, pr: &IssueRef) -> bool {
        let Some(issue) = IssueRef::parse(key) else {
            return false;
        };
        let named = self
            .settings
            .rules
            .iter()
            .any(|r| r.repo.eq_ignore_ascii_case(&issue.repo));
        let allowed = named && pr.repo.eq_ignore_ascii_case(&issue.repo);
        if !allowed {
            eprintln!("kbl-ingest: ignoring the pull request {pr} for {key}: not in its repo");
        }
        allowed
    }

    // ---- prerequisites ----

    /// The item named by the marker in one of our own issues, in a repo
    /// whose rule opts in. Ingest writes its marker last, so the last line
    /// that looks like one is ours.
    fn prerequisite_marker(&self, issue: &Issue) -> Option<ItemId> {
        if !self.own.contains(&issue.author.to_ascii_lowercase()) {
            return None;
        }
        self.opening_rule(&issue.id.repo)?;
        let id = issue
            .body
            .lines()
            .rev()
            .find_map(|l| l.trim().strip_prefix(MARKER))?;
        ItemId::new(id.trim()).ok()
    }

    /// The rule that lets ingest open prerequisite issues in `repo`.
    fn opening_rule(&self, repo: &str) -> Option<&Rule> {
        self.settings
            .rules
            .iter()
            .find(|r| r.open_prerequisites && r.repo.eq_ignore_ascii_case(repo))
    }

    /// One of our prerequisite issues in a listing: tie it to its item if
    /// that write was lost, then follow it like any imported issue.
    async fn on_prerequisite_issue(
        &self,
        p: &mut Poll,
        issue: &Issue,
        item: ItemId,
    ) -> Result<(), SourceError> {
        let key = issue.id.to_string();
        if let Some(t) = p.state.items.get(&key).cloned() {
            if t.moved_to.is_some() {
                return Ok(());
            }
            return self.follow(p, issue, t).await;
        }
        let known = p.roots.get(&key).map(|r| r.item.clone());
        let tied = p
            .roots
            .values()
            .find(|r| r.item == item)
            .map(|r| r.upstream_id.clone());
        let item = match (known, tied) {
            (Some(root), _) => root,
            (None, Some(other)) => {
                // Two processes both opened one; a person closes the spare.
                eprintln!("kbl-ingest: {key} is an orphan: {item} is already tied to {other}");
                return Ok(());
            }
            (None, None) if self.attach(p, &item, &issue.id, &issue.title).await? => item,
            (None, None) => return Ok(()),
        };
        let mut t = tracked_as(&item, "", issue);
        t.prerequisite = true;
        self.follow(p, issue, t).await
    }

    /// Opens an issue for each prerequisite a person accepted in a repo whose
    /// rule opts in, and ties the two. Nothing is sent unless a rule opts in.
    async fn open_prerequisites(&self, p: &mut Poll, reconciled: bool) -> Result<(), SourceError> {
        // An entry for an item that closed or was tied is done with.
        let listed: HashSet<&str> = p.prerequisites.iter().map(|x| x.item.as_str()).collect();
        p.state
            .opened
            .retain(|item, _| listed.contains(item.as_str()));
        let pending = std::mem::take(&mut p.prerequisites);
        for pre in &pending {
            let Some(rule) = self.opening_rule(&pre.repo) else {
                continue;
            };
            if p.roots.values().any(|r| r.item == pre.item) {
                continue;
            }
            let id = match p.state.opened.get(pre.item.as_str()).map(String::as_str) {
                Some("") if !reconciled => {
                    // An earlier poll may have opened it and not lived to say
                    // so; its marker ties it when the listing shows it. A
                    // reconcile gives up on that and tries again.
                    continue;
                }
                Some(k) if !k.is_empty() => IssueRef::parse(k),
                _ => None,
            };
            let id = match id {
                Some(id) => id,
                None => {
                    // Claim the open in the versioned state first, so of two
                    // processes only the one whose save wins sends the POST.
                    // Narrow window left: a second process that loaded the state
                    // after this save, listed roots before this tie lands, and is
                    // on a reconcile poll saves at the new version and opens a
                    // second issue. The window is one POST round trip; the spare
                    // is logged as an orphan and a person closes it.
                    p.state.opened.insert(pre.item.to_string(), String::new());
                    let state = serde_json::to_value(&p.state).map_err(anyhow::Error::from)?;
                    match self
                        .tracker
                        .save(self.source.name(), &state, p.version)
                        .await
                    {
                        Ok(v) => p.version = Some(v),
                        Err(e) => {
                            eprintln!("kbl-ingest: not opening issues this poll: {e:#}");
                            p.state.opened.remove(pre.item.as_str());
                            return Ok(());
                        }
                    }
                    let body = prerequisite_body(pre, rule.link_waiter);
                    match self
                        .source
                        .open_issue(&rule.repo, &cut_title(&pre.title), &body)
                        .await
                    {
                        Ok(id) => id,
                        Err(e @ SourceError::RateLimited { .. }) => {
                            p.state.opened.remove(pre.item.as_str());
                            return Err(e);
                        }
                        // A repo we cannot write to must not hold the cursors.
                        Err(e) => {
                            eprintln!("kbl-ingest: no issue opened for {}: {e}", pre.item);
                            p.state.opened.remove(pre.item.as_str());
                            continue;
                        }
                    }
                }
            };
            p.state.opened.insert(pre.item.to_string(), id.to_string());
            self.attach(p, &pre.item, &id, &pre.title).await?;
        }
        Ok(())
    }

    /// Ties a prerequisite to its issue. The fold checks that a person
    /// accepted it and that the issue is in its target repo.
    async fn attach(
        &self,
        p: &mut Poll,
        item: &ItemId,
        id: &IssueRef,
        title: &str,
    ) -> Result<bool, SourceError> {
        let key = id.to_string();
        let record = WorkRecord::Imported {
            item: item.clone(),
            source: self.source.name().to_owned(),
            upstream_id: key.clone(),
            title: cut_title(title),
            labels: vec![],
            milestone: None,
        };
        let refused = self.tracker.append(&record).await?;
        p.state.opened.remove(item.as_str());
        if let Appended::Refused(why) = refused {
            eprintln!("kbl-ingest: {key} was not tied to {item}: {why}");
            return Ok(false);
        }
        p.roots.insert(
            key.clone(),
            Root {
                item: item.clone(),
                source: self.source.name().to_owned(),
                upstream_id: key,
                stage: "ready".to_owned(),
                closed: false,
                closed_by_sync: false,
                closed_by_merge: Some(false),
                left_scope_by_sync: false,
                reopened_by: None,
                has_children: false,
                pr: None,
                claimed_at_ms: None,
                pr_status: None,
                release: None,
                awaits_release: false,
            },
        );
        Ok(true)
    }

    // ---- writes ----

    /// Appends a record and says whether the log has it. A refusal that says
    /// it is already there counts as written; any other is printed, not
    /// returned, so one bad record does not stop the poll.
    async fn write(&self, record: &WorkRecord) -> Result<bool, SourceError> {
        match self.tracker.append(record).await? {
            Appended::Written => Ok(true),
            Appended::Refused(why) if why == "duplicate" => Ok(true),
            Appended::Refused(why) => {
                eprintln!("kbl-ingest: a {} record was refused: {why}", record.kind());
                Ok(false)
            }
        }
    }

    async fn context(&self, item: &ItemId, entry: &str, text: String) -> Result<(), SourceError> {
        self.write(&WorkRecord::Context {
            item: item.clone(),
            entry: Key::new(entry).map_err(anyhow::Error::from)?,
            text: Content::Text(text),
        })
        .await?;
        Ok(())
    }
}

/// The line that ties an issue ingest opened to its prerequisite item.
pub(crate) const MARKER: &str = "kbl-prerequisite:";

/// The issue body. An agent wrote `why`, so a marker line in it is dropped:
/// only ingest's own, written last, may name the item.
/// Whether this poll's changes are fresh enough for the lag metric.
fn tracks_lag(reconcile: bool, cursors: &BTreeMap<String, crate::source::Cursor>) -> bool {
    !reconcile && !cursors.is_empty()
}

fn prerequisite_body(pre: &Prerequisite, link_waiter: bool) -> String {
    let why: String = pre
        .why
        .as_deref()
        .unwrap_or("The reason is in the tracker.")
        .lines()
        .filter(|l| !l.trim().starts_with(MARKER))
        .collect::<Vec<_>>()
        .join("\n");
    let needed = match &pre.needed_by {
        Some(up) if link_waiter => format!("\n\nNeeded by {up}."),
        _ => String::new(),
    };
    format!("{}{needed}\n\n{MARKER} {}", cut(&why), pre.item)
}

fn tracked(item: &ItemId, rule: &Rule, issue: &Issue) -> Tracked {
    tracked_as(item, &rule.name, issue)
}

fn tracked_as(item: &ItemId, rule: &str, issue: &Issue) -> Tracked {
    Tracked {
        item: item.clone(),
        rule: rule.to_owned(),
        fingerprint: fingerprint(issue),
        title: issue.title.clone(),
        labels: kbl_core::work_fold::clean_labels(&issue.labels),
        milestone: issue
            .milestone
            .as_deref()
            .and_then(kbl_core::work_fold::clean_milestone),
        closed: None,
        in_scope: true,
        comments: BTreeMap::new(),
        pending_links: Default::default(),
        linked: Default::default(),
        not_ours: Default::default(),
        written_back: false,
        write_back_tries: 0,
        announced: Default::default(),
        announce_tries: 0,
        moved_to: None,
        prerequisite: false,
    }
}

/// Whether `body` has a closing keyword (close, fix, resolve and their forms)
/// followed by the issue, as `owner/repo#n` or `#n`.
fn closes_issue(body: &str, issue: &str) -> bool {
    let Some((_, number)) = issue.split_once('#') else {
        return false;
    };
    let body = body.to_ascii_lowercase();
    let issue = issue.to_ascii_lowercase();
    let words: Vec<&str> = body
        .split(|c: char| c.is_whitespace() || c == ':')
        .filter(|w| !w.is_empty())
        .collect();
    words.windows(2).any(|w| {
        let keyword = matches!(
            w[0],
            "close"
                | "closes"
                | "closed"
                | "fix"
                | "fixes"
                | "fixed"
                | "resolve"
                | "resolves"
                | "resolved"
        );
        let target = w[1].trim_end_matches(['.', ',', ';']);
        keyword && (target == issue || target == format!("#{number}"))
    })
}

/// Open roots the rule has now.
fn open_roots(p: &Poll, rule: &str) -> usize {
    p.state
        .items
        .iter()
        .filter(|(_, t)| t.rule == rule && t.moved_to.is_none())
        .filter(|(k, _)| p.roots.get(*k).is_some_and(|r| !r.closed))
        .map(|(_, t)| &t.item)
        .collect::<HashSet<_>>()
        .len()
}

/// Forgets closed issues, whose entries (and comment maps) would otherwise
/// grow the saved state without bound. The tracker's root says all that is
/// needed to handle one that is reopened: see `on_issue`. A dropped issue is
/// kept until its pull request has been dealt with.
/// Polls a root sits out after a release lookup that failed or cannot finish,
/// so one root cannot spend every poll's budget.
const RELEASE_BACKOFF_POLLS: u32 = 10;

/// The item for an upstream id. A closed issue may have been pruned from the
/// state; its root remains.
fn item_of(p: &Poll, key: &str) -> Option<ItemId> {
    p.roots
        .get(key)
        .map(|r| r.item.clone())
        .or_else(|| p.state.items.get(key).map(|o| o.item.clone()))
}

fn back_off(p: &mut Poll, key: &str) {
    if let Some(a) = p.state.awaiting.get_mut(key) {
        a.skip = RELEASE_BACKOFF_POLLS;
    }
}

fn prune(p: &mut Poll) {
    // The new id of a transfer is the only entry that keeps the move from
    // importing the issue a second time under it.
    let targets: HashSet<String> = p
        .state
        .items
        .values()
        .filter_map(|t| t.moved_to.clone())
        .collect();
    let done: Vec<String> = p
        .state
        .items
        .iter()
        .filter(|(k, t)| {
            !targets.contains(*k)
                && t.moved_to.is_none()
                && t.closed.is_some()
                && (t.closed == Some(Closed::Completed) || t.written_back)
                && p.roots.get(*k).is_some_and(|r| r.closed)
        })
        .map(|(k, _)| k.clone())
        .collect();
    for k in done {
        p.state.items.remove(&k);
    }
}

/// The tracker knows a transferred issue's root only under the id it was
/// imported with, so each new id is given the root of the id it moved from.
/// A chain of moves is followed to its end.
pub(crate) fn alias_moved(state: &State, roots: &mut HashMap<String, Root>) {
    for (old, t) in &state.items {
        let Some(root) = roots.get(old).cloned() else {
            continue;
        };
        let mut seen = HashSet::from([old.clone()]);
        let mut next = t.moved_to.clone();
        while let Some(new) = next.filter(|n| seen.insert(n.clone())) {
            // As `moved` does: the old id's root wins.
            roots.insert(new.clone(), root.clone());
            next = state.items.get(&new).and_then(|n| n.moved_to.clone());
        }
    }
}

fn set_closed(p: &mut Poll, key: &str, closed: bool) {
    if let Some(r) = p.roots.get_mut(key) {
        r.closed = closed;
        // Ingest is the only writer here, so its close is a sync close.
        r.closed_by_sync = closed;
        if !closed {
            r.reopened_by = Some(Actor::Tracker);
        }
    }
}

/// A stable item id for an upstream id, so a retry after a crash names the
/// same item instead of making a second one.
pub(crate) fn item_id(upstream: &str) -> ItemId {
    let digest = Sha256::digest(upstream.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    ItemId::new(format!("g{hex}")).expect("hex is a valid name")
}

/// Hashes what an edit would change: title, body and labels.
fn fingerprint(issue: &Issue) -> String {
    let mut labels: Vec<String> = issue
        .labels
        .iter()
        .map(|l| l.to_ascii_lowercase())
        .collect();
    labels.sort();
    let mut h = Sha256::new();
    for part in [&issue.title, &issue.body, &labels.join("\n")] {
        h.update(part.len().to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn issue_text(issue: &Issue) -> String {
    format!(
        "{} by @{}\n{}\n\n{}",
        issue.id,
        issue.author,
        issue.url,
        cut(&issue.body)
    )
}

fn intake_text(rule: &Rule, issue: &Issue) -> String {
    let repo = rule
        .target_for(issue)
        .unwrap_or_else(|| "no repository is mapped for it, so it waits in Needs you".to_owned());
    let how = match rule.mode {
        Mode::Admit if rule.triage == Triage::Skip => "admitted without triage",
        Mode::Admit => "admitted",
        Mode::Propose => "proposed, waiting for a person to accept it",
    };
    format!(
        "Rule {}: {how}. Work in {repo}. Tier {}.",
        rule.name,
        rule.tier_for(issue)
    )
}

fn cut_title(s: &str) -> String {
    let mut end = TITLE_LIMIT.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// Text cut at a character boundary to fit a record.
fn cut(s: &str) -> String {
    if s.len() <= TEXT_LIMIT {
        return s.to_owned();
    }
    let mut end = TEXT_LIMIT;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[cut; the rest is upstream]", &s[..end])
}

/// The next `1/polls` of the sorted `open` keys after `after`, going round
/// from the end to the start, and whether the slice reached the end.
fn next_slice(open: &[String], after: Option<&str>, polls: usize) -> (Vec<String>, bool) {
    if open.is_empty() {
        return (Vec::new(), false);
    }
    let take = open.len().div_ceil(polls).min(open.len());
    let start = after.map_or(0, |a| open.partition_point(|k| k.as_str() <= a));
    let keys = (0..take)
        .map(|i| open[(start + i) % open.len()].clone())
        .collect();
    (keys, start + take >= open.len())
}

#[cfg(test)]
mod tests;
