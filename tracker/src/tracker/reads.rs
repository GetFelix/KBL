//! What a person reads through the tracker: the items as the fold sees them,
//! and the records about one item.

use std::collections::HashMap;

use kbl_core::names::{ItemId, Key};
use kbl_core::work::{WORK_EVENTS, WorkRecord, read};
use kbl_core::work_fold::Stage;
use serde_json::{Value, json};

use super::{Error, Tracker};
use crate::felix::Broker;

/// Most records one history answer carries. `next` names where to go on.
pub(crate) const HISTORY_PAGE: usize = 500;

/// The widest window the metrics reads take, in days.
pub(crate) const MAX_WINDOW_DAYS: u64 = 366;

const DAY_MS: u64 = 86_400_000;

/// Most records one history answer reads, matching or not.
pub(crate) const HISTORY_SCAN: usize = 20_000;

impl<B: Broker> Tracker<B> {
    /// Every item, or those in `stage`, in id order.
    pub(crate) async fn items(&self, stage: Option<Stage>) -> Result<Value, Error> {
        self.begin().await?;
        let core = self.0.core.lock().await;
        let s = &core.state;
        let items: Vec<Value> = s
            .items
            .values()
            .filter(|i| stage.is_none() || s.stage(&i.id) == stage)
            .map(|i| {
                json!({
                    "item": i.id,
                    "title": i.title,
                    "work": i.work,
                    "stage": s.stage(&i.id),
                    "needs": s.needs_you(&i.id),
                    "tier": i.tier,
                    "parent": i.parent,
                    "root": i.root,
                    "held_by": i.hold.as_ref().map(|h| &h.by),
                    "flagged": i.flagged,
                    "assignee": i.assignee,
                    "closed": i.closed,
                })
            })
            .collect();
        Ok(json!({ "items": items, "at": s.at }))
    }

    /// What waits on a person, in the order My queue shows it: unknown start
    /// first (it began before this tracker's snapshot), then longest wait,
    /// then lower tier. `viewer`'s list: an item assigned to another person
    /// is left out. `count` is the whole queue; `queue` holds its first
    /// `limit` entries.
    pub(crate) async fn queue(&self, viewer: &Key, limit: usize) -> Result<Value, Error> {
        self.begin().await?;
        let core = self.0.core.lock().await;
        let s = &core.state;
        let all = s.queue_for(super::now_ms(), Some(viewer));
        let count = all.len();
        let entries: Vec<Value> = all
            .into_iter()
            .take(limit)
            .map(|e| {
                let it = &s.items[&e.item];
                json!({
                    "item": e.item,
                    "title": it.title,
                    "work": it.work,
                    "stage": s.stage(&e.item),
                    "needs": s.needs_you(&e.item),
                    "wait": e.kind,
                    "tier": e.tier,
                    "since_ms": e.since_ms,
                    "root": it.root,
                    "assignee": e.assignee,
                })
            })
            .collect();
        let at = s.at;
        drop(core);
        Ok(json!({ "queue": entries, "count": count, "at": at }))
    }

    /// Time to first human action over the last `days` days, team level:
    /// count, p50 and p90 of the waits a person ended, and the waits still open.
    pub(crate) async fn time_to_human_action(&self, days: u64) -> Result<Value, Error> {
        self.begin().await?;
        let core = self.0.core.lock().await;
        let to = super::now_ms();
        let from = to.saturating_sub(days.saturating_mul(DAY_MS));
        let stats = core.state.time_to_human_action(from, to);
        Ok(json!({ "days": days, "stats": stats, "at": core.state.at }))
    }

    /// One item as the fold holds it, with its stage and why it needs a person.
    pub(crate) async fn item(&self, id: &ItemId) -> Result<Value, Error> {
        self.begin().await?;
        let core = self.0.core.lock().await;
        let s = &core.state;
        let it = s.items.get(id).ok_or_else(unknown_item)?;
        let waits_on: Vec<&ItemId> = s
            .edges
            .iter()
            .filter(|(a, _)| a == id)
            .map(|(_, b)| b)
            .collect();
        Ok(json!({
            "item": it,
            "stage": s.stage(id),
            "needs": s.needs_you(id),
            "waits_on": waits_on,
            "at": s.at,
        }))
    }

    /// The records on `work.events` about `id`, oldest first, from offset
    /// `from`: those that name it, and records that point at it (links,
    /// prerequisites, a triage answer that calls it a duplicate). A record
    /// the fold refused on replay carries `rejected`. `next` is where to go
    /// on: after [`HISTORY_PAGE`] records, or after [`HISTORY_SCAN`] scanned.
    pub(crate) async fn history(&self, id: &ItemId, from: u64) -> Result<Value, Error> {
        self.history_capped(id, from, HISTORY_PAGE, HISTORY_SCAN)
            .await
    }

    async fn history_capped(
        &self,
        id: &ItemId,
        from: u64,
        most: usize,
        scan: usize,
    ) -> Result<Value, Error> {
        self.begin().await?;
        let (rejected, at): (HashMap<u64, _>, _) = {
            let core = self.0.core.lock().await;
            if !core.state.items.contains_key(id) {
                return Err(unknown_item());
            }
            let rejected = core.state.rejections.iter().map(|r| (r.offset, r.why));
            (rejected.collect(), core.state.at)
        };
        let mut records = Vec::new();
        let mut scanned = 0;
        let mut cursor = Some(from);
        let mut next = None;
        // Read page by page, so a long log costs one bounded answer at a time.
        'pages: while let Some(at_offset) = cursor {
            let (page, after) = self
                .io(None, self.0.broker.read_page(WORK_EVENTS, at_offset))
                .await
                .map_err(Error::Felix)?;
            for (offset, payload) in page.into_iter().filter(|(o, _)| *o >= from) {
                if records.len() == most || scanned == scan {
                    next = Some(offset);
                    break 'pages;
                }
                scanned += 1;
                // Malformed and newer records are both left out; the state
                // the other calls serve stopped at a newer one already.
                let Ok(rec) = read(&payload) else { continue };
                if !about(&rec.record, id) {
                    continue;
                }
                let mut v = json!({ "offset": offset, "record": rec });
                if let Some(why) = rejected.get(&offset) {
                    v["rejected"] = json!(why);
                }
                records.push(v);
            }
            cursor = after;
        }
        Ok(json!({ "records": records, "next": next, "at": at }))
    }
}

/// Whether `record` is about `id`: it names it, or points at it.
fn about(record: &WorkRecord, id: &ItemId) -> bool {
    record.item() == Some(id)
        || match record {
            WorkRecord::Linked { to, .. } | WorkRecord::Unlinked { to, .. } => to == id,
            WorkRecord::Prerequisite {
                prerequisite,
                triage,
                ..
            } => prerequisite == id || triage.as_ref() == Some(id),
            WorkRecord::Triaged { duplicate_of, .. } => duplicate_of.as_ref() == Some(id),
            _ => false,
        }
}

fn unknown_item() -> Error {
    Error::Refused {
        why: "unknown_item".into(),
    }
}

#[cfg(test)]
mod tests;
