//! What happened on each item, in log order, for the panel's timeline and attempt history.
//!
//! Fold-only: the page reads these from records it already applies, and nothing here is
//! written to the log. A step's output keeps only its tail, so a long build log does not
//! sit in memory for every item.

use std::collections::{BTreeMap, BTreeSet};

use kbl_core::content::Content;
use kbl_core::names::ItemId;
use kbl_core::work::{Actor, WorkLogged, WorkRecord};
use kbl_core::work_fold::Reject;
use serde_json::{Value, json};

/// Events kept per item; the oldest go first.
const MAX_EVENTS: usize = 500;
/// Bytes of a step's output kept, from the end.
const OUTPUT_TAIL: usize = 800;

/// Failed steps kept per attempt; the rest are only counted.
const MAX_FAILED: usize = 10;
/// Files counted per attempt; edits past this still add to the line totals.
const MAX_FILES: usize = 500;

/// What one attempt handed off, from the records the fold accepted.
#[derive(Default)]
struct Hand {
    summary: Option<String>,
    files: BTreeSet<String>,
    added: u64,
    removed: u64,
    /// An edit's hunk lives in an artifact, so the line totals leave it out.
    partial: bool,
    failed: Vec<(u64, String, i32)>,
    failed_total: u32,
    done: Option<u64>,
    accurate: Option<bool>,
}

/// Lines a unified diff hunk adds and removes, without its file headers.
fn hunk_lines(hunk: &str) -> (u64, u64) {
    // File headers come before the first `@@`; inside a hunk every `+` or `-` line is content.
    let mut in_hunk = false;
    let mut counts = (0, 0);
    for l in hunk.lines() {
        if l.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk || !(l.starts_with("+++") || l.starts_with("---")) {
            if l.starts_with('+') {
                counts.0 += 1;
            } else if l.starts_with('-') {
                counts.1 += 1;
            }
        }
    }
    counts
}

#[derive(Default)]
pub(crate) struct Activity {
    by_item: BTreeMap<ItemId, Vec<Value>>,
    hands: BTreeMap<ItemId, BTreeMap<u32, Hand>>,
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
        if refused.is_none() {
            self.note_handoff(offset, rec);
        }
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
            WorkRecord::Accepted {
                tier, admitted_by, ..
            } => {
                let mut more = json!({ "tier": tier });
                if let Some(rule) = admitted_by {
                    more["rule"] = json!(rule);
                }
                e("accepted", None, more)
            }
            WorkRecord::AdmissionUndone { .. } => e("admission_undone", None, json!({})),
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

    /// Folds an accepted record into the attempt's handoff. A refused record never gets here.
    fn note_handoff(&mut self, offset: u64, rec: &WorkLogged) {
        let (item, attempt) = match &rec.record {
            WorkRecord::Progress { item, attempt, .. }
            | WorkRecord::Step { item, attempt, .. }
            | WorkRecord::Edit { item, attempt, .. }
            | WorkRecord::Done { item, attempt, .. }
            | WorkRecord::SummaryRated { item, attempt, .. } => (item, *attempt),
            _ => return,
        };
        let h = self
            .hands
            .entry(item.clone())
            .or_default()
            .entry(attempt)
            .or_default();
        match &rec.record {
            // Only the agent's own note is its summary; a person's note on the attempt is not.
            WorkRecord::Progress { note, .. } if rec.actor == Actor::Agent => {
                h.summary = match note {
                    Content::Text(t) => Some(t.clone()),
                    Content::Artifact(_) => None,
                };
            }
            WorkRecord::Step {
                command, exit_code, ..
            } if *exit_code != 0 => {
                h.failed_total += 1;
                if h.failed.len() < MAX_FAILED {
                    h.failed.push((offset, command.clone(), *exit_code));
                }
            }
            WorkRecord::Edit { file, hunk, .. } => {
                if h.files.len() < MAX_FILES {
                    h.files.insert(file.clone());
                }
                match hunk {
                    Content::Text(t) => {
                        let (a, r) = hunk_lines(t);
                        h.added += a;
                        h.removed += r;
                    }
                    Content::Artifact(_) => h.partial = true,
                }
            }
            WorkRecord::Done { .. } => h.done = Some(offset),
            WorkRecord::SummaryRated { accurate, .. } => h.accurate = Some(*accurate),
            _ => {}
        }
    }

    /// For each item, its latest finished attempt's handoff, as `{item: {...}}`.
    pub(crate) fn handoffs_json(&self) -> String {
        let mut out = serde_json::Map::new();
        for (item, attempts) in &self.hands {
            let Some((attempt, h)) = attempts.iter().rev().find(|(_, h)| h.done.is_some()) else {
                continue;
            };
            out.insert(
                item.as_str().to_owned(),
                json!({
                    "attempt": attempt,
                    "done_at": h.done,
                    "summary": h.summary,
                    "files": h.files.len(),
                    "added": h.added,
                    "removed": h.removed,
                    "partial": h.partial,
                    "failed_steps": h.failed.iter()
                        .map(|(o, c, x)| json!({ "offset": o, "command": c, "exit_code": x }))
                        .collect::<Vec<_>>(),
                    "failed_total": h.failed_total,
                    "accurate": h.accurate,
                }),
            );
        }
        Value::Object(out).to_string()
    }

    /// The item's events as a JSON array, oldest first.
    pub(crate) fn json(&self, item: &ItemId) -> String {
        serde_json::to_string(self.by_item.get(item).map_or(&[][..], Vec::as_slice))
            .expect("plain data serializes")
    }
}
