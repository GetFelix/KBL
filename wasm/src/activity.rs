//! What happened on each item, in log order, for the panel's timeline and attempt history.
//!
//! Fold-only: the page reads these from records it already applies, and nothing here is
//! written to the log. A step's output keeps only its tail, so a long build log does not
//! sit in memory for every item.

use std::collections::BTreeMap;

use kbl_core::content::Content;
use kbl_core::names::ItemId;
use kbl_core::work::{WorkLogged, WorkRecord};
use kbl_core::work_fold::Reject;
use serde_json::{Value, json};

/// Events kept per item; the oldest go first.
const MAX_EVENTS: usize = 500;
/// Bytes of a step's output kept, from the end.
const OUTPUT_TAIL: usize = 800;

#[derive(Default)]
pub(crate) struct Activity {
    by_item: BTreeMap<ItemId, Vec<Value>>,
}

fn text(c: &Content) -> Value {
    match c {
        Content::Text(t) => json!(t),
        Content::Artifact(_) => Value::Null,
    }
}

/// The last `OUTPUT_TAIL` bytes of `s`, cut on a character boundary.
fn tail(s: &str) -> &str {
    if s.len() <= OUTPUT_TAIL {
        return s;
    }
    let mut from = s.len() - OUTPUT_TAIL;
    while !s.is_char_boundary(from) {
        from += 1;
    }
    &s[from..]
}

impl Activity {
    /// Notes `rec` at `offset`. `charged` is what the fold charged the item for it, and
    /// `refused` is why the fold took nothing from it.
    pub(crate) fn note(
        &mut self,
        offset: u64,
        rec: &WorkLogged,
        charged: u64,
        refused: Option<Reject>,
    ) {
        let Some(item) = rec.record.item() else {
            return;
        };
        let by = rec.by.as_str();
        let e = |kind: &str, attempt: Option<u32>, more: Value| {
            let mut v = json!({ "offset": offset, "at_ms": rec.at_ms, "by": by, "kind": kind });
            if let Some(a) = attempt {
                v["attempt"] = json!(a);
            }
            if charged > 0 {
                v["cost_micros"] = json!(charged);
            }
            if let (Some(m), Some(extra)) = (v.as_object_mut(), more.as_object()) {
                m.extend(extra.clone());
            }
            Some(v)
        };
        let event = match &rec.record {
            WorkRecord::Claimed {
                attempt,
                deadline_ms,
                ..
            } => e(
                "claimed",
                Some(*attempt),
                json!({ "deadline_ms": deadline_ms }),
            ),
            WorkRecord::Progress { attempt, note, .. } => {
                e("progress", Some(*attempt), json!({ "text": text(note) }))
            }
            WorkRecord::Step {
                attempt,
                command,
                exit_code,
                output,
                ..
            } => {
                let out = match output {
                    Content::Text(t) => json!(tail(t)),
                    Content::Artifact(_) => Value::Null,
                };
                e(
                    "step",
                    Some(*attempt),
                    json!({ "command": command, "exit_code": exit_code, "output": out }),
                )
            }
            WorkRecord::Edit { attempt, file, .. } => {
                e("edit", Some(*attempt), json!({ "file": file }))
            }
            WorkRecord::Decision {
                attempt,
                phase,
                chosen,
                why,
                ..
            } => e(
                "decision",
                Some(*attempt),
                json!({ "phase": phase, "chosen": chosen, "why": why }),
            ),
            WorkRecord::Question {
                attempt,
                text: t,
                options,
                ..
            } => e(
                "question",
                Some(*attempt),
                json!({ "text": text(t), "options": options }),
            ),
            WorkRecord::Answer {
                question,
                text: t,
                chosen,
                ..
            } => e(
                "answer",
                None,
                json!({ "question": question, "text": text(t), "chosen": chosen }),
            ),
            WorkRecord::HumanInput {
                attempt, text: t, ..
            } => e("note", *attempt, json!({ "text": text(t) })),
            WorkRecord::Steered {
                attempt, text: t, ..
            } => e(
                "note",
                Some(*attempt),
                json!({ "text": text(t), "steered": true }),
            ),
            WorkRecord::Done { attempt, .. } => e("done", Some(*attempt), json!({})),
            WorkRecord::Released {
                attempt,
                reason,
                why,
                ..
            } => e(
                "released",
                Some(*attempt),
                json!({ "reason": reason, "why": why }),
            ),
            WorkRecord::GaveUp {
                attempt, reason, ..
            } => e("gave_up", Some(*attempt), json!({ "reason": reason })),
            WorkRecord::AttemptClosed { attempt, .. } => {
                e("attempt_closed", Some(*attempt), json!({}))
            }
            WorkRecord::SummaryRated {
                attempt, accurate, ..
            } => e("rated", Some(*attempt), json!({ "accurate": accurate })),
            WorkRecord::Accepted { tier, .. } => e("accepted", None, json!({ "tier": tier })),
            WorkRecord::TierChanged { tier, .. } => e("tier", None, json!({ "tier": tier })),
            WorkRecord::Taken { .. } => e("taken", None, json!({})),
            WorkRecord::Reassigned { to, .. } => e("reassigned", None, json!({ "to": to })),
            WorkRecord::Closed { why, .. } => e("closed", None, json!({ "why": why })),
            WorkRecord::Reopened { .. } => e("reopened", None, json!({})),
            _ => None,
        };
        if let Some(mut v) = event {
            if let Some(why) = refused {
                v["refused"] = serde_json::to_value(why).expect("plain data serializes");
            }
            let list = self.by_item.entry(item.clone()).or_default();
            list.push(v);
            if list.len() > MAX_EVENTS {
                list.remove(0);
            }
        }
    }

    /// The item's events as a JSON array, oldest first.
    pub(crate) fn json(&self, item: &ItemId) -> String {
        serde_json::to_string(self.by_item.get(item).map_or(&[][..], Vec::as_slice))
            .expect("plain data serializes")
    }
}
