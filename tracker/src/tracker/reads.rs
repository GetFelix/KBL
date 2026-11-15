//! What a person reads through the tracker: the items as the fold sees them,
//! and the records about one item.

use kbl_core::names::ItemId;
use kbl_core::work::{WORK_EVENTS, WorkRecord, read};
use kbl_core::work_fold::Stage;
use serde_json::{Value, json};

use super::{Error, Tracker};
use crate::felix::Broker;

/// Most records one history answer carries. `next` names where to go on.
pub(crate) const HISTORY_PAGE: usize = 500;

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
                    "closed": i.closed,
                })
            })
            .collect();
        Ok(json!({ "items": items, "at": s.at }))
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
    /// `from`: those that name it, and links that point at it. A record the
    /// fold refused on replay carries `rejected`.
    pub(crate) async fn history(&self, id: &ItemId, from: u64) -> Result<Value, Error> {
        self.begin().await?;
        if !self.0.core.lock().await.state.items.contains_key(id) {
            return Err(unknown_item());
        }
        let log = self
            .io(None, self.0.broker.read_from(WORK_EVENTS, from))
            .await
            .map_err(Error::Felix)?;
        let rejected: std::collections::HashMap<u64, _> = {
            let core = self.0.core.lock().await;
            core.state
                .rejections
                .iter()
                .map(|r| (r.offset, r.why))
                .collect()
        };
        let mut records = Vec::new();
        let mut next = None;
        for (offset, payload) in log.into_iter().filter(|(o, _)| *o >= from) {
            // A record this build cannot read is skipped here as on replay;
            // the fold that serves the item stopped at it already.
            let Ok(rec) = read(&payload) else { continue };
            let about = rec.record.item() == Some(id)
                || matches!(&rec.record,
                    WorkRecord::Linked { to, .. } | WorkRecord::Unlinked { to, .. } if to == id);
            if !about {
                continue;
            }
            if records.len() == HISTORY_PAGE {
                next = Some(offset);
                break;
            }
            let mut v = json!({ "offset": offset, "record": rec });
            if let Some(why) = rejected.get(&offset) {
                v["rejected"] = json!(why);
            }
            records.push(v);
        }
        Ok(json!({ "records": records, "next": next }))
    }
}

fn unknown_item() -> Error {
    Error::Refused {
        why: "unknown_item".into(),
    }
}
