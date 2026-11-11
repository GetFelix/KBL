//! What the ingest worker may do through the tracker. It writes only the
//! records upstream sync needs, as the tracker's role under its own name, and
//! keeps its cursor in Felix so a restart does not import again.

use agents_core::artifact::INLINE_LIMIT;
use agents_core::content::Content;
use agents_core::names::{ItemId, Key};
use agents_core::work::{Actor, CloseWhy, PrPhase, ReviewPass, ReviewSpec, WorkKind, WorkRecord};
use agents_core::work_fold::State;
use serde_json::{Value, json};

use super::{Error, Tracker};
use crate::felix::Broker;

/// Longest saved state: a cache value, not a log.
const MAX_STATE_BYTES: usize = 4 << 20;

/// Whether ingest may write this record. Budgets, claims, answers and the
/// records a person writes are not among them.
fn allowed(record: &WorkRecord) -> bool {
    match record {
        WorkRecord::Imported { .. }
        | WorkRecord::Accepted { .. }
        | WorkRecord::Context { .. }
        | WorkRecord::Linked { .. }
        | WorkRecord::Updated { .. }
        | WorkRecord::LeftScope { .. }
        | WorkRecord::PrStatus { .. }
        | WorkRecord::SourceStalled { .. }
        | WorkRecord::Reopened { .. } => true,
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
        | WorkRecord::Context { item, .. }
        | WorkRecord::Updated { item, .. }
        | WorkRecord::LeftScope { item }
        | WorkRecord::PrStatus { item, .. }
        | WorkRecord::Reopened { item }
        | WorkRecord::Closed { item, .. } => vec![item],
        WorkRecord::Linked { item, to } => vec![item, to],
        WorkRecord::Proposed {
            parent: Some(parent),
            ..
        } => vec![parent],
        _ => Vec::new(),
    }
}

/// The id of a root's review of a request: the same on every retry.
fn review_id(root: &ItemId, pr: &str, pass: &str) -> ItemId {
    let hash =
        agents_core::artifact::ArtifactId::of(format!("review\n{root}\n{pr}\n{pass}").as_bytes());
    ItemId::new(format!("w{}", &hash.as_str()[2..22])).expect("hash characters are valid")
}

fn review_key(pr: &str, pass: ReviewPass) -> Key {
    let hash = agents_core::artifact::ArtifactId::of(format!("{pr}\n{pass:?}").as_bytes());
    Key::new(format!("review-{}", &hash.as_str()[2..18])).expect("hash characters are valid")
}

fn refused(why: &str) -> Error {
    Error::Refused { why: why.into() }
}

impl<B: Broker> Tracker<B> {
    /// Appends an upstream sync record if the fold accepts it.
    pub(crate) async fn ingest_record(&self, by: Key, record: WorkRecord) -> Result<Value, Error> {
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
        self.begin().await?;
        let kind = record.kind();
        let offset = {
            let mut core = self.0.core.lock().await;
            if !targets(&record)
                .into_iter()
                .all(|t| is_imported(&core.state, t))
            {
                return Err(refused("not_imported"));
            }
            if let WorkRecord::Closed {
                item,
                why: CloseWhy::Merged,
            } = &record
                && !merged_request_reported(&core.state, item)
            {
                return Err(refused("not_merged"));
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
                eprintln!("agents-tracker: could not open the {title} for {pr}: {e}");
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
            eprintln!("agents-tracker: could not order the reviews of {pr}: {e}");
        }
    }

    /// Every imported root: what ingest needs to count a rule's open roots and
    /// to write back to the right pull request.
    pub(crate) async fn ingest_roots(&self) -> Result<Value, Error> {
        self.begin().await?;
        let core = self.0.core.lock().await;
        let roots: Vec<Value> = core
            .state
            .imported()
            .filter_map(|(source, upstream_id, item)| {
                let it = core.state.items.get(item)?;
                Some(json!({
                    "item": item,
                    "source": source,
                    "upstream_id": upstream_id,
                    "stage": core.state.stage(item),
                    "closed": it.closed.is_some(),
                    "held": it.hold.is_some(),
                    "pr": it.pr,
                    "claimed_at_ms": it.first_claim_ms,
                    "pr_status": it.pr_status,
                }))
            })
            .collect();
        Ok(json!({ "roots": roots }))
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
/// as merged on no other evidence.
fn merged_request_reported(state: &State, item: &ItemId) -> bool {
    state.items.get(item).is_some_and(|it| {
        it.pr_status
            .as_ref()
            .is_some_and(|s| s.phase == PrPhase::Merged && it.pr.as_deref() == Some(s.pr.as_str()))
    })
}

fn is_imported(state: &State, item: &ItemId) -> bool {
    state.imported().any(|(_, _, i)| i == item)
}

fn state_name(source: &str) -> Result<String, Error> {
    Key::new(source)
        .map(|k| k.as_str().to_owned())
        .map_err(|_| refused("invalid"))
}
