//! An item's story: the decisions the working agent recorded, in order across
//! attempts and grouped by phase, with the edits and evidence behind each, and
//! the flags where a claim and the recorded steps disagree.
//!
//! Everything here is a field the log holds or a rule over those fields.
//! Nothing is generated, and the rules are listed in
//! `docs/decision-replay.md#claims-checked-against-steps`. A step nobody
//! recorded is invisible to every rule.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::content::Content;
use crate::names::ItemId;
use crate::work::{Evidence, HumanKind, Phase, RevertCheck, WORK_EVENTS, WorkLogged, WorkRecord};
use crate::work_fold::{Item, Path, State};

#[cfg(test)]
mod tests;

/// The fields of a record the story reads, kept by offset. The fold keeps only
/// the offsets, and a page has no log to read the rest from. A holder keeps one
/// per accepted decision, step, edit and note, so its size is bounded by those
/// records' own text (a hunk or command at most as large as the record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    Decision(Box<KeptDecision>),
    Step {
        command: String,
        exit_code: i32,
    },
    Edit {
        file: String,
        hunk: Content,
        decision: u64,
    },
    Input {
        by: String,
        input: HumanKind,
        text: Content,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptDecision {
    pub item: ItemId,
    pub attempt: u32,
    pub by: String,
    pub at_ms: u64,
    pub phase: Phase,
    pub so_far: String,
    pub question: String,
    pub options: Vec<String>,
    pub chosen: String,
    pub why: String,
    pub commit: String,
    pub evidence: Vec<Evidence>,
    pub revert_check: Option<RevertCheck>,
}

impl Kept {
    /// What the story needs of `rec`, or None for a kind it does not read.
    pub fn of(rec: &WorkLogged) -> Option<Kept> {
        Some(match &rec.record {
            WorkRecord::Decision {
                item,
                attempt,
                phase,
                so_far,
                question,
                options,
                chosen,
                why,
                commit,
                evidence,
                revert_check,
            } => Kept::Decision(Box::new(KeptDecision {
                item: item.clone(),
                attempt: *attempt,
                by: rec.by.as_str().to_owned(),
                at_ms: rec.at_ms,
                phase: *phase,
                so_far: so_far.clone(),
                question: question.clone(),
                options: options.clone(),
                chosen: chosen.clone(),
                why: why.clone(),
                commit: commit.clone(),
                evidence: evidence.clone(),
                revert_check: revert_check.clone(),
            })),
            WorkRecord::Step {
                command, exit_code, ..
            } => Kept::Step {
                command: command.clone(),
                exit_code: *exit_code,
            },
            WorkRecord::Edit {
                file,
                hunk,
                decision,
                ..
            } => Kept::Edit {
                file: file.clone(),
                hunk: hunk.clone(),
                decision: *decision,
            },
            WorkRecord::HumanInput { input, text, .. } => Kept::Input {
                by: rec.by.as_str().to_owned(),
                input: *input,
                text: text.clone(),
            },
            _ => return None,
        })
    }
}

/// Phases in the order a story reads them.
const PHASES: [Phase; 5] = [
    Phase::Understand,
    Phase::Reproduce,
    Phase::Cause,
    Phase::Fix,
    Phase::Verify,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Story {
    pub item: ItemId,
    /// Each attempt, so a reader sees where a redo branched.
    pub attempts: Vec<AttemptLine>,
    /// Every phase that has a decision, in story order, decisions by offset.
    pub groups: Vec<Group>,
    /// What people wrote on the item, by offset.
    pub people: Vec<PersonLine>,
    pub flags: Vec<Flag>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptLine {
    pub attempt: u32,
    /// The attempt and offset of the decision a redo started from.
    pub forked_from: Option<(u32, u64)>,
    pub done: bool,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    pub phase: Phase,
    pub decisions: Vec<DecisionLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionLine {
    pub offset: u64,
    pub attempt: u32,
    pub by: String,
    pub at_ms: u64,
    pub so_far: String,
    pub question: String,
    pub options: Vec<String>,
    pub chosen: String,
    pub why: String,
    pub commit: String,
    pub evidence: Vec<Cited>,
    pub revert_check: Option<RevertCheck>,
    /// The revert check showed the test guards the fix.
    pub proven: bool,
    /// The edits that carry this decision out, in offset order.
    pub edits: Vec<EditLine>,
}

/// An offset a decision cites. `found` is what the log holds there, when the
/// record is one the story keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cited {
    pub stream: String,
    pub offset: u64,
    pub found: Option<Found>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Found {
    Step { command: String, exit_code: i32 },
    Edit { file: String },
    Decision { chosen: String },
    Input { by: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditLine {
    pub offset: u64,
    pub file: String,
    pub hunk: Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonLine {
    pub offset: u64,
    /// The attempt a note steers, or none for the item as a whole.
    pub attempt: Option<u32>,
    pub by: String,
    pub input: HumanKind,
    pub text: Content,
}

/// The rules a flag comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// A decision says tests pass and no recorded passing test step lies between the attempt's last edit before it and the decision.
    ClaimsPassWithoutRun,
    /// A decision says tests pass and cites a step that exited non-zero.
    CitedStepFailed,
    /// A decision cites an offset on the work stream that is not a recorded step, edit, decision or person's note.
    EvidenceNotRecorded,
    /// A decision carries a revert check that did not show the test guards the fix.
    RevertCheckNotProven,
    /// A fix decision with no recorded edit carrying it out.
    FixWithoutEdit,
    /// An attempt recorded edits and no passing step after its last one.
    EditsNeverRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Flag {
    pub rule: Rule,
    pub attempt: u32,
    /// The decision the flag is about, or none for an attempt-wide flag.
    pub decision: Option<u64>,
    /// The offsets the message names.
    pub offsets: Vec<u64>,
    pub message: String,
}

/// Whether the fold took `rec` at `offset` as one the story reads. A page
/// keeps [`Kept`] only for these, so a refused record is not held.
pub fn accepted(state: &State, offset: u64, rec: &WorkLogged) -> bool {
    let Some(item) = rec.record.item().and_then(|i| state.items.get(i)) else {
        return false;
    };
    let in_path = |f: fn(&Path) -> &Vec<u64>| item.paths.iter().any(|p| f(p).contains(&offset));
    match &rec.record {
        WorkRecord::Decision { .. } => in_path(|p| &p.decisions),
        WorkRecord::Step { .. } => in_path(|p| &p.steps),
        WorkRecord::Edit { .. } => in_path(|p| &p.edits),
        WorkRecord::HumanInput { .. } => {
            item.human_inputs.contains(&offset) || in_path(|p| &p.human_inputs)
        }
        _ => false,
    }
}

/// Whether the item has a decision to tell a story from.
pub fn has_story(item: &Item) -> bool {
    item.paths.iter().any(|p| !p.decisions.is_empty())
}

/// The story of `item`, or None when it has no recorded decision. `kept`
/// answers an offset with what the log holds there.
pub fn story<'a>(
    state: &State,
    item: &ItemId,
    kept: impl Fn(u64) -> Option<&'a Kept>,
) -> Option<Story> {
    let it = state.items.get(item)?;
    if !has_story(it) {
        return None;
    }
    let mut flags = Vec::new();
    let mut lines: BTreeMap<u64, (Phase, DecisionLine)> = BTreeMap::new();
    for p in &it.paths {
        let mut edits: BTreeMap<u64, Vec<(u64, &str, &Content)>> = BTreeMap::new();
        for &o in &p.edits {
            if let Some(Kept::Edit {
                file,
                hunk,
                decision,
            }) = kept(o)
            {
                edits
                    .entry(*decision)
                    .or_default()
                    .push((o, file.as_str(), hunk));
            }
        }
        for &o in &p.decisions {
            let Some(Kept::Decision(d)) = kept(o) else {
                continue;
            };
            let line = decision_line(o, d, p, &edits, &kept);
            decision_flags(o, d, &line, p, &kept, &mut flags);
            lines.insert(o, (d.phase, line));
        }
        edits_never_run(p, &kept, &mut flags);
    }
    flags.sort_by_key(|f| (f.decision.unwrap_or(u64::MAX), f.attempt, f.rule as u8));
    let groups = PHASES
        .iter()
        .filter_map(|&phase| {
            let decisions: Vec<_> = lines
                .values()
                .filter(|(p, _)| *p == phase)
                .map(|(_, l)| l.clone())
                .collect();
            (!decisions.is_empty()).then_some(Group { phase, decisions })
        })
        .collect();
    let mut people: Vec<PersonLine> = Vec::new();
    let inputs = it.human_inputs.iter().map(|&o| (o, None)).chain(
        it.paths
            .iter()
            .flat_map(|p| p.human_inputs.iter().map(|&o| (o, Some(p.attempt)))),
    );
    for (offset, attempt) in inputs {
        if let Some(Kept::Input { by, input, text }) = kept(offset) {
            people.push(PersonLine {
                offset,
                attempt,
                by: by.clone(),
                input: *input,
                text: text.clone(),
            });
        }
    }
    people.sort_by_key(|p| p.offset);
    Some(Story {
        item: item.clone(),
        attempts: it
            .paths
            .iter()
            .map(|p| AttemptLine {
                attempt: p.attempt,
                forked_from: p.forked_from.as_ref().map(|f| (f.from_attempt, f.at)),
                done: p.done,
                closed: p.closed,
            })
            .collect(),
        groups,
        people,
        flags,
    })
}

fn decision_line<'a>(
    offset: u64,
    d: &KeptDecision,
    path: &Path,
    edits: &BTreeMap<u64, Vec<(u64, &str, &Content)>>,
    kept: &impl Fn(u64) -> Option<&'a Kept>,
) -> DecisionLine {
    DecisionLine {
        offset,
        attempt: d.attempt,
        by: d.by.clone(),
        at_ms: d.at_ms,
        so_far: d.so_far.clone(),
        question: d.question.clone(),
        options: d.options.clone(),
        chosen: d.chosen.clone(),
        why: d.why.clone(),
        commit: d.commit.clone(),
        evidence: d
            .evidence
            .iter()
            .map(|e| Cited {
                stream: e.stream.clone(),
                offset: e.offset,
                found: (e.stream == WORK_EVENTS)
                    .then(|| kept(e.offset).map(found))
                    .flatten(),
            })
            .collect(),
        revert_check: d.revert_check.clone(),
        proven: path.proven.contains(&offset),
        edits: edits
            .get(&offset)
            .into_iter()
            .flatten()
            .map(|&(o, file, hunk)| EditLine {
                offset: o,
                file: file.to_owned(),
                hunk: hunk.clone(),
            })
            .collect(),
    }
}

fn found(k: &Kept) -> Found {
    match k {
        Kept::Step { command, exit_code } => Found::Step {
            command: command.clone(),
            exit_code: *exit_code,
        },
        Kept::Edit { file, .. } => Found::Edit { file: file.clone() },
        Kept::Decision(d) => Found::Decision {
            chosen: d.chosen.clone(),
        },
        Kept::Input { by, .. } => Found::Input { by: by.clone() },
    }
}

/// A decision says tests pass when its own words say both "test" and "pass".
/// It is a prompt to look, so "tests do not pass" is read the same way.
fn claims_pass(d: &KeptDecision) -> bool {
    let said = format!("{} {} {}", d.so_far, d.chosen, d.why).to_lowercase();
    said.contains("test") && said.contains("pass")
}

/// Programs whose `test` subcommand runs tests.
const TEST_DRIVERS: [&str; 18] = [
    "cargo", "npm", "yarn", "pnpm", "bun", "deno", "go", "make", "task", "just", "mvn", "gradle",
    "gradlew", "dotnet", "swift", "mix", "rake", "flutter",
];

/// Programs that are a test runner by themselves.
const TEST_RUNNERS: [&str; 9] = [
    "pytest", "py.test", "jest", "vitest", "mocha", "rspec", "phpunit", "nextest", "ctest",
];

/// A step that ran tests: a test runner (`pytest`), a driver's `test`
/// subcommand (`cargo test`, `npm run test`, `go test`) or `node --test`.
/// Words are matched whole, so `latest` or `grep attest` do not count.
fn is_test_run(command: &str) -> bool {
    let lower = command.to_lowercase();
    lower.split([';', '|', '&', '(', ')']).any(|part| {
        let words: Vec<&str> = part
            .split_whitespace()
            .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
            .map(|w| w.rsplit('/').next().unwrap_or(w))
            .collect();
        words.iter().enumerate().any(|(i, &w)| {
            if TEST_RUNNERS.contains(&w) || w == "unittest" {
                return true;
            }
            let before = || {
                words[..i]
                    .iter()
                    .rev()
                    .find(|p| !p.starts_with(['-', '+']) && **p != "run")
            };
            match w {
                "--test" => words[..i].last() == Some(&"node"),
                _ if w == "test" || w.starts_with("test:") => {
                    before().is_some_and(|p| TEST_DRIVERS.contains(p))
                }
                _ => false,
            }
        })
    })
}

fn passing_run(k: &Kept) -> bool {
    matches!(k, Kept::Step { command, exit_code: 0 } if is_test_run(command))
}

fn decision_flags<'a>(
    offset: u64,
    d: &KeptDecision,
    line: &DecisionLine,
    path: &Path,
    kept: &impl Fn(u64) -> Option<&'a Kept>,
    out: &mut Vec<Flag>,
) {
    let flag = |rule, offsets: Vec<u64>, message: String| Flag {
        rule,
        attempt: d.attempt,
        decision: Some(offset),
        offsets,
        message,
    };
    let claims = claims_pass(d);
    if claims {
        let last_edit = path
            .edits
            .iter()
            .copied()
            .filter(|&e| e < offset)
            .max()
            .map_or(0, |e| e + 1);
        let ran = path
            .steps
            .iter()
            .any(|&s| s >= last_edit && s < offset && kept(s).is_some_and(passing_run));
        if !ran {
            out.push(flag(
                Rule::ClaimsPassWithoutRun,
                path.edits
                    .iter()
                    .copied()
                    .filter(|&e| e < offset)
                    .max()
                    .into_iter()
                    .collect(),
                "Says tests pass; no recorded passing test run since the last recorded edit".into(),
            ));
        }
        for c in &line.evidence {
            if let Some(Found::Step { command, exit_code }) = &c.found
                && *exit_code != 0
            {
                out.push(flag(
                    Rule::CitedStepFailed,
                    vec![c.offset],
                    format!("Says tests pass; the step it cites (`{command}`) exited {exit_code}"),
                ));
            }
        }
    }
    for c in &line.evidence {
        if c.stream == WORK_EVENTS && c.found.is_none() {
            out.push(flag(
                Rule::EvidenceNotRecorded,
                vec![c.offset],
                format!(
                    "Cites offset {}, which is not a recorded step, edit, decision or note",
                    c.offset
                ),
            ));
        }
    }
    if let Some(r) = &d.revert_check
        && !r.proves()
    {
        out.push(flag(
            Rule::RevertCheckNotProven,
            Vec::new(),
            format!(
                "Revert check on `{}` did not show it fails without the fix and passes with it",
                r.test
            ),
        ));
    }
    if d.phase == Phase::Fix && line.edits.is_empty() {
        out.push(flag(
            Rule::FixWithoutEdit,
            Vec::new(),
            "A fix decision with no recorded edit carrying it out".into(),
        ));
    }
}

fn edits_never_run<'a>(path: &Path, kept: &impl Fn(u64) -> Option<&'a Kept>, out: &mut Vec<Flag>) {
    let Some(&last) = path.edits.iter().max() else {
        return;
    };
    if !path
        .steps
        .iter()
        .any(|&s| s > last && kept(s).is_some_and(passing_run))
    {
        out.push(Flag {
            rule: Rule::EditsNeverRun,
            attempt: path.attempt,
            decision: None,
            offsets: vec![last],
            message: "Edits recorded, and no recorded passing test run after the last one".into(),
        });
    }
}
