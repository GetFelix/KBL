//! What the ingest worker may do through the tracker. It writes only the
//! records upstream sync needs, as the tracker's role under its own name, and
//! keeps its cursor in Felix so a restart does not import again.

use kbl_core::artifact::INLINE_LIMIT;
use kbl_core::content::Content;
use kbl_core::names::{ItemId, Key};
use kbl_core::work::{Actor, CloseWhy, PrPhase, ReviewPass, ReviewSpec, WorkKind, WorkRecord};
use kbl_core::work_fold::State;
use serde_json::{Value, json};

use super::{Error, TextRead, Tracker};
use crate::felix::Broker;
use std::collections::HashMap;

/// Longest saved state: a cache value, not a log.
const MAX_STATE_BYTES: usize = 4 << 20;

/// The source whose worker reads pull requests. Its token may record a
/// request's state on a root from any source, since an agent's request lives
/// here whatever tracker the issue is in.
const PULL_REQUEST_SOURCE: &str = "github";

/// Whether ingest may write this record. Budgets, claims, answers and the
/// records a person writes are not among them.
fn allowed(record: &WorkRecord) -> bool {
    match record {
        // A policy acceptance is the tracker's own write, never ingest's.
        WorkRecord::Accepted { admitted_by, .. } => admitted_by.is_none(),
        WorkRecord::IntakeTier { .. } => true,
        WorkRecord::Imported { .. }
        | WorkRecord::Context { .. }
        | WorkRecord::Linked { .. }
        | WorkRecord::Unlinked { .. }
        | WorkRecord::Updated { .. }
        | WorkRecord::LeftScope { .. }
        | WorkRecord::ReturnedToScope { .. }
        | WorkRecord::PrStatus { .. }
        | WorkRecord::FlakeRetried { .. }
        | WorkRecord::SourceStalled { .. }
        | WorkRecord::Reopened { .. }
        | WorkRecord::Shipped { .. } => true,
        // Whether it opens fix work is the tracker's call, under `auto_fix_tries`.
        WorkRecord::CheckFailed { fix, .. } => !fix,
        // Upstream's labels and milestone and a rule's class; never a person's title or tier.
        WorkRecord::Edited { title, tier, .. } => title.is_none() && tier.is_none(),
        // The two items an admitted root starts with.
        WorkRecord::Proposed { work, .. } => matches!(work, WorkKind::Triage | WorkKind::Augment),
        WorkRecord::Closed { why, .. } => matches!(
            why,
            CloseWhy::Completed | CloseWhy::Upstream | CloseWhy::Merged
        ),
        _ => false,
    }
}

/// The existing items a record acts on. Ingest may touch only roots it
/// imported, so these must all be among them.
fn targets(record: &WorkRecord) -> Vec<&ItemId> {
    match record {
        WorkRecord::Accepted { item, .. }
        | WorkRecord::IntakeTier { item, .. }
        | WorkRecord::Context { item, .. }
        | WorkRecord::Updated { item, .. }
        | WorkRecord::LeftScope { item }
        | WorkRecord::ReturnedToScope { item }
        | WorkRecord::PrStatus { item, .. }
        | WorkRecord::FlakeRetried { item, .. }
        | WorkRecord::CheckFailed { item, .. }
        | WorkRecord::Reopened { item }
        | WorkRecord::Shipped { item, .. }
        | WorkRecord::Edited { item, .. }
        | WorkRecord::Closed { item, .. } => vec![item],
        WorkRecord::Linked { item, to } | WorkRecord::Unlinked { item, to } => vec![item, to],
        WorkRecord::Proposed {
            parent: Some(parent),
            ..
        } => vec![parent],
        _ => Vec::new(),
    }
}

/// The flake list as ingest and people read it.
pub(crate) fn flake_list(state: &kbl_core::work_fold::State) -> Vec<Value> {
    state
        .flakes
        .iter()
        .map(|(name, f)| {
            json!({
                "flake": name,
                "check": f.check,
                "pattern": f.pattern,
                "issue": f.issue,
                "hits": f.hits,
            })
        })
        .collect()
}

/// The id of a root's review of a request: the same on every retry.
fn review_id(root: &ItemId, pr: &str, pass: &str) -> ItemId {
    let hash =
        kbl_core::artifact::ArtifactId::of(format!("review\n{root}\n{pr}\n{pass}").as_bytes());
    ItemId::new(format!("w{}", &hash.as_str()[2..22])).expect("hash characters are valid")
}

fn review_key(pr: &str, pass: ReviewPass) -> Key {
    let hash = kbl_core::artifact::ArtifactId::of(format!("{pr}\n{pass:?}").as_bytes());
    Key::new(format!("review-{}", &hash.as_str()[2..18])).expect("hash characters are valid")
}

fn refused(why: &str) -> Error {
    Error::Refused { why: why.into() }
}

impl<B: Broker> Tracker<B> {
    /// For a context record over an entry no agent owns: the offset of the
    /// entry now and what reading its text back found. The read happens with
    /// the core lock dropped.
    async fn stored_context(&self, record: &WorkRecord) -> Result<Option<(u64, TextRead)>, Error> {
        let WorkRecord::Context { item, entry, .. } = record else {
            return Ok(None);
        };
        let at = {
            let mut core = self.0.core.lock().await;
            let Some(it) = core.state.items.get(item) else {
                return Ok(None);
            };
            let Some(&at) = it.context.get(entry) else {
                return Ok(None);
            };
            if it.context_agents.contains_key(entry) {
                return Ok(None);
            }
            if let Some(t) = Self::cached_text(&mut core, at) {
                return Ok(Some((at, TextRead::Text(t))));
            }
            at
        };
        let read = self
            .read_texts(&[at])
            .await?
            .remove(&at)
            .unwrap_or(TextRead::Gone);
        if let TextRead::Text(t) = &read {
            self.0.core.lock().await.recall.texts.insert(at, t.clone());
        }
        Ok(Some((at, read)))
    }

    /// [`Self::ingest_record_in`] for a worker that is not tied to a source.
    #[cfg(test)]
    pub(crate) async fn ingest_record(&self, by: Key, record: WorkRecord) -> Result<Value, Error> {
        self.ingest_record_in(by, None, record).await
    }

    /// Appends an upstream sync record if the fold accepts it. A worker tied
    /// to a `source` acts only on that source's roots, except that the pull
    /// request worker may also record the state of a request for any root.
    pub(crate) async fn ingest_record_in(
        &self,
        by: Key,
        source: Option<&str>,
        mut record: WorkRecord,
    ) -> Result<Value, Error> {
        if !allowed(&record) {
            return Err(refused("not_allowed"));
        }
        if let WorkRecord::Context {
            text: Content::Text(text),
            ..
        } = &record
            && text.len() > INLINE_LIMIT
        {
            return Err(refused("too_big"));
        }
        // A record with no existing target (`Proposed` without a parent) is
        // refused by the fold: only the two admitted-root items are allowed.
        if matches!(record, WorkRecord::Proposed { parent: None, .. }) {
            return Err(refused("not_allowed"));
        }
        // A worker tied to a source imports only that source's issues.
        let imports = match &record {
            WorkRecord::Imported { source, .. } => Some(source.as_str()),
            _ => None,
        };
        if source.is_some() && imports.is_some() && imports != source {
            return Err(refused("wrong_source"));
        }
        self.begin().await?;
        let kind = record.kind();
        let stored = self.stored_context(&record).await?;
        let offset = {
            let mut core = self.0.core.lock().await;
            let sources = core.state.imported_sources();
            let reaches = |item: &ItemId| match (sources.get(item), source) {
                (None, _) => false,
                (Some(_), None) => true,
                (Some(root), Some(own)) => {
                    *root == own
                        || (own == PULL_REQUEST_SOURCE
                            && matches!(
                                record,
                                WorkRecord::PrStatus { .. } | WorkRecord::CheckFailed { .. }
                            ))
                }
            };
            if !targets(&record).into_iter().all(reaches) {
                return Err(refused("not_imported"));
            }
            drop(sources);
            if let WorkRecord::Closed {
                item,
                why: CloseWhy::Merged,
            } = &record
                && !merged_request_reported(&core.state, item)
            {
                return Err(refused("not_merged"));
            }
            // A release can carry only a merge the tracker has seen, and the
            // release a person recorded stands.
            if let WorkRecord::Shipped { item, .. } = &record {
                if !merged_request_reported(&core.state, item) {
                    return Err(refused("not_merged"));
                }
                if core
                    .state
                    .items
                    .get(item)
                    .is_some_and(|it| it.release.is_some())
                {
                    return Err(refused("already_shipped"));
                }
            }
            // Ingest removes only an edge it made, so it may not take over one
            // that a prerequisite, an answer, a person or an agent made.
            if let WorkRecord::Linked { item, to } | WorkRecord::Unlinked { item, to } = &record {
                let edge = (item.clone(), to.clone());
                let ours = core.recall.ingest_edges.contains(&edge);
                match (&record, core.state.edges.contains(&edge)) {
                    (WorkRecord::Linked { .. }, true) if !ours => {
                        return Err(refused("already_linked"));
                    }
                    (WorkRecord::Unlinked { .. }, false) => return Err(refused("not_linked")),
                    (WorkRecord::Unlinked { .. }, true) if !ours => {
                        return Err(refused("not_ingests"));
                    }
                    _ => {}
                }
            }
            // Ingest that lost its saved state writes every open root's entries
            // again. The same text over an entry no agent owns changes nothing a
            // read shows, so it is not logged.
            if let WorkRecord::Context { item, entry, text } = &record
                && let Some(it) = core.state.items.get(item)
                && let Some(&at) = it.context.get(entry)
                && !it.context_agents.contains_key(entry)
                && let Some((known, read)) = &stored
                && *known == at
            {
                match read {
                    TextRead::Text(old) if old == text => {
                        return Ok(json!({ "kind": kind, "offset": at, "unchanged": true }));
                    }
                    // Retention trimmed the stored text, so readers can no longer
                    // see it. Writing the current text may repeat an entry; skipping
                    // would leave the item without it.
                    TextRead::Gone => {
                        eprintln!(
                            "kbl-tracker: context {entry} on {item} at offset {at}: \
                             previous text trimmed; writing current text"
                        );
                    }
                    _ => {}
                }
            }
            // A failed check becomes fix work while the check has tries left
            // and no fix for it is still open; otherwise a person reads it.
            if let WorkRecord::CheckFailed {
                item,
                pr,
                check,
                fix,
                ..
            } = &mut record
            {
                let tries = core.state.fix_check_tries(item, pr, check);
                *fix = tries < usize::from(self.0.settings.auto_fix_tries)
                    && !core.state.fix_check_open(item, pr, check);
            }
            let opened = match &record {
                WorkRecord::PrStatus { item, status } if status.phase == PrPhase::Open => {
                    Some((item.clone(), status.pr.clone()))
                }
                _ => None,
            };
            let offset = self.append(&mut core, Actor::Tracker, by, record).await?;
            if let Some((root, pr)) = opened {
                self.open_reviews(&mut core, &root, &pr).await;
            }
            offset
        };
        self.sync_ready().await;
        Ok(json!({ "kind": kind, "offset": offset }))
    }

    /// When a request opens, the root gets a first-pass review and a final
    /// review of it, each for a reviewer tier, the final waiting for the first.
    /// Once per request: the ids come from the root and the request. Each piece
    /// is written only if it is missing, so a retry finishes a partial write.
    /// The tracker writes them itself, so ingest's token gains no new power.
    async fn open_reviews(&self, core: &mut super::Core, root: &ItemId, pr: &str) {
        let tiers = &self.0.settings.review_tiers;
        let (Some(first_tier), Some(final_tier)) = (tiers.first(), tiers.last()) else {
            return;
        };
        let id = |pass: &str| review_id(root, pr, pass);
        let (first, last) = (id("first"), id("final"));
        let plan = [
            (&first, ReviewPass::First, first_tier, "First-pass review"),
            (&last, ReviewPass::Final, final_tier, "Final review"),
        ];
        for (item, pass, tier, title) in plan {
            if core.state.items.contains_key(item) {
                continue;
            }
            let record = WorkRecord::Proposed {
                item: item.clone(),
                parent: Some(root.clone()),
                key: review_key(pr, pass),
                work: WorkKind::Review,
                title: format!("{title}: {pr}").chars().take(200).collect(),
                review: Some(ReviewSpec {
                    pass,
                    tier: tier.clone(),
                    pr: pr.to_owned(),
                }),
            };
            if let Err(e) = self
                .append(core, Actor::Tracker, self.0.me.clone(), record)
                .await
            {
                eprintln!("kbl-tracker: could not open the {title} for {pr}: {e}");
                return;
            }
        }
        if core.state.is_linked(&last, &first) {
            return;
        }
        let wait = WorkRecord::Linked {
            item: last,
            to: first,
        };
        if let Err(e) = self
            .append(core, Actor::Tracker, self.0.me.clone(), wait)
            .await
        {
            eprintln!("kbl-tracker: could not order the reviews of {pr}: {e}");
        }
    }

    /// Every imported root: what ingest needs to count a rule's open roots and
    /// to write back to the right pull request. Also the prerequisites ingest
    /// may open an upstream issue for. A worker tied to a source other than the
    /// pull request source sees only that source's; the pull request worker
    /// sees every root, to record a request's state on it.
    pub(crate) async fn ingest_roots(&self, tied: Option<&str>) -> Result<Value, Error> {
        let only = tied.filter(|s| *s != PULL_REQUEST_SOURCE);
        self.begin().await?;
        let mut fetched: HashMap<u64, TextRead> = HashMap::new();
        loop {
            let mut core = self.0.core.lock().await;
            let roots: Vec<Value> = core
                .state
                .imported()
                .filter(|(source, _, _)| only.is_none_or(|o| o == *source))
                .filter_map(|(source, upstream_id, item)| {
                    let it = core.state.items.get(item)?;
                    Some(json!({
                        "item": item,
                        "source": source,
                        "upstream_id": upstream_id,
                        "stage": core.state.stage(item),
                        "closed": it.closed.is_some(),
                        "closed_by_sync": it.closed_by_sync,
                        "closed_by_merge": it.closed == Some(kbl_core::work_fold::Closure::Merged),
                        "left_scope_by_sync": it.left_scope_by_sync,
                        "reopened_by": it.reopened_by,
                        "has_children": !it.children.is_empty(),
                        "held": it.hold.is_some(),
                        "pr": it.pr,
                        "claimed_at_ms": it.first_claim_ms,
                        "pr_status": it.pr_status,
                        "release": it.release,
                        "awaits_release": core.state.awaits_release(item),
                    }))
                })
                .collect();
            let prerequisite = Key::new("prerequisite").expect("a valid key");
            let unopened: Vec<_> = core
                .state
                .unopened_prerequisites()
                .into_iter()
                .filter_map(|(it, waiter)| {
                    let target = it.target.as_ref()?;
                    if only.is_some_and(|o| o != target.source.as_str()) {
                        return None;
                    }
                    let at = it.context.get(&prerequisite).copied();
                    Some((
                        it.id.clone(),
                        it.title.clone(),
                        target.clone(),
                        waiter.map(str::to_owned),
                        at,
                    ))
                })
                .collect();
            // Texts the cache let go are read with the lock dropped, then this is redone.
            let mut missing: Vec<u64> = Vec::new();
            let mut whys: HashMap<u64, Option<Content>> = HashMap::new();
            for at in unopened.iter().filter_map(|u| u.4) {
                if let Some(t) = Self::cached_text(&mut core, at) {
                    whys.insert(at, Some(t));
                } else {
                    match fetched.get(&at) {
                        Some(TextRead::Text(t)) => {
                            core.recall.texts.insert(at, t.clone());
                            whys.insert(at, Some(t.clone()));
                        }
                        Some(TextRead::NoText | TextRead::Gone) => {
                            whys.insert(at, None);
                        }
                        None => missing.push(at),
                    }
                }
            }
            if !missing.is_empty() {
                drop(core);
                fetched.extend(self.read_texts(&missing).await?);
                continue;
            }
            let mut prerequisites: Vec<Value> = Vec::new();
            for (id, title, target, waiter, at) in unopened {
                // The `why` of the record that created it, when it is inline text.
                let why = match at.and_then(|a| whys.get(&a).cloned().flatten()) {
                    Some(Content::Text(t)) => Some(t),
                    _ => None,
                };
                prerequisites.push(json!({
                    "item": id,
                    "title": title,
                    "source": target.source,
                    "repo": target.repo,
                    "why": why,
                    "needed_by": waiter,
                }));
            }
            let flakes = flake_list(&core.state);
            return Ok(json!({
                "roots": roots,
                "prerequisites": prerequisites,
                "flakes": flakes,
            }));
        }
    }

    /// A source's ingest lease and the version to write it back at.
    pub(crate) async fn source_lease(
        &self,
        source: &str,
    ) -> Result<Option<(kbl_core::lease::Lease, u64)>, Error> {
        match self.0.broker.source_lease_get(source).await {
            Ok(Some((bytes, version))) => serde_json::from_slice(&bytes)
                .map(|l| Some((l, version)))
                .map_err(|e| Error::Felix(anyhow::anyhow!("a source lease is not JSON: {e}"))),
            Ok(None) => Ok(None),
            Err(e) => Err(Error::Felix(e)),
        }
    }

    /// Writes a source's lease if nobody wrote it since `version`.
    pub(crate) async fn put_source_lease(
        &self,
        source: &str,
        lease: &kbl_core::lease::Lease,
        version: Option<u64>,
    ) -> Result<bool, Error> {
        let bytes = serde_json::to_vec(lease).map_err(|_| refused("invalid"))?;
        match self.0.broker.source_lease_put(source, bytes, version).await {
            Ok(put) => Ok(put.is_some()),
            Err(e) => Err(Error::Felix(e)),
        }
    }

    /// A worker group's membership entry and the version to write it back at.
    pub(crate) async fn group_members<T: serde::de::DeserializeOwned>(
        &self,
        group: &Key,
    ) -> Result<Option<(T, u64)>, Error> {
        match self.0.broker.group_members_get(group.as_str()).await {
            Ok(Some((bytes, version))) => serde_json::from_slice(&bytes)
                .map(|m| Some((m, version)))
                .map_err(|e| Error::Felix(anyhow::anyhow!("a group's members are not JSON: {e}"))),
            Ok(None) => Ok(None),
            Err(e) => Err(Error::Felix(e)),
        }
    }

    /// Writes a group's members if nobody wrote them since `version`.
    pub(crate) async fn put_group_members<T: serde::Serialize>(
        &self,
        group: &Key,
        members: &T,
        version: Option<u64>,
    ) -> Result<bool, Error> {
        let bytes = serde_json::to_vec(members).map_err(|_| refused("invalid"))?;
        match self
            .0
            .broker
            .group_members_put(group.as_str(), bytes, version)
            .await
        {
            Ok(put) => Ok(put.is_some()),
            Err(e) => Err(Error::Felix(e)),
        }
    }

    pub(crate) async fn ingest_state(&self, source: &str) -> Result<Value, Error> {
        let name = state_name(source)?;
        match self.0.broker.cursor_get(&name).await {
            Ok(Some((bytes, version))) => {
                let state: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| Error::Felix(anyhow::anyhow!("saved state is not JSON: {e}")))?;
                Ok(json!({ "state": state, "version": version }))
            }
            Ok(None) => Ok(json!({ "state": null, "version": null })),
            Err(e) => Err(Error::Felix(e)),
        }
    }

    /// Saves ingest's state if nobody saved since `version`.
    pub(crate) async fn ingest_save(
        &self,
        source: &str,
        state: &Value,
        version: Option<u64>,
    ) -> Result<Value, Error> {
        let name = state_name(source)?;
        let bytes = serde_json::to_vec(state).map_err(|_| refused("invalid"))?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(refused("too_big"));
        }
        match self.0.broker.cursor_put(&name, bytes, version).await {
            Ok(Some(version)) => Ok(json!({ "version": version })),
            Ok(None) => Err(refused("stale_state")),
            Err(e) => Err(Error::Felix(e)),
        }
    }
}

/// Whether the item's reported request has a merged status. Ingest may close
/// as merged, or record a release, on no other evidence.
fn merged_request_reported(state: &State, item: &ItemId) -> bool {
    state.items.get(item).is_some_and(|it| {
        it.pr_status
            .as_ref()
            .is_some_and(|s| s.phase == PrPhase::Merged && it.pr.as_deref() == Some(s.pr.as_str()))
    })
}

fn state_name(source: &str) -> Result<String, Error> {
    Key::new(source)
        .map(|k| k.as_str().to_owned())
        .map_err(|_| refused("invalid"))
}
