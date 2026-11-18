//! Which ready item `work_next` hands out when a caller filters or several
//! roots have work in the same tier, and the read-only peek at that order.
//!
//! A claim chosen here is a direct claim, like an assigned item's: it is made
//! from the state and touches no ready record, so a record for an item the
//! caller filtered out is never polled, acked or nacked. The record of the item
//! that was claimed stays in its lane and is acked as stale when polled.
//! See docs/work-tracker.md.

use kbl_core::work::{Assignee, WorkKind};
use kbl_core::work_fold::{Item, Stage};
use serde_json::{Value, json};

use super::*;

/// Most items one peek returns.
pub(crate) const MAX_PEEK: usize = 50;

/// What a caller narrows `work_next` to. Every field left empty matches all.
#[derive(Debug, Default, Clone)]
pub(crate) struct Filter {
    pub(crate) kinds: Vec<WorkKind>,
    /// Priority tiers, 0 is the highest.
    pub(crate) tiers: Vec<u8>,
    /// The repo the work lands in, as `owner/name`, without regard to case.
    pub(crate) repo: Option<String>,
    pub(crate) exclude_roots: Vec<ItemId>,
}

impl Filter {
    pub(crate) fn is_empty(&self) -> bool {
        self.kinds.is_empty()
            && self.tiers.is_empty()
            && self.repo.is_none()
            && self.exclude_roots.is_empty()
    }

    /// Reads the wire form, naming the first value that means nothing.
    pub(crate) fn parse(
        kinds: &[String],
        tiers: &[u8],
        repo: Option<&str>,
        exclude_roots: &[ItemId],
    ) -> Result<Self, Error> {
        let mut parsed = Vec::new();
        for k in kinds {
            let kind: WorkKind = serde_json::from_value(json!(k)).map_err(|_| {
                Error::Invalid(format!(
                    "{k:?} is not a kind of work; use triage, augment, fix, split, question, test, resume or review"
                ))
            })?;
            parsed.push(kind);
        }
        if let Some(t) = tiers.iter().find(|t| **t >= TIERS) {
            return Err(Error::Invalid(format!(
                "tier {t} does not exist; tiers run from 0 to {}",
                TIERS - 1
            )));
        }
        if exclude_roots.len() > MAX_EXCLUDED {
            return Err(Error::Invalid(format!(
                "exclude at most {MAX_EXCLUDED} roots"
            )));
        }
        Ok(Self {
            kinds: parsed,
            tiers: tiers.to_vec(),
            repo: repo.map(str::to_owned),
            exclude_roots: exclude_roots.to_vec(),
        })
    }

    pub(super) fn matches(&self, core: &Core, it: &Item) -> bool {
        (self.kinds.is_empty() || self.kinds.contains(&it.work))
            && (self.tiers.is_empty()
                || self
                    .tiers
                    .contains(&it.tier.unwrap_or(DEFAULT_TIER).min(TIERS - 1)))
            && !self.exclude_roots.contains(&it.root)
            && self.repo.as_ref().is_none_or(|want| {
                core.state
                    .repo_of(&it.id)
                    .is_some_and(|t| t.repo.eq_ignore_ascii_case(want))
            })
    }
}

/// Most roots a caller may exclude in one call.
const MAX_EXCLUDED: usize = 64;

/// A ready item this agent could claim, with what orders it.
#[derive(Clone)]
pub(super) struct Candidate {
    pub(super) item: ItemId,
    pub(super) root: ItemId,
    pub(super) lane: u8,
    /// Work an answer started: it waits behind other roots' ready work.
    follow_up: bool,
    ready_for: Duration,
}

/// Whether a person's answer made this item ready: a resume item, or an item
/// whose asker let go and that carries the question and its answer.
fn is_follow_up(it: &Item) -> bool {
    let has = |name: &str| Key::new(name).is_ok_and(|k| it.context.contains_key(&k));
    it.work == WorkKind::Resume || (has("question") && has("answer"))
}

impl<B: Broker> Tracker<B> {
    /// The ready items `agent` may claim from the lanes, matching `filter`, in no
    /// order. An item the agent may not take without the filter is not here:
    /// its assignee's, its own review, one the fold refused at this log position.
    pub(super) fn candidates(
        &self,
        core: &Core,
        agent: &AgentName,
        review: &[u8],
        filter: &Filter,
    ) -> Vec<Candidate> {
        let key = agent_key(agent);
        let now = Instant::now();
        let at_ms = now_ms();
        core.state
            .items
            .iter()
            .filter(|(id, _)| core.state.stage(id) == Some(Stage::Ready))
            .filter(|(id, _)| !core.state.blocked_at(id, at_ms))
            .filter(|(id, _)| core.refused.get(*id) != Some(&core.state.at))
            .filter(|(_, it)| !matches!(it.assignee, Some(Assignee::Agent(_))))
            .filter(|(id, _)| !self.review_full(&core.state, id))
            .filter_map(|(id, it)| {
                let lane = self.0.settings.lane(it)?;
                if lane >= TIERS && !review.contains(&lane) {
                    return None;
                }
                if lane >= TIERS && core.state.worked_on(&it.root, &key) {
                    return None;
                }
                filter.matches(core, it).then(|| Candidate {
                    item: id.clone(),
                    root: it.root.clone(),
                    lane,
                    follow_up: is_follow_up(it),
                    ready_for: core
                        .published
                        .get(id)
                        .map_or(Duration::ZERO, |(at, _)| now.duration_since(*at)),
                })
            })
            .collect()
    }

    /// `pool` in the order they are handed out: by lane, then round-robin across
    /// roots, the root served longest ago first, oldest ready item first within a
    /// root. Follow-ups of an answer go after other roots' ordinary work in the
    /// lane until they have been ready for one visibility period.
    pub(super) fn ordered(&self, core: &Core, mut pool: Vec<Candidate>) -> Vec<Candidate> {
        let patience = self.0.settings.visibility;
        let mut served = core.served.clone();
        let mut next = core.served_count;
        let mut out = Vec::with_capacity(pool.len());
        while !pool.is_empty() {
            let lane = pool.iter().map(|c| c.lane).min().unwrap_or(0);
            let behind = |c: &Candidate| {
                c.follow_up
                    && c.ready_for < patience
                    && pool
                        .iter()
                        .any(|o| o.lane == lane && !o.follow_up && o.root != c.root)
            };
            let Some(i) = pool
                .iter()
                .enumerate()
                .filter(|(_, c)| c.lane == lane)
                .min_by(|(_, a), (_, b)| {
                    let rank = |c: &Candidate| {
                        (
                            behind(c),
                            served.get(&c.root).copied().unwrap_or(0),
                            std::cmp::Reverse(c.ready_for),
                        )
                    };
                    rank(a).cmp(&rank(b)).then_with(|| a.item.cmp(&b.item))
                })
                .map(|(i, _)| i)
            else {
                break;
            };
            let c = pool.swap_remove(i);
            next += 1;
            served.insert(c.root.clone(), next);
            out.push(c);
        }
        out
    }

    /// Whether the lane's ready items for this agent come from more than one root,
    /// the case where the order of the lane's records is not the fair one.
    pub(super) fn several_roots(pool: &[Candidate], lane: u8) -> bool {
        let mut roots = pool.iter().filter(|c| c.lane == lane).map(|c| &c.root);
        roots.next().is_some_and(|first| roots.any(|r| r != first))
    }

    /// Notes that `item`'s root was just served, for the round-robin.
    pub(super) fn served(core: &mut Core, item: &ItemId) {
        let Some(root) = core.state.items.get(item).map(|i| i.root.clone()) else {
            return;
        };
        core.served_count += 1;
        let at = core.served_count;
        if core.served.len() > 4096 {
            let state = &core.state;
            core.served.retain(|r, _| state.items.contains_key(r));
        }
        core.served.insert(root, at);
    }

    /// The next `limit` items `agent` would be handed, in order, and nothing
    /// claimed. Items assigned to this agent come first, as in a claim.
    pub(crate) async fn peek(
        &self,
        agent: &AgentName,
        limit: usize,
        reviews: &[String],
        filter: &Filter,
    ) -> Result<Value, Error> {
        let lanes = self.review_lanes(reviews)?;
        self.begin().await?;
        self.sync_ready().await;
        let core = self.0.core.lock().await;
        if core.revoked.contains(agent) {
            return Err(Error::Refused {
                why: "agent_revoked".into(),
            });
        }
        let key = agent_key(agent);
        let mut mine: Vec<(u8, &ItemId)> = core
            .state
            .items
            .iter()
            .filter(|(_, it)| it.assignee == Some(Assignee::Agent(key.clone())))
            .filter(|(id, _)| core.state.stage(id) == Some(Stage::Ready))
            .filter(|(id, _)| !core.state.blocked_at(id, now_ms()))
            .filter(|(id, _)| core.refused.get(*id) != Some(&core.state.at))
            .filter(|(id, _)| !self.review_full(&core.state, id))
            .filter(|(_, it)| filter.matches(&core, it))
            .map(|(id, it)| (self.0.settings.lane(it).unwrap_or(DEFAULT_TIER), id))
            .collect();
        mine.sort();
        let limit = limit.clamp(1, MAX_PEEK);
        let pool = self.candidates(&core, agent, &lanes, filter);
        let list: Vec<Value> = mine
            .into_iter()
            .map(|(_, id)| (id.clone(), true))
            .chain(
                self.ordered(&core, pool)
                    .into_iter()
                    .map(|c| (c.item, false)),
            )
            .take(limit)
            .enumerate()
            .filter_map(|(n, (id, assigned))| {
                let it = core.state.items.get(&id)?;
                Some(json!({
                    "position": n + 1,
                    "item": id,
                    "title": it.title,
                    "work": it.work,
                    "tier": it.tier.unwrap_or(DEFAULT_TIER).min(TIERS - 1),
                    "root": it.root,
                    "repo": core.state.repo_of(&id).map(|t| &t.repo),
                    "assigned_to_you": assigned,
                }))
            })
            .collect();
        Ok(json!({
            "next": list,
            "note": "Nothing is claimed. work_next may hand out a different item if the work changes first.",
        }))
    }
}
