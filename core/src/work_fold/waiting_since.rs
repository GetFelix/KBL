//! When each item entered Needs you for the reason it is there now, as a log
//! offset, so the inbox sorts oldest first the same way after a reload. It sits
//! on the item and in the state's wire form, so a snapshot keeps it.

use serde::{Deserialize, Serialize};

use super::{NeedsYou, Stage, State};
use crate::names::ItemId;
use crate::work::WorkLogged;

/// A reason an item waits on a person and the offset of the record that began it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingSince {
    /// Set for a reason from Needs you; otherwise `stage` says a proposal waits.
    pub needs: Option<NeedsYou>,
    pub stage: Option<Stage>,
    /// The open question's offset: a new question is a new wait.
    pub question: Option<u64>,
    pub at: u64,
}

impl State {
    /// The reason `id` waits on a person in the inbox, without its start.
    fn inbox_wait(&self, id: &ItemId) -> Option<WaitingSince> {
        let it = self.items.get(id)?;
        let needs = self.needs_you(id);
        let stage = if needs.is_some() {
            None
        } else {
            match self.stage(id)? {
                Stage::Proposed => Some(Stage::Proposed),
                Stage::Triage if it.proposal.is_some() => Some(Stage::Triage),
                _ => return None,
            }
        };
        Some(WaitingSince {
            needs,
            stage,
            question: it.open_question,
            at: 0,
        })
    }

    /// Runs after each accepted record, on the item it names and on what a close frees.
    pub(super) fn track_waiting_since(&mut self, offset: u64, rec: &WorkLogged) {
        let Some(id) = rec.record.item().cloned() else {
            return;
        };
        self.refresh_waiting_since(&id, offset);
        if self.items.get(&id).is_some_and(|i| i.closed.is_some()) {
            let dependents: Vec<ItemId> = self
                .edges
                .iter()
                .filter(|(_, b)| *b == id)
                .map(|(a, _)| a.clone())
                .collect();
            for a in dependents {
                self.refresh_waiting_since(&a, offset);
            }
        }
    }

    fn refresh_waiting_since(&mut self, id: &ItemId, offset: u64) {
        let now = self.inbox_wait(id);
        let Some(it) = self.items.get_mut(id) else {
            return;
        };
        let same = |a: &WaitingSince, b: &WaitingSince| {
            (a.needs, a.stage, a.question) == (b.needs, b.stage, b.question)
        };
        it.waiting_since = match (it.waiting_since, now) {
            (Some(old), Some(new)) if same(&old, &new) => Some(old),
            (_, Some(new)) => Some(WaitingSince { at: offset, ..new }),
            (_, None) => None,
        };
    }
}
