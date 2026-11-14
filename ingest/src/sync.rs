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

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Context;
use kbl_core::content::Content;
use kbl_core::names::{ItemId, Key};
use kbl_core::work::{CloseWhy, PrPhase, PrSnapshot, WorkKind, WorkRecord};
use sha2::{Digest, Sha256};

use crate::config::{Mode, Rule};
use crate::rules::first_match;
use crate::source::{
    Changes, Closed, Comment, Fetched, Issue, IssueRef, Source, SourceError, WriteBack,
};
use crate::state::{State, Tracked};
use crate::tracker_api::{Appended, Root, Tracker};

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
    /// Polls between full reconciles, which notice transfers and deletions.
    pub(crate) reconcile_every: u32,
}

pub(crate) struct Ingest<S, T> {
    source: S,
    tracker: T,
    settings: Settings,
    /// Lower-cased logins of our own credential.
    own: HashSet<String>,
}

/// What one poll knows about the tracker's side, kept current as it writes.
struct Poll {
    state: State,
    /// Imported roots by upstream id.
    roots: HashMap<String, Root>,
    /// Issues seen this poll that are open and in scope, whose links to read.
    touched: Vec<String>,
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
        }
    }

    /// Polls once. `reconcile` forces a full reconcile now (a webhook said an
    /// issue was transferred or deleted).
    pub(crate) async fn poll(&self, reconcile: bool) -> Result<(), SourceError> {
        let name = self.source.name();
        let (saved, version) = self.tracker.load(name).await?;
        let state: State = match saved {
            Some(v) => serde_json::from_value(v).context("the saved ingest state is unreadable")?,
            None => State::default(),
        };
        let mut state = state;
        state.migrate(&self.settings.repos);
        let roots = self
            .tracker
            .roots()
            .await?
            .into_iter()
            .filter(|r| r.source == name)
            .map(|r| (r.upstream_id.clone(), r))
            .collect();
        let mut poll = Poll {
            state,
            roots,
            touched: Vec::new(),
        };
        let done = self.run(&mut poll, reconcile).await;
        // Saved on failure too, so imports already made are remembered; the
        // cursors only moved if the poll finished.
        let saved = self
            .tracker
            .save(
                name,
                &serde_json::to_value(&poll.state).map_err(anyhow::Error::from)?,
                version,
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
        self.report_stalls(p, &denied).await?;
        for issue in issues {
            self.on_issue(p, &issue).await?;
        }
        for c in comments {
            self.on_comment(p, &c).await?;
        }
        self.retry_waiting(p).await?;
        self.pull_requests(p).await?;
        p.state.polls += 1;
        if reconcile || p.state.polls >= self.settings.reconcile_every {
            p.state.polls = 0;
            self.reconcile(p).await?;
        }
        self.link(p).await?;
        self.write_back(p).await?;
        prune(p);
        p.state.cursors.extend(cursors);
        Ok(())
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
                // A person reopened the root of an issue that is still closed.
                Some(root) if !root.closed && issue.closed.is_some() => {
                    let rule = first_match(&self.settings.rules, issue).map_or("", |r| &r.name);
                    let mut t = tracked_as(&root.item, rule, issue);
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
            p.state
                .items
                .insert(key.clone(), tracked(&item, &rule, issue));
            // The writes below are idempotent, so each step the crashed poll
            // may not have reached is redone.
            self.context(&item, "issue", issue_text(issue)).await?;
            self.context(&item, "intake", intake_text(&rule, issue))
                .await?;
            if rule.mode == Mode::Admit && !root.closed {
                self.admit(
                    &item,
                    &rule,
                    issue,
                    root.stage == "triage" && !root.has_children,
                )
                .await?;
            }
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
        let imported = WorkRecord::Imported {
            item: item.clone(),
            source: self.source.name().to_owned(),
            upstream_id: key.clone(),
            title: cut_title(&issue.title),
        };
        match self.tracker.append(&imported).await? {
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
                has_children: rule.mode == Mode::Admit,
                pr: None,
                claimed_at_ms: None,
                pr_status: None,
            },
        );
        p.state
            .items
            .insert(key.clone(), tracked(&item, &rule, issue));
        if was_waiting {
            self.import_comments(p, issue).await?;
        }
        p.state.waiting.remove(&key);
        p.touched.push(key);
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
    /// item. `accept` is false once a person may have acted on the root, so a
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
            })
            .await?;
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
            (None, None) => {}
        }
        let still_open = p.roots.get(&key).is_some_and(|r| !r.closed);
        if still_open {
            let in_rule = self
                .settings
                .rules
                .iter()
                .find(|r| r.name == t.rule)
                .is_some_and(|r| r.filter(issue));
            if !in_rule {
                if t.in_scope {
                    self.write(&WorkRecord::LeftScope {
                        item: t.item.clone(),
                    })
                    .await?;
                    t.in_scope = false;
                }
            } else {
                t.in_scope = true;
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
            let pull = match self.source.pull_request(&pr).await {
                Ok(Some(pull)) => pull,
                Ok(None) => continue,
                Err(e @ SourceError::RateLimited { .. }) => return Err(e),
                // The name comes from an agent; one bad request must not stop the poll.
                Err(e) => {
                    eprintln!("kbl-ingest: could not read {pr} for {key}: {e}");
                    continue;
                }
            };
            let ours = self.own.contains(&pull.author.to_ascii_lowercase());
            let Some(status) = pull.status.filter(|_| ours) else {
                eprintln!("kbl-ingest: {pr} was not opened by us; not reading its state");
                continue;
            };
            // The agent names the request, so it must be tied to this item by
            // more than the agent's word: the branch name the tracker derives
            // from the item id, or a closing reference to the issue.
            let item = p.roots.get(&key).map(|r| r.item.clone());
            let claimed = p.roots.get(&key).and_then(|r| r.claimed_at_ms);
            let closes = closes_issue(&pull.body, &key);
            // A request made before the first claim cannot be this item's
            // work, whatever branch the agent names. With no claim time, only
            // a closing reference will do.
            let linked = match claimed {
                Some(at) => {
                    pull.created_ms.is_some_and(|c| c >= at)
                        && (closes
                            || item.is_some_and(|i| pull.head_ref == kbl_core::work::pr_branch(&i)))
                }
                None => closes,
            };
            if !linked {
                eprintln!(
                    "kbl-ingest: {pr} is not linked to {key} (branch, reference or age); not reading its state"
                );
                continue;
            }
            self.on_pull_request(p, &key, status).await?;
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

    /// Fetches each open imported issue, which finds what no listing shows.
    async fn reconcile(&self, p: &mut Poll) -> Result<(), SourceError> {
        let keys: Vec<String> = p
            .state
            .items
            .iter()
            .filter(|(_, t)| t.closed.is_none() && t.moved_to.is_none())
            .map(|(k, _)| k.clone())
            .collect();
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
            if let Some(t) = p.state.items.get_mut(&key) {
                let found: Vec<String> = links
                    .blocked_by
                    .iter()
                    .chain(&links.sub_issues)
                    .map(ToString::to_string)
                    .filter(|l| !t.linked.contains(l))
                    .collect();
                t.pending_links.extend(found);
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
                // A closed issue may have been pruned from the state; its root remains.
                let Some(other) = p
                    .roots
                    .get(&to)
                    .map(|r| r.item.clone())
                    .or_else(|| p.state.items.get(&to).map(|o| o.item.clone()))
                else {
                    continue;
                };
                match self
                    .tracker
                    .append(&WorkRecord::Linked {
                        item: item.clone(),
                        to: other,
                    })
                    .await?
                {
                    Appended::Written => {}
                    Appended::Refused(why) => {
                        eprintln!("kbl-ingest: link {key} -> {to} refused: {why}");
                    }
                }
                if let Some(t) = p.state.items.get_mut(&key) {
                    t.pending_links.remove(&to);
                    t.linked.insert(to);
                }
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
                match self
                    .source
                    .write_back(&WriteBack::DraftPullRequest { pr, note })
                    .await
                {
                    Ok(()) => {}
                    // The wait applies to the whole poll; try again after it.
                    Err(e @ SourceError::RateLimited { .. }) => return Err(e),
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

    // ---- writes ----

    /// Appends a record. A refusal that says it is already there is fine; any
    /// other refusal is logged, since the next poll will find the same thing.
    async fn write(&self, record: &WorkRecord) -> Result<(), SourceError> {
        match self.tracker.append(record).await? {
            Appended::Written => {}
            Appended::Refused(why) if why == "duplicate" => {}
            Appended::Refused(why) => {
                eprintln!("kbl-ingest: a {} record was refused: {why}", record.kind());
            }
        }
        Ok(())
    }

    async fn context(&self, item: &ItemId, entry: &str, text: String) -> Result<(), SourceError> {
        self.write(&WorkRecord::Context {
            item: item.clone(),
            entry: Key::new(entry).map_err(anyhow::Error::from)?,
            text: Content::Text(text),
        })
        .await
    }
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
        closed: None,
        in_scope: true,
        comments: BTreeMap::new(),
        pending_links: Default::default(),
        linked: Default::default(),
        written_back: false,
        write_back_tries: 0,
        moved_to: None,
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

fn set_closed(p: &mut Poll, key: &str, closed: bool) {
    if let Some(r) = p.roots.get_mut(key) {
        r.closed = closed;
        // Ingest is the only writer here, so its close is a sync close.
        r.closed_by_sync = closed;
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
    let repo = rule.target_repo.as_deref().unwrap_or(&issue.id.repo);
    let how = match rule.mode {
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

#[cfg(test)]
mod tests;
