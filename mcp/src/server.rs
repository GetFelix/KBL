//! The MCP server and the tools it lists.

use std::time::Duration;

use kbl_core::content::Content;
use kbl_core::names::{AgentName, Resources, RunId};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::felix::Felix;
use crate::tasks::{Post, Tasks};
use crate::work::{Http, Work};

#[derive(Clone)]
pub(crate) struct Server {
    run: RunId,
    agent: AgentName,
    felix: Felix,
    tasks: Tasks<Felix>,
    /// None when no tracker is configured.
    work: Option<Work<Http>>,
}

/// Claim timing, when the defaults (90 s claim, extended every 30 s) are overridden.
pub(crate) struct Timing {
    pub(crate) visibility: Option<Duration>,
    pub(crate) extend_every: Option<Duration>,
}

impl Server {
    pub(crate) fn new(
        run: RunId,
        agent: AgentName,
        felix: Felix,
        tracker: Option<Http>,
        timing: Timing,
    ) -> Self {
        let tasks = Tasks::new(
            felix.clone(),
            Resources::new(run.clone()),
            agent.clone(),
            timing.visibility,
            timing.extend_every,
        );
        let work = tracker.map(|api| Work::new(api, timing.visibility, timing.extend_every));
        Self {
            run,
            agent,
            felix,
            tasks,
            work,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct TaskPostArgs {
    /// What the task is, in a line.
    title: String,
    /// The full instructions for the worker.
    body: Option<String>,
    /// The task or plan this one comes from. Defaults to `root`.
    parent: Option<String>,
    /// A lease key the worker should take before touching something shared.
    lease: Option<String>,
    /// Milliseconds since the Unix epoch after which the task is not worth finishing.
    deadline_ms: Option<u64>,
    /// An id of your own, 1 to 64 characters of A-Z, a-z, 0-9, - and _. Made up when left out.
    id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct TaskClaimArgs {
    /// Seconds to wait for a task before answering that there is none. Default 10, at most 30.
    wait_seconds: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkFindingArgs {
    /// Defaults to the review you hold.
    item: Option<String>,
    /// `low`, `medium`, `high` or `critical`.
    severity: String,
    /// The file the finding is in.
    file: String,
    /// The line, from 1.
    line: u32,
    /// One line naming the problem.
    title: String,
    /// The concrete case that goes wrong, so the author can reproduce it.
    scenario: String,
    /// `fix` (the author fixes it), `defer` (a person decides whether it is work) or `dismiss`.
    verdict: String,
}

#[derive(Deserialize, JsonSchema)]
struct WorkTriageArgs {
    /// Defaults to the triage item you hold.
    item: Option<String>,
    /// The tier you propose, 0 (most urgent) to 3.
    tier: u8,
    /// One line on why that tier, or what is unclear.
    why: String,
    /// The id of an item this one repeats, if you found one.
    duplicate_of: Option<String>,
    /// Set when you cannot settle the item: `scope`, `repo` or `acceptance`. It goes to a person as a question.
    unclear: Option<String>,
    /// Ids of items this one should wait on. A person decides whether to link them.
    depends_on: Option<Vec<String>>,
    /// Titles this item should be split into, if it is too big for one change. A person decides; nothing is created.
    split: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkVerdictArgs {
    /// Defaults to the review you hold.
    item: Option<String>,
    /// `merge` or `block`. A person decides; this is your recommendation.
    verdict: String,
    /// Why.
    why: String,
}

#[derive(Deserialize, JsonSchema)]
struct TaskCompleteArgs {
    /// The id of the task you hold.
    task: String,
    /// Your result, as text.
    result: String,
}

#[derive(Deserialize, JsonSchema)]
struct TaskReleaseArgs {
    /// The id of the task you hold.
    task: String,
    /// Why you are handing it back.
    reason: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct TaskGiveUpArgs {
    /// The id of the task you hold.
    task: String,
    /// Why the task cannot be done.
    reason: String,
}

#[derive(Deserialize, JsonSchema)]
struct WorkNextArgs {
    /// Seconds to wait for an item before answering that there is none. Default 10, at most 30.
    wait_seconds: Option<u64>,
    /// Reviewer tiers you take reviews of, such as `fast` or `deep`. Without one you are given ordinary work only. You are never given a review of work you did.
    review_tiers: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkExtendArgs {
    /// The id of the item you hold.
    item: String,
}

#[derive(Deserialize, JsonSchema)]
struct WorkCompleteArgs {
    /// The id of the item you hold.
    item: String,
    /// What this attempt cost, in millionths of a dollar. The tracker cannot verify it, so it
    /// charges at least its cost floor.
    cost_micros: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkReleaseArgs {
    /// The id of the item you hold.
    item: String,
    /// Why you are handing it back.
    reason: Option<String>,
    /// What this attempt cost, in millionths of a dollar. The tracker charges at least its
    /// cost floor.
    cost_micros: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkPostArgs {
    /// The item you hold, which the new work is created under.
    item: Option<String>,
    /// Your own key for this piece of work, 1 to 64 characters of A-Z, a-z, 0-9, - and _. Sending the same key again returns the first item instead of making a second.
    key: String,
    /// `split`, `augment`, `question` or `test` for work inside this item, `proposal` for a bug or task outside it, or `prerequisite` when you cannot go on until work in another repo is done. A proposal waits for a person to accept it.
    work: String,
    /// What the work is, in a line. Not used with `existing`.
    #[serde(default)]
    title: String,
    /// `prerequisite` only: the source of the repo that needs the work, such as `github`.
    target_source: Option<String>,
    /// `prerequisite` only: the repo that needs the work, as `owner/repo`. It must be one you may reach.
    target_repo: Option<String>,
    /// `prerequisite` only: why you cannot go on without it.
    why: Option<String>,
    /// `prerequisite` only: stream and offset of the steps that show it.
    evidence: Option<Vec<EvidenceArg>>,
    /// `prerequisite` only: the id of an item that already exists, instead of making a new one. Give it and nothing else.
    existing: Option<String>,
    /// `prerequisite` only: `merged` (the default) or `released`, for work you need shipped in a release.
    done_when: Option<String>,
    /// `prerequisite` only: what this attempt cost so far, in millionths of a dollar. The
    /// tracker charges at least its cost floor.
    cost_micros: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkLinkArgs {
    /// The item that must wait.
    item: String,
    /// The item it waits on. Refused if that would make a cycle.
    waits_on: String,
}

#[derive(Deserialize, JsonSchema)]
struct WorkContextArgs {
    /// The item to read or add to. Defaults to the one you hold.
    item: Option<String>,
    /// Give `entry` and `text` to add context; leave both out to read it. The same entry again replaces it.
    entry: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkRulesArgs {
    /// Defaults to the item you hold.
    item: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkProgressArgs {
    /// Defaults to the item you hold.
    item: Option<String>,
    /// A plan or an update.
    note: String,
    branch: Option<String>,
    /// The pull request URL.
    pr: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct DecideArgs {
    /// Defaults to the item you hold.
    item: Option<String>,
    /// `understand`, `reproduce`, `cause`, `fix` or `verify`.
    phase: String,
    /// What you know so far.
    so_far: String,
    /// What you were deciding.
    question: Option<String>,
    options: Option<Vec<String>>,
    chosen: Option<String>,
    /// Why you chose it.
    why: String,
    /// The commit your work stands on.
    commit: String,
    /// The earlier records this rests on.
    evidence: Option<Vec<EvidenceArg>>,
    /// For a fix: the test, and whether it failed with the fix reverted and passed with it restored.
    revert_check: Option<RevertCheckArg>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct RevertCheckArg {
    /// The test's name.
    test: String,
    /// It failed with the fix reverted.
    failed_without_fix: bool,
    /// It passed with the fix restored.
    passed_with_fix: bool,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct EvidenceArg {
    /// The stream. Only `work.events` is taken.
    stream: String,
    /// The offset of an earlier record about an item of the same tree, such as a step you recorded.
    offset: u64,
}

#[derive(Deserialize, JsonSchema)]
struct RecordStepArgs {
    /// Defaults to the item you hold.
    item: Option<String>,
    /// The command you ran.
    command: String,
    exit_code: i32,
    /// Its output. Long output is stored for you.
    output: String,
}

#[derive(Deserialize, JsonSchema)]
struct EditArgs {
    /// Defaults to the item you hold.
    item: Option<String>,
    file: String,
    /// The change, as a diff hunk.
    hunk: String,
    /// The offset `decide` returned for the decision this edit carries out.
    decision: u64,
}

#[derive(Deserialize, JsonSchema)]
struct WorkAskArgs {
    /// Defaults to the item you hold.
    item: Option<String>,
    /// What you need a person to tell you. The item waits for the answer.
    question: String,
    /// Up to 8 short choices (200 bytes each). The person can always type something else instead.
    options: Option<Vec<String>>,
    /// The option taken when the person answers without choosing. Must be one of `options`.
    default: Option<String>,
    /// What this attempt has cost so far, in micro-dollars. Report it here: the answer ends your hold.
    /// The tracker charges at least its cost floor.
    cost_micros: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct WorkAnswerArgs {
    /// The item the question was asked on.
    item: String,
    /// The offset of the question record.
    question: u64,
    /// Free text. Leave it empty when you pick an option.
    answer: String,
    /// One of the options the question offered.
    chosen: Option<String>,
    evidence: Option<Vec<EvidenceArg>>,
}

/// Who the agent is and where it works.
#[derive(Serialize)]
struct RunInfo<'a> {
    run: &'a str,
    agent: &'a str,
    tenant: &'a str,
    namespace: &'a str,
}

#[tool_router]
impl Server {
    #[tool(
        description = "Say which run you are in, your name in it, and the Felix tenant and namespace it lives in."
    )]
    fn run_info(&self) -> String {
        serde_json::to_string(&RunInfo {
            run: self.run.as_str(),
            agent: self.agent.as_str(),
            tenant: &self.felix.tenant,
            namespace: &self.felix.namespace,
        })
        .expect("a struct of strings serializes")
    }

    #[tool(
        description = "Post a task for the workers. Any one worker takes it and its result comes back once."
    )]
    async fn task_post(&self, Parameters(a): Parameters<TaskPostArgs>) -> Result<String, String> {
        self.tasks
            .post(Post {
                id: a.id,
                title: a.title,
                body: a.body,
                parent: a.parent,
                lease: a.lease,
                deadline_ms: a.deadline_ms,
            })
            .await
    }

    #[tool(
        description = "Take the next task, waiting a few seconds if there is none. You keep it while you work and it is extended for you. Finish with task_complete, or hand it back with task_release or task_give_up. You can hold one task at a time."
    )]
    async fn task_claim(&self, Parameters(a): Parameters<TaskClaimArgs>) -> Result<String, String> {
        self.tasks
            .claim(a.wait_seconds.map(Duration::from_secs))
            .await
    }

    #[tool(
        description = "Finish the task you hold with your result. Only the first result for a task counts; the reply says whether yours did."
    )]
    async fn task_complete(
        &self,
        Parameters(a): Parameters<TaskCompleteArgs>,
    ) -> Result<String, String> {
        self.tasks.complete(&a.task, a.result).await
    }

    #[tool(
        description = "Hand the task you hold back so another worker can take it now. It counts as one of the task's two attempts."
    )]
    async fn task_release(
        &self,
        Parameters(a): Parameters<TaskReleaseArgs>,
    ) -> Result<String, String> {
        self.tasks.release(&a.task, a.reason).await
    }

    #[tool(
        description = "Give up on the task you hold for good. It is not handed out again; a person looks at it."
    )]
    async fn task_give_up(
        &self,
        Parameters(a): Parameters<TaskGiveUpArgs>,
    ) -> Result<String, String> {
        self.tasks.give_up(&a.task, a.reason).await
    }

    #[tool(
        description = "Take the highest-priority work item you can start, waiting a few seconds if there is none. The reply carries the standing rules for the item's repo and root, with their version: follow the text and run the checks before you push. You cannot change them. You keep it while you work and it is extended for you. Finish with work_complete, or hand it back with work_release. You can hold one item at a time. Keep taking items: call work_next again after each one, and stop when it says nothing is ready or you are asked to stop."
    )]
    async fn work_next(&self, Parameters(a): Parameters<WorkNextArgs>) -> Result<String, String> {
        self.work()?
            .next_for(
                a.wait_seconds.map(Duration::from_secs),
                a.review_tiers.unwrap_or_default(),
            )
            .await
    }

    #[tool(
        description = "Record one finding of the review you hold: severity, file, line, a title and the scenario that goes wrong. Its verdict decides what follows: `fix` becomes work for the pull request's author, `defer` a proposal a person accepts, `dismiss` is only recorded. Answers with the finding's offset."
    )]
    async fn work_finding(
        &self,
        Parameters(a): Parameters<WorkFindingArgs>,
    ) -> Result<String, String> {
        check_one_of(
            "severity",
            &a.severity,
            &["low", "medium", "high", "critical"],
        )?;
        check_one_of("verdict", &a.verdict, &["fix", "defer", "dismiss"])?;
        let item = self.item_or_held(a.item).await?;
        let reply = self
            .work()?
            .tool(
                "/v1/work/finding",
                json!({
                    "item": item,
                    "severity": a.severity,
                    "file": a.file,
                    "line": a.line,
                    "title": a.title,
                    "scenario": a.scenario,
                    "verdict": a.verdict,
                }),
            )
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Answer the triage item you hold: propose a tier for the item it triages and say why in one line. Name `duplicate_of` if it repeats another item, `depends_on` for items it should wait on, and `split` for the titles it should be cut into. If you cannot settle it, set `unclear` to `scope`, `repo` or `acceptance` and it goes to a person as a question. This is a proposal; a person accepts or edits it. Complete the triage item after."
    )]
    async fn work_triage(
        &self,
        Parameters(a): Parameters<WorkTriageArgs>,
    ) -> Result<String, String> {
        if let Some(u) = &a.unclear {
            check_one_of("unclear", u, &["scope", "repo", "acceptance"])?;
        }
        let item = self.item_or_held(a.item).await?;
        let reply = self
            .work()?
            .tool(
                "/v1/work/triage",
                json!({
                    "item": item,
                    "tier": a.tier,
                    "why": a.why,
                    "duplicate_of": a.duplicate_of,
                    "unclear": a.unclear,
                    "depends_on": a.depends_on.unwrap_or_default(),
                    "split": a.split.unwrap_or_default(),
                }),
            )
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "End the final review you hold with a recommendation, `merge` or `block`, and why. You cannot recommend a merge while a fix a review asked for is open. A person decides whether to merge. Complete the review after."
    )]
    async fn work_verdict(
        &self,
        Parameters(a): Parameters<WorkVerdictArgs>,
    ) -> Result<String, String> {
        check_one_of("verdict", &a.verdict, &["merge", "block"])?;
        let item = self.item_or_held(a.item).await?;
        let reply = self
            .work()?
            .tool(
                "/v1/work/verdict",
                json!({ "item": item, "verdict": a.verdict, "why": a.why }),
            )
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Read the standing rules in effect for an item you hold: the text to follow and the checks to run before you push, with their version. They come with work_next too; call this to see them again. You cannot change them."
    )]
    async fn work_rules(&self, Parameters(a): Parameters<WorkRulesArgs>) -> Result<String, String> {
        let item = self.item_or_held(a.item).await?;
        let reply = self
            .work()?
            .tool("/v1/work/rules/read", json!({ "item": item }))
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Keep the claim on the item you hold standing. work_next already does this every 30 seconds; call it only to check the claim."
    )]
    async fn work_extend(
        &self,
        Parameters(a): Parameters<WorkExtendArgs>,
    ) -> Result<String, String> {
        self.work()?.extend(&a.item).await
    }

    #[tool(
        description = "Finish the item you hold and report what the attempt cost. It is refused while the item has open children."
    )]
    async fn work_complete(
        &self,
        Parameters(a): Parameters<WorkCompleteArgs>,
    ) -> Result<String, String> {
        self.work()?
            .complete(&a.item, a.cost_micros.unwrap_or(0))
            .await
    }

    #[tool(
        description = "Hand the item you hold back so another agent can take it now. It uses one of the item's two tries."
    )]
    async fn work_release(
        &self,
        Parameters(a): Parameters<WorkReleaseArgs>,
    ) -> Result<String, String> {
        self.work()?
            .release(&a.item, a.reason, a.cost_micros.unwrap_or(0))
            .await
    }

    #[tool(
        description = "Create work under the item you hold: split, augment, question or test, live at once and counted against the root's budget. Use `proposal` for a bug or task outside this item; a person decides whether it becomes work. Use `prerequisite` when you cannot go on until work in another repo is done: give target_source, target_repo, title and why, or `existing` for an item that is already there. It ends your claim as blocked without costing a try, and the item comes back to Ready when the work is done (`done_when`: merged or released). While you hold a triage item, a `prerequisite` is for the item you are triaging: that item waits, you keep your claim and `cost_micros` is ignored (report the cost when you complete), so post what it needs from other repos before you answer with work_triage. Sending the same key again is safe."
    )]
    async fn work_post(&self, Parameters(a): Parameters<WorkPostArgs>) -> Result<String, String> {
        let item = self.item_or_held(a.item).await?;
        if a.work != "prerequisite" {
            let reply = self
                .work()?
                .tool(
                    "/v1/work/post",
                    json!({ "item": item, "key": a.key, "work": a.work, "title": a.title }),
                )
                .await?;
            return Ok(reply.to_string());
        }
        let why = match a.why {
            Some(w) => Some(self.content(w).await?),
            None => None,
        };
        let evidence: Vec<_> = a
            .evidence
            .unwrap_or_default()
            .into_iter()
            .map(|e| json!({ "stream": e.stream, "offset": e.offset }))
            .collect();
        // The claim ends with this call unless it is triage, so the keep-alive stops with it.
        self.work()?
            .finish_post(
                &item,
                json!({
                    "item": item, "key": a.key, "work": a.work, "title": a.title,
                    "target_source": a.target_source, "target_repo": a.target_repo,
                    "why": why, "evidence": evidence, "existing": a.existing,
                    "done_when": a.done_when.unwrap_or_else(|| "merged".into()),
                    "cost_micros": a.cost_micros.unwrap_or(0),
                }),
            )
            .await
    }

    #[tool(
        description = "Make one item wait on another. Use it to order the children you posted. Refused if it would make a cycle."
    )]
    async fn work_link(&self, Parameters(a): Parameters<WorkLinkArgs>) -> Result<String, String> {
        let reply = self
            .work()?
            .tool("/v1/work/link", json!({ "item": a.item, "to": a.waits_on }))
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Read what is known about an item (context entries and notes people left), or add a context entry with `entry` and `text`. Read before you start."
    )]
    async fn work_context(
        &self,
        Parameters(a): Parameters<WorkContextArgs>,
    ) -> Result<String, String> {
        let item = self.item_or_held(a.item).await?;
        match (a.entry, a.text) {
            (Some(entry), Some(text)) => {
                let text = self.content(text).await?;
                let reply = self
                    .work()?
                    .tool(
                        "/v1/work/context/add",
                        json!({ "item": item, "entry": entry, "text": text }),
                    )
                    .await?;
                Ok(reply.to_string())
            }
            (None, None) => {
                let mut reply = self
                    .work()?
                    .tool("/v1/work/context", json!({ "item": item }))
                    .await?;
                self.inline_artifacts(&mut reply).await;
                Ok(reply.to_string())
            }
            _ => Err("give both entry and text to add context, or neither to read it".into()),
        }
    }

    #[tool(
        description = "Record a plan, a branch or a pull request on the item you hold, so a person can see how it is going."
    )]
    async fn work_progress(
        &self,
        Parameters(a): Parameters<WorkProgressArgs>,
    ) -> Result<String, String> {
        let item = self.item_or_held(a.item).await?;
        let note = self.content(a.note).await?;
        let reply = self
            .work()?
            .tool(
                "/v1/work/progress",
                json!({ "item": item, "note": note, "branch": a.branch, "pr": a.pr }),
            )
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Record a decision before you act on it: the phase, what you know so far, what you chose and why, and the commit you stand on. A person can fork a redo from it. Answers with the decision's offset, which an edit cites."
    )]
    async fn decide(&self, Parameters(a): Parameters<DecideArgs>) -> Result<String, String> {
        check_phase(&a.phase)?;
        let item = self.item_or_held(a.item).await?;
        let evidence = serde_json::to_value(a.evidence.unwrap_or_default()).unwrap_or(Value::Null);
        let reply = self
            .work()?
            .tool(
                "/v1/work/decide",
                json!({
                    "item": item,
                    "phase": a.phase,
                    "so_far": a.so_far,
                    "question": a.question.unwrap_or_default(),
                    "options": a.options.unwrap_or_default(),
                    "chosen": a.chosen.unwrap_or_default(),
                    "why": a.why,
                    "commit": a.commit,
                    "evidence": evidence,
                    "revert_check": a.revert_check,
                }),
            )
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Record a command you ran: the command, its exit code and its output. Long output is stored for you. Answers with the step's offset."
    )]
    async fn record_step(
        &self,
        Parameters(a): Parameters<RecordStepArgs>,
    ) -> Result<String, String> {
        let item = self.item_or_held(a.item).await?;
        let output = self.content(a.output).await?;
        let reply = self
            .work()?
            .tool(
                "/v1/work/step",
                json!({ "item": item, "command": a.command, "exit_code": a.exit_code, "output": output }),
            )
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Record a code change you made: the file, the hunk, and the decision it carries out. Record the decision first."
    )]
    async fn edit(&self, Parameters(a): Parameters<EditArgs>) -> Result<String, String> {
        let item = self.item_or_held(a.item).await?;
        let hunk = self.content(a.hunk).await?;
        let reply = self
            .work()?
            .tool(
                "/v1/work/edit",
                json!({ "item": item, "file": a.file, "hunk": hunk, "decision": a.decision }),
            )
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Ask a person a question you cannot settle yourself. Offer options when the answer is a choice; they can always type something else. The item waits in Needs you until they answer, then a resume item carrying the question, the answer and your last so_far is open for any agent to claim. Answers with the question's offset."
    )]
    async fn work_ask(&self, Parameters(a): Parameters<WorkAskArgs>) -> Result<String, String> {
        let item = self.item_or_held(a.item).await?;
        let text = self.content(a.question).await?;
        let reply = self
            .work()?
            .tool(
                "/v1/work/ask",
                json!({
                    "item": item,
                    "text": text,
                    "options": a.options.unwrap_or_default(),
                    "default": a.default,
                    "cost_micros": a.cost_micros.unwrap_or(0),
                }),
            )
            .await?;
        Ok(reply.to_string())
    }

    #[tool(
        description = "Answer a question on an item while you hold a `question` item under it. Pick one of its options with `chosen`, or write free text. Cite the records your answer rests on."
    )]
    async fn work_answer(
        &self,
        Parameters(a): Parameters<WorkAnswerArgs>,
    ) -> Result<String, String> {
        let text = self.content(a.answer).await?;
        let evidence = serde_json::to_value(a.evidence.unwrap_or_default()).unwrap_or(Value::Null);
        let reply = self
            .work()?
            .tool(
                "/v1/work/answer",
                json!({
                    "item": a.item,
                    "question": a.question,
                    "text": text,
                    "chosen": a.chosen,
                    "evidence": evidence,
                }),
            )
            .await?;
        Ok(reply.to_string())
    }
}

impl Server {
    /// The item the call names, or the one this agent holds.
    async fn item_or_held(&self, item: Option<String>) -> Result<String, String> {
        match item {
            Some(i) => Ok(i),
            None => self
                .work()?
                .held_item()
                .await
                .ok_or_else(|| "you hold no item; name one or call work_next".to_owned()),
        }
    }

    /// Short text stays in the record; longer text is stored as an artifact first.
    async fn content(&self, text: String) -> Result<Content, String> {
        self.tasks.content(text).await
    }

    /// Replaces each `{"artifact": id}` in a reply with the text it names.
    async fn inline_artifacts(&self, value: &mut Value) {
        let mut ids = Vec::new();
        collect_artifacts(value, &mut ids);
        let mut texts = std::collections::HashMap::new();
        for id in ids {
            let text = self.tasks.artifact_text(&id).await;
            texts.insert(id, text);
        }
        swap_artifacts(value, &texts);
    }

    fn work(&self) -> Result<&Work<Http>, String> {
        self.work.as_ref().ok_or_else(|| {
            "the work tools are not set up: start kbl-mcp with KBL_TRACKER_URL and KBL_TRACKER_TOKEN"
                .to_owned()
        })
    }
}

const PHASES: [&str; 5] = ["understand", "reproduce", "cause", "fix", "verify"];

fn check_phase(phase: &str) -> Result<(), String> {
    if PHASES.contains(&phase) {
        Ok(())
    } else {
        Err(format!(
            "phase is {phase:?}; use one of {}",
            PHASES.join(", ")
        ))
    }
}

fn check_one_of(name: &str, value: &str, allowed: &[&str]) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{name} is {value:?}; use one of {}",
            allowed.join(", ")
        ))
    }
}

fn collect_artifacts(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => match map.get("artifact").and_then(Value::as_str) {
            Some(id) => out.push(id.to_owned()),
            None => map.values().for_each(|v| collect_artifacts(v, out)),
        },
        Value::Array(a) => a.iter().for_each(|v| collect_artifacts(v, out)),
        _ => {}
    }
}

fn swap_artifacts(v: &mut Value, texts: &std::collections::HashMap<String, String>) {
    match v {
        Value::Object(map) => {
            let id = map
                .get("artifact")
                .and_then(Value::as_str)
                .map(str::to_owned);
            match id.and_then(|id| texts.get(&id)) {
                Some(text) => *v = json!({ "text": text }),
                None => map.values_mut().for_each(|v| swap_artifacts(v, texts)),
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|v| swap_artifacts(v, texts)),
        _ => {}
    }
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("kbl-mcp", env!("CARGO_PKG_VERSION")))
    }
}

#[cfg(test)]
mod tests;
