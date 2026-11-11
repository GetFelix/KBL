//! The work tools an agent calls beyond claiming: post, link, context,
//! progress, decide, record a step, edit, answer. Each appends one record
//! the fold accepts, attributed to the agent whose token made the call. The
//! tracker writes no text of its own.
//!
//! A refusal leaves no record, with two exceptions: a root that ran out of
//! budget is flagged (as for a claim), and the first time an attempt hits a
//! per-attempt cap the refusal is logged, so a looping agent cannot fill the
//! stream with refusals either.

use agents_core::artifact::{ArtifactId, INLINE_LIMIT};
use agents_core::content::Content;
use agents_core::names::{AgentName, ItemId, Key};
use agents_core::work::{
    Actor, DoneWhen, Evidence, NewPrerequisite, Phase, RefusalReason, Target, WorkKind, WorkRecord,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

use super::{Core, Error, Tracker, agent_key, holds};
use crate::felix::Broker;

/// Longest title, command, file name or reason an agent may write.
const MAX_FIELD: usize = 4096;
const MAX_OPTIONS: usize = 16;
const MAX_EVIDENCE: usize = 32;

/// What `work_post` creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Post {
    /// A child under the item the agent holds.
    Child(WorkKind),
    /// Out-of-scope work: a root once a person accepts it.
    Proposal,
}

impl Post {
    pub(crate) fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "split" => Post::Child(WorkKind::Split),
            "augment" => Post::Child(WorkKind::Augment),
            "question" => Post::Child(WorkKind::Question),
            "test" => Post::Child(WorkKind::Test),
            "proposal" | "fix" => Post::Proposal,
            other => {
                return Err(Error::Invalid(format!(
                    "{other:?} is not a kind of work you can post: use split, augment, question, test, or proposal for work outside this item"
                )));
            }
        })
    }
}

/// The work another item has to wait for.
pub(crate) enum Prereq {
    /// A proposal to create in the target repo's Triage.
    New {
        target: Target,
        title: String,
        why: Content,
        evidence: Vec<Evidence>,
    },
    /// An item that exists already, if the agent's grants can see it.
    Existing(ItemId),
}

/// What a decision carries, as the agent wrote it.
pub(crate) struct Decide {
    pub(crate) phase: Phase,
    pub(crate) so_far: String,
    pub(crate) question: String,
    pub(crate) options: Vec<String>,
    pub(crate) chosen: String,
    pub(crate) why: String,
    pub(crate) commit: String,
    pub(crate) evidence: Vec<Evidence>,
}

impl<B: Broker> Tracker<B> {
    /// Creates a child under the item the agent holds, or a proposal. The same
    /// `key` again answers with the item the first call made.
    pub(crate) async fn post(
        &self,
        agent: &AgentName,
        parent: &ItemId,
        key: Key,
        kind: Post,
        title: String,
    ) -> Result<Value, Error> {
        field("title", &title)?;
        if title.is_empty() {
            return Err(Error::Invalid("a title is needed".into()));
        }
        self.begin().await?;
        let (work, parent_of) = match kind {
            Post::Child(w) => (w, Some(parent.clone())),
            Post::Proposal => (WorkKind::Fix, None),
        };
        // Proposals are keyed per agent, children per parent.
        let id = item_for(parent_of.as_ref(), kind, agent, &key);
        let reply = {
            let mut core = self.0.core.lock().await;
            held(&core, agent, parent)?;
            if let Some(existing) = core.state.items.get(&id) {
                if existing.work != work || existing.parent != parent_of {
                    return Err(Error::Refused {
                        why: "key_reused".into(),
                    });
                }
                return Ok(json!({ "item": id, "created": false, "work": work }));
            }
            let record = WorkRecord::Proposed {
                item: id.clone(),
                parent: parent_of,
                key,
                work,
                title,
            };
            let offset = self
                .agent_append(&mut core, agent, parent, "post", record)
                .await?;
            json!({ "item": id, "created": true, "work": work, "offset": offset })
        };
        self.sync_ready().await;
        Ok(reply)
    }

    /// The held item cannot go on until work in another tree is done. The
    /// claim ends as blocked, which costs no try, and the item waits on the
    /// prerequisite. This is the one cross-tree link an agent may make, and
    /// only into a repo its grants name or the repo of its own tree.
    pub(crate) async fn prerequisite(
        &self,
        agent: &AgentName,
        item: &ItemId,
        key: Key,
        which: Prereq,
        done_when: DoneWhen,
        cost_micros: u64,
    ) -> Result<Value, Error> {
        let (prerequisite, new) = match which {
            Prereq::New {
                target,
                title,
                why,
                evidence,
            } => {
                field("title", &title)?;
                field("repo", &target.repo)?;
                field("source", &target.source)?;
                check_content("why", &why)?;
                check_evidence(&evidence)?;
                if title.is_empty() || target.repo.is_empty() || target.source.is_empty() {
                    return Err(Error::Invalid(
                        "a source, a repo and a title are needed".into(),
                    ));
                }
                let id = item_for(None, Post::Proposal, agent, &key);
                let new = NewPrerequisite {
                    target,
                    title,
                    why,
                    evidence,
                };
                (id, Some(new))
            }
            Prereq::Existing(id) => (id, None),
        };
        self.begin().await?;
        {
            let core = self.0.core.lock().await;
            if let Some(n) = &new {
                // A retry after the claim ended answers with what the first call made.
                if let Some(old) = core.state.items.get(&prerequisite) {
                    let same = old.proposed_by_agent
                        && old.target.as_ref() == Some(&n.target)
                        && core
                            .state
                            .edges
                            .contains(&(item.clone(), prerequisite.clone()));
                    if !same {
                        return Err(Error::Refused {
                            why: "key_reused".into(),
                        });
                    }
                    return Ok(json!({
                        "item": item, "status": "blocked",
                        "prerequisite": prerequisite, "created": false
                    }));
                }
            }
            held(&core, agent, item)?;
            let allowed = self.visible_repos(&core, agent, item);
            let seen = match &new {
                Some(n) => allowed.contains(n.target.repo.as_str()),
                None => core.state.items.get(&prerequisite).is_some_and(|p| {
                    let own = core.state.items.get(item).map(|i| &i.root) == Some(&p.root);
                    let repo = core
                        .state
                        .items
                        .get(&p.root)
                        .and_then(|r| r.target.as_ref());
                    own || repo.is_some_and(|t| allowed.contains(t.repo.as_str()))
                }),
            };
            if !seen {
                // The same answer for an item that is missing and one the agent may not see.
                return Err(Error::Refused {
                    why: if new.is_some() {
                        "no_grant"
                    } else {
                        "unknown_item"
                    }
                    .into(),
                });
            }
        }
        let created = new.is_some();
        let record = {
            let prerequisite = prerequisite.clone();
            let item = item.clone();
            move |attempt| WorkRecord::Prerequisite {
                item,
                attempt,
                prerequisite,
                key,
                new,
                done_when,
                cost_micros,
            }
        };
        let reply = json!({
            "item": item, "status": "blocked",
            "prerequisite": prerequisite, "created": created
        });
        let result = self.finish(agent, item, record, reply).await;
        if let Err(Error::Refused { why }) = &result {
            let mut core = self.0.core.lock().await;
            self.note_refusal(&mut core, agent, item, "prerequisite", why)
                .await;
        }
        result
    }

    /// The repos `agent` may reach from `item`: its grants and its own tree's repo.
    fn visible_repos<'a>(
        &'a self,
        core: &'a Core,
        agent: &AgentName,
        item: &ItemId,
    ) -> BTreeSet<&'a str> {
        let mut repos: BTreeSet<&str> = self
            .0
            .settings
            .grants
            .get(agent.as_str())
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        let own = core
            .state
            .items
            .get(item)
            .and_then(|i| core.state.items.get(&i.root))
            .and_then(|r| r.target.as_ref());
        repos.extend(own.map(|t| t.repo.as_str()));
        repos
    }

    /// Adds the edge "`item` waits on `to`". Refused when it would close a cycle.
    pub(crate) async fn link(
        &self,
        agent: &AgentName,
        item: &ItemId,
        to: &ItemId,
    ) -> Result<Value, Error> {
        self.begin().await?;
        {
            let mut core = self.0.core.lock().await;
            // A replay would append a record that changes nothing.
            if core.state.edges.contains(&(item.clone(), to.clone())) {
                return Err(Error::Refused {
                    why: "already_linked".into(),
                });
            }
            let record = WorkRecord::Linked {
                item: item.clone(),
                to: to.clone(),
            };
            self.agent_append(&mut core, agent, item, "link", record)
                .await?;
        }
        self.sync_ready().await;
        Ok(json!({ "item": item, "waits_on": to }))
    }

    /// Writes a context entry; the same `entry` again replaces it.
    pub(crate) async fn context_add(
        &self,
        agent: &AgentName,
        item: &ItemId,
        entry: Key,
        text: Content,
    ) -> Result<Value, Error> {
        check_content("text", &text)?;
        self.begin().await?;
        let mut core = self.0.core.lock().await;
        let record = WorkRecord::Context {
            item: item.clone(),
            entry: entry.clone(),
            text,
        };
        let offset = self
            .agent_append(&mut core, agent, item, "context", record)
            .await?;
        Ok(json!({ "item": item, "entry": entry, "offset": offset }))
    }

    /// What an agent should know before it works on `item`: its context
    /// entries, and the notes people wrote for the attempt it is on. The agent
    /// must hold the item or its parent.
    pub(crate) async fn context_read(
        &self,
        agent: &AgentName,
        item: &ItemId,
    ) -> Result<Value, Error> {
        self.begin().await?;
        let core = self.0.core.lock().await;
        let it = core.state.items.get(item).ok_or_else(|| Error::Refused {
            why: "unknown_item".into(),
        })?;
        let parent_held = it
            .parent
            .as_ref()
            .and_then(|p| core.state.items.get(p))
            .is_some_and(|p| holds(p, agent));
        if !holds(it, agent) && !parent_held {
            return Err(Error::NotHeld(format!(
                "you hold neither {item} nor its parent"
            )));
        }
        let text = |offset: &u64| core.texts.get(offset).map_or(Value::Null, |c| json!(c));
        let entries: Vec<Value> = it
            .context
            .iter()
            .map(
                |(entry, offset)| json!({ "entry": entry, "offset": offset, "text": text(offset) }),
            )
            .collect();
        let attempt = it.hold.as_ref().map(|h| h.attempt);
        let offsets = match attempt {
            Some(a) => core.state.human_inputs_on_path(item, a),
            None => it.human_inputs.clone(),
        };
        let notes: Vec<Value> = offsets
            .iter()
            .map(|o| json!({ "offset": o, "text": text(o) }))
            .collect();
        // What the item waited on, so a resumed claim knows what landed and where.
        let waited: Vec<Value> = core
            .state
            .edges
            .iter()
            .filter(|(a, _)| a == item)
            .filter_map(|(_, b)| core.state.items.get(b))
            .map(|p| {
                json!({
                    "item": p.id,
                    "title": p.title,
                    "repo": p.target.as_ref().map(|t| &t.repo),
                    "stage": core.state.stage(&p.id),
                    "pr": p.pr,
                    "release": p.release,
                })
            })
            .collect();
        Ok(json!({
            "item": item,
            "title": it.title,
            "work": it.work,
            "parent": it.parent,
            "context": entries,
            "notes": notes,
            "open_question": it.open_question,
            "prerequisites": waited,
        }))
    }

    /// Asks a person a question; the item waits in Needs you until one answers.
    pub(crate) async fn ask(
        &self,
        agent: &AgentName,
        item: &ItemId,
        text: Content,
    ) -> Result<Value, Error> {
        check_content("text", &text)?;
        self.held_write(agent, item, "ask", |attempt| WorkRecord::Question {
            item: item.clone(),
            attempt,
            text,
        })
        .await
    }

    /// Records a plan, a branch or a PR on the item the agent holds.
    pub(crate) async fn progress(
        &self,
        agent: &AgentName,
        item: &ItemId,
        note: Content,
        branch: Option<String>,
        pr: Option<String>,
    ) -> Result<Value, Error> {
        check_content("note", &note)?;
        for (name, v) in [("branch", &branch), ("pr", &pr)] {
            if let Some(v) = v {
                field(name, v)?;
            }
        }
        self.held_write(agent, item, "progress", |attempt| WorkRecord::Progress {
            item: item.clone(),
            attempt,
            note,
            branch,
            pr,
        })
        .await
    }

    /// Records a decision. The reply's `offset` is what an edit cites, and what
    /// a person forks from.
    pub(crate) async fn decide(
        &self,
        agent: &AgentName,
        item: &ItemId,
        d: Decide,
    ) -> Result<Value, Error> {
        for (name, v) in [
            ("so_far", &d.so_far),
            ("question", &d.question),
            ("chosen", &d.chosen),
            ("why", &d.why),
            ("commit", &d.commit),
        ] {
            field(name, v)?;
        }
        if d.why.is_empty() {
            return Err(Error::Invalid("why is needed".into()));
        }
        if d.options.len() > MAX_OPTIONS {
            return Err(Error::Invalid(format!("at most {MAX_OPTIONS} options")));
        }
        for o in &d.options {
            field("option", o)?;
        }
        check_evidence(&d.evidence)?;
        self.held_write(agent, item, "decide", |attempt| WorkRecord::Decision {
            item: item.clone(),
            attempt,
            phase: d.phase,
            so_far: d.so_far,
            question: d.question,
            options: d.options,
            chosen: d.chosen,
            why: d.why,
            commit: d.commit,
            evidence: d.evidence,
        })
        .await
    }

    /// Records a command the agent ran: what it was, its exit code, its output.
    pub(crate) async fn record_step(
        &self,
        agent: &AgentName,
        item: &ItemId,
        command: String,
        exit_code: i32,
        output: Content,
    ) -> Result<Value, Error> {
        field("command", &command)?;
        check_content("output", &output)?;
        self.held_write(agent, item, "record_step", |attempt| WorkRecord::Step {
            item: item.clone(),
            attempt,
            command,
            exit_code,
            output,
        })
        .await
    }

    /// Records a code change and the decision it carries out.
    pub(crate) async fn edit(
        &self,
        agent: &AgentName,
        item: &ItemId,
        file: String,
        hunk: Content,
        decision: u64,
    ) -> Result<Value, Error> {
        field("file", &file)?;
        check_content("hunk", &hunk)?;
        self.held_write(agent, item, "edit", |attempt| WorkRecord::Edit {
            item: item.clone(),
            attempt,
            file,
            hunk,
            decision,
        })
        .await
    }

    /// Answers the open question on `item`, as the agent holding a question
    /// item under it.
    pub(crate) async fn answer(
        &self,
        agent: &AgentName,
        item: &ItemId,
        question: u64,
        text: Content,
        evidence: Vec<Evidence>,
    ) -> Result<Value, Error> {
        check_content("text", &text)?;
        check_evidence(&evidence)?;
        self.begin().await?;
        let offset = {
            let mut core = self.0.core.lock().await;
            let record = WorkRecord::Answer {
                item: item.clone(),
                question,
                text,
                evidence,
            };
            self.agent_append(&mut core, agent, item, "answer", record)
                .await?
        };
        self.sync_ready().await;
        Ok(json!({ "item": item, "offset": offset }))
    }

    /// A person answers a question an agent asked, or one asked of the item.
    /// `question` defaults to the open one.
    pub(crate) async fn person_answer(
        &self,
        person: Key,
        item: &ItemId,
        question: Option<u64>,
        text: Content,
        evidence: Vec<Evidence>,
    ) -> Result<Value, Error> {
        self.begin().await?;
        let question = {
            let core = self.0.core.lock().await;
            question
                .or_else(|| core.state.items.get(item).and_then(|i| i.open_question))
                .ok_or_else(|| Error::Refused {
                    why: "no_open_question".into(),
                })?
        };
        self.person_record(
            person,
            WorkRecord::Answer {
                item: item.clone(),
                question,
                text,
                evidence,
            },
        )
        .await
    }

    /// Writes a record that names the attempt the agent is on, found from the log.
    async fn held_write(
        &self,
        agent: &AgentName,
        item: &ItemId,
        op: &str,
        record: impl FnOnce(u32) -> WorkRecord,
    ) -> Result<Value, Error> {
        self.begin().await?;
        let mut core = self.0.core.lock().await;
        let attempt = held(&core, agent, item)?;
        let offset = self
            .agent_append(&mut core, agent, item, op, record(attempt))
            .await?;
        Ok(json!({ "item": item, "attempt": attempt, "offset": offset }))
    }

    /// Appends as the agent. A root out of budget is flagged, and the first cap
    /// an attempt hits is logged.
    async fn agent_append(
        &self,
        core: &mut Core,
        agent: &AgentName,
        item: &ItemId,
        op: &str,
        record: WorkRecord,
    ) -> Result<u64, Error> {
        let result = self
            .append(core, Actor::Agent, agent_key(agent), record)
            .await;
        if let Err(Error::Refused { why }) = &result {
            self.note_refusal(core, agent, item, op, why).await;
        }
        result
    }

    /// Logs the refusals the stream should show: a root out of budget, and the
    /// first cap an attempt hits.
    async fn note_refusal(
        &self,
        core: &mut Core,
        agent: &AgentName,
        item: &ItemId,
        op: &str,
        why: &str,
    ) {
        match why {
            "over_budget" | "too_deep" => self.log_over_budget(core, item).await,
            "limit_reached" => {
                let held = core.state.held_by(agent.as_str());
                let logged = held.as_ref().is_none_or(|(i, attempt)| {
                    core.state
                        .items
                        .get(i)
                        .and_then(|it| it.paths.iter().find(|p| p.attempt == *attempt))
                        .is_none_or(|p| p.capped)
                });
                if let (false, Some((held, _))) = (logged, held) {
                    let refusal = WorkRecord::Refused {
                        item: Some(held),
                        op: op.to_owned(),
                        reason: RefusalReason::Limit,
                    };
                    let _ = self
                        .append(core, Actor::Tracker, self.0.me.clone(), refusal)
                        .await;
                }
            }
            _ => {}
        }
    }
}

/// The attempt `agent` holds `item` on, from the log.
fn held(core: &Core, agent: &AgentName, item: &ItemId) -> Result<u32, Error> {
    core.state
        .items
        .get(item)
        .filter(|i| holds(i, agent))
        .and_then(|i| i.hold.as_ref())
        .map(|h| h.attempt)
        .ok_or_else(|| Error::NotHeld(format!("you do not hold {item}")))
}

/// The id a keyed post gets, the same on every retry. Children are keyed by
/// parent, proposals by the agent that found them.
fn item_for(parent: Option<&ItemId>, kind: Post, agent: &AgentName, key: &Key) -> ItemId {
    let scope = match (kind, parent) {
        (Post::Child(_), Some(p)) => format!("c:{p}"),
        _ => format!("p:{agent}"),
    };
    let hash = ArtifactId::of(format!("{scope}\n{key}").as_bytes());
    // 20 characters of the hash: wide enough that a collision is not a concern.
    ItemId::new(format!("w{}", &hash.as_str()[2..22])).expect("hash characters are valid")
}

fn check_evidence(evidence: &[Evidence]) -> Result<(), Error> {
    if evidence.len() > MAX_EVIDENCE {
        return Err(Error::Invalid(format!(
            "at most {MAX_EVIDENCE} evidence entries"
        )));
    }
    for e in evidence {
        field("evidence stream", &e.stream)?;
    }
    Ok(())
}

fn field(name: &str, value: &str) -> Result<(), Error> {
    if value.len() > MAX_FIELD {
        return Err(Error::Invalid(format!(
            "{name} is {} bytes; at most {MAX_FIELD}",
            value.len()
        )));
    }
    Ok(())
}

/// Inline text stays under the record limit; anything bigger is an artifact
/// the caller stored first.
fn check_content(name: &str, content: &Content) -> Result<(), Error> {
    match content {
        Content::Text(t) if t.len() > INLINE_LIMIT => Err(Error::Invalid(format!(
            "{name} is {} bytes; over {INLINE_LIMIT} it must be an artifact: store it and pass its id",
            t.len()
        ))),
        _ => Ok(()),
    }
}

/// The size rules for any record a caller writes.
pub(super) fn check_sizes(record: &WorkRecord) -> Result<(), Error> {
    match record {
        WorkRecord::Answer { text, evidence, .. } => {
            check_content("text", text)?;
            check_evidence(evidence)
        }
        WorkRecord::Context { text, .. }
        | WorkRecord::Question { text, .. }
        | WorkRecord::HumanInput { text, .. } => check_content("text", text),
        WorkRecord::Edited {
            title: Some(title), ..
        } => {
            field("title", title)?;
            if title.is_empty() {
                return Err(Error::Invalid("a title is needed".into()));
            }
            Ok(())
        }
        WorkRecord::Shipped { release, .. } => {
            field("release", release)?;
            if release.is_empty() {
                return Err(Error::Invalid("a release is needed".into()));
            }
            Ok(())
        }
        WorkRecord::Progress { note, .. } => check_content("note", note),
        WorkRecord::Step { output, .. } => check_content("output", output),
        WorkRecord::Edit { hunk, .. } => check_content("hunk", hunk),
        WorkRecord::Proposed { title, .. } | WorkRecord::Imported { title, .. } => {
            field("title", title)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
