//! The task tools: how an agent posts, claims, finishes and gives back tasks.
//! See "Handing out tasks" in docs/design.md.
//!
//! Every step goes on the run log, refusals included. A failed log write is
//! reported on stderr and does not fail the tool: the claim or the result it
//! describes already happened, and failing would tell the model it did not.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agents_core::artifact::{ArtifactId, PART_SIZE};
use agents_core::codec::Codec;
use agents_core::content::Content;
use agents_core::names::{AgentName, Key, Resources, TaskId, WORKERS_GROUP};
use agents_core::record::{Logged, Record};
use agents_core::stored::TaskResult;
use agents_core::task::Task;
use felix_client::{GroupMember, GroupRecord};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::felix::{Backend, ExtendError};
use crate::keepalive::{self, Claim, Health, at_risk};

/// How long a claim stands without an extension.
const VISIBILITY: Duration = Duration::from_secs(90);
/// How often a held claim is extended. Three tries fit in one visibility.
const EXTEND_EVERY: Duration = Duration::from_secs(30);
/// How long `task_claim` waits for work when the agent does not say.
const DEFAULT_WAIT: Duration = Duration::from_secs(10);
/// Longest a single `task_claim` waits, so a host's tool timeout is not hit.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// Deliveries of one task a worker takes on. Felix's own limit is broker-wide
/// and defaults to 5, too many for a task that costs model calls (gap 1).
const MAX_ATTEMPTS: u32 = 2;

pub(crate) struct Post {
    pub(crate) id: Option<String>,
    pub(crate) title: String,
    pub(crate) body: Option<String>,
    pub(crate) parent: Option<String>,
    pub(crate) lease: Option<String>,
    pub(crate) deadline_ms: Option<u64>,
}

/// The one task this agent holds, and the loop that keeps its claim alive.
struct Held {
    task: Task,
    record: GroupRecord,
    attempt: u32,
    extender: JoinHandle<()>,
    health: Arc<StdMutex<Health>>,
}

struct Inner<B> {
    backend: B,
    resources: Resources,
    agent: AgentName,
    /// Also taken for the whole of a claim, so two calls cannot both take a task.
    held: Mutex<Option<Held>>,
    visibility: Duration,
    extend_every: Duration,
}

pub(crate) struct Tasks<B>(Arc<Inner<B>>);

impl<B> Clone for Tasks<B> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<B: Backend> Tasks<B> {
    pub(crate) fn new(
        backend: B,
        resources: Resources,
        agent: AgentName,
        visibility: Option<Duration>,
        extend_every: Option<Duration>,
    ) -> Self {
        Self::with_timing(
            backend,
            resources,
            agent,
            visibility.unwrap_or(VISIBILITY),
            extend_every.unwrap_or(EXTEND_EVERY),
        )
    }

    fn with_timing(
        backend: B,
        resources: Resources,
        agent: AgentName,
        visibility: Duration,
        extend_every: Duration,
    ) -> Self {
        Self(Arc::new(Inner {
            backend,
            resources,
            agent,
            held: Mutex::new(None),
            visibility,
            extend_every,
        }))
    }

    pub(crate) async fn post(&self, post: Post) -> Result<String, String> {
        let given = post.id.is_some();
        let id = match post.id {
            Some(id) => TaskId::new(id),
            None => TaskId::new(format!("t-{:x}-{}", now_ms(), next_seq())),
        }
        .map_err(|e| e.to_string())?;
        if given && self.result_of(&id).await?.is_some() {
            return Err(format!("task {id} already has a result; use a new id"));
        }
        let parent = TaskId::new(post.parent.unwrap_or_else(|| "root".into()))
            .map_err(|e| format!("parent: {e}"))?;
        let lease = post
            .lease
            .map(Key::new)
            .transpose()
            .map_err(|e| format!("lease: {e}"))?;
        let body = match post.body {
            Some(text) => Some(self.content(text).await?),
            None => None,
        };
        let task = Task {
            id: id.clone(),
            parent: parent.clone(),
            title: post.title.clone(),
            body,
            lease,
            deadline_ms: post.deadline_ms,
        };
        let stream = self.0.resources.tasks();
        let offset = self
            .checked(
                "publish",
                &stream,
                None,
                self.0.backend.publish(&stream, task.encode()),
            )
            .await?;
        self.log(Record::TaskPosted {
            task: id.clone(),
            parent,
            title: post.title,
            offset,
        })
        .await;
        Ok(json!({ "task": id, "offset": offset }).to_string())
    }

    pub(crate) async fn claim(&self, wait: Option<Duration>) -> Result<String, String> {
        let wait = wait.unwrap_or(DEFAULT_WAIT).min(MAX_WAIT);
        let deadline = Instant::now() + wait;
        let mut held = self.0.held.lock().await;
        if let Some(lost) = lost(&mut held) {
            return Err(lost);
        }
        if let Some(h) = held.as_ref() {
            let mut msg = format!(
                "you already hold task {}; complete, release or give it up first",
                h.task.id
            );
            if let Some(risk) = at_risk(&h.health) {
                msg.push_str(&format!(". {risk}"));
            }
            return Err(msg);
        }
        let stream = self.0.resources.tasks();
        // The broker acts on `reclaim` in a connection's first poll only, so
        // it is safe to send every time.
        let member = GroupMember {
            consumer: self.0.agent.to_string(),
            reclaim: true,
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let polled = self
                .checked(
                    "group_poll",
                    &stream,
                    None,
                    self.0.backend.poll(
                        &stream,
                        WORKERS_GROUP,
                        &member,
                        self.0.visibility,
                        remaining,
                    ),
                )
                .await?;
            let Some(record) = polled else {
                return Ok(json!({ "claimed": false }).to_string());
            };
            let task = match Task::decode(&record.payload) {
                Ok(task) => task,
                Err(e) => {
                    self.dead_letter(record.offset).await?;
                    return Err(format!(
                        "dead-lettered the record at offset {}, which is not a task: {e}",
                        record.offset
                    ));
                }
            };
            // A redelivery of a finished task: settle it without a model run.
            if let Some(winner) = self.result_of(&task.id).await.ok().flatten() {
                self.checked(
                    "group_ack",
                    &stream,
                    None,
                    self.0.backend.ack(&stream, WORKERS_GROUP, record.offset),
                )
                .await?;
                if let Ok(winner) = TaskResult::decode(&winner) {
                    self.log(Record::TaskDuplicate {
                        task: task.id,
                        offset: record.offset,
                        attempt: record.attempts,
                        result: winner.artifact,
                    })
                    .await;
                }
                continue;
            }
            // 0 is a broker that did not say. Counting it as a first delivery
            // would let the attempt limit never fire.
            if record.attempts == 0 {
                self.dead_letter(record.offset).await?;
                self.log(Record::TaskDead {
                    task: task.id.clone(),
                    offset: record.offset,
                    attempt: 0,
                    reason: "the broker did not report an attempt count".into(),
                })
                .await;
                return Err(format!(
                    "dead-lettered task {}: the broker did not report how many times it was delivered",
                    task.id
                ));
            }
            let attempt = record.attempts;
            if attempt > MAX_ATTEMPTS {
                self.dead_letter(record.offset).await?;
                self.log(Record::TaskDead {
                    task: task.id,
                    offset: record.offset,
                    attempt,
                    reason: format!("attempts used up after {MAX_ATTEMPTS}"),
                })
                .await;
                continue;
            }
            self.log(Record::TaskClaimed {
                task: task.id.clone(),
                offset: record.offset,
                attempt,
                deadline_ms: now_ms() + self.0.visibility.as_millis() as u64,
            })
            .await;
            let shown = self.show(&task).await;
            let offset = record.offset;
            let health = Health::fresh();
            let extender = tokio::spawn(keepalive::keep_alive(
                TaskClaim {
                    tasks: self.clone(),
                    task: task.id.clone(),
                    record: record.clone(),
                    attempt,
                },
                self.0.extend_every,
                self.0.visibility,
                health.clone(),
            ));
            *held = Some(Held {
                task,
                record,
                attempt,
                extender,
                health,
            });
            return Ok(json!({
                "claimed": true,
                "task": shown,
                "attempt": attempt,
                "attempts_allowed": MAX_ATTEMPTS,
                "offset": offset,
            })
            .to_string());
        }
    }

    pub(crate) async fn complete(&self, task: &str, result: String) -> Result<String, String> {
        let mut held = self.0.held.lock().await;
        let h = holding(&held, task)?;
        let risk = at_risk(&h.health);
        // A lost claim still gets its result written, which is safe because
        // only the first write lands. The delivery is no longer ours to ack.
        let claim_lost = h.health.lock().expect("health lock").lost;
        let (id, offset, attempt) = (h.task.id.clone(), h.record.offset, h.attempt);
        let artifact = self.store(result.into_bytes()).await?;
        let value = TaskResult {
            attempt,
            agent: self.0.agent.clone(),
            artifact: artifact.clone(),
        };
        let results = self.0.resources.results();
        let won = self
            .checked(
                "cache_put_if",
                &results,
                Some(id.as_str()),
                self.0
                    .backend
                    .cache_put_absent(&results, id.as_str(), value.encode()),
            )
            .await?;
        let h = held.take().expect("checked above");
        h.extender.abort();
        // Acked either way: a redelivered copy that lost has nothing left to do.
        let stream = self.0.resources.tasks();
        let acked = if claim_lost {
            Ok(())
        } else {
            self.checked(
                "group_ack",
                &stream,
                None,
                self.0.backend.ack(&stream, WORKERS_GROUP, offset),
            )
            .await
        };
        self.log(if won {
            Record::TaskDone {
                task: id.clone(),
                offset,
                attempt,
                result: artifact.clone(),
            }
        } else {
            Record::TaskDuplicate {
                task: id.clone(),
                offset,
                attempt,
                result: artifact.clone(),
            }
        })
        .await;
        let mut reply = json!({
            "task": id,
            "status": if won { "done" } else { "duplicate" },
            "result": artifact,
        });
        if won {
            reply["note"] = "your result is the task's result".into();
        } else {
            reply["note"] =
                "another copy of this task finished first; yours is kept but is not the result"
                    .into();
        }
        if claim_lost {
            reply["warning"] = if won {
                "your claim had lapsed, but your result landed first and is the task's result"
            } else {
                "your claim had lapsed and another copy finished first"
            }
            .into();
        } else if let Err(e) = acked {
            reply["warning"] =
                format!("the result is recorded but the task was not acknowledged: {e}").into();
        } else if let Some(risk) = risk {
            reply["warning"] = risk.into();
        }
        Ok(reply.to_string())
    }

    pub(crate) async fn release(
        &self,
        task: &str,
        reason: Option<String>,
    ) -> Result<String, String> {
        let mut held = self.0.held.lock().await;
        if let Some(lost) = lost(&mut held) {
            return Err(lost);
        }
        let h = holding(&held, task)?;
        let risk = at_risk(&h.health);
        let stream = self.0.resources.tasks();
        self.checked(
            "group_nack",
            &stream,
            None,
            self.0.backend.nack(&stream, WORKERS_GROUP, &h.record),
        )
        .await?;
        let h = held.take().expect("checked above");
        h.extender.abort();
        self.log(Record::TaskReleased {
            task: h.task.id.clone(),
            offset: h.record.offset,
            attempt: h.attempt,
            reason,
        })
        .await;
        Ok(with_warning(
            json!({ "task": h.task.id, "status": "released" }),
            risk,
        ))
    }

    pub(crate) async fn give_up(&self, task: &str, reason: String) -> Result<String, String> {
        let mut held = self.0.held.lock().await;
        if let Some(lost) = lost(&mut held) {
            return Err(lost);
        }
        let h = holding(&held, task)?;
        let risk = at_risk(&h.health);
        let stream = self.0.resources.tasks();
        self.checked(
            "group_dead_letter",
            &stream,
            None,
            self.0
                .backend
                .dead_letter(&stream, WORKERS_GROUP, h.record.offset),
        )
        .await?;
        let h = held.take().expect("checked above");
        h.extender.abort();
        self.log(Record::TaskDead {
            task: h.task.id.clone(),
            offset: h.record.offset,
            attempt: h.attempt,
            reason,
        })
        .await;
        Ok(with_warning(
            json!({ "task": h.task.id, "status": "dead" }),
            risk,
        ))
    }

    async fn dead_letter(&self, offset: u64) -> Result<(), String> {
        let stream = self.0.resources.tasks();
        self.checked(
            "group_dead_letter",
            &stream,
            None,
            self.0.backend.dead_letter(&stream, WORKERS_GROUP, offset),
        )
        .await
    }

    /// The results cache's value for `task`, when a result has been written.
    async fn result_of(&self, task: &TaskId) -> Result<Option<Vec<u8>>, String> {
        let results = self.0.resources.results();
        self.checked(
            "cache_get",
            &results,
            Some(task.as_str()),
            self.0.backend.cache_get(&results, task.as_str()),
        )
        .await
    }

    /// Short text stays inline; longer text becomes an artifact.
    pub(crate) async fn content(&self, text: String) -> Result<Content, String> {
        let (content, stored) = Content::from_text(text);
        if let Some((_, bytes)) = stored {
            self.store(bytes).await?;
        }
        Ok(content)
    }

    /// The text of an artifact, or a line saying it could not be read.
    pub(crate) async fn artifact_text(&self, id: &str) -> String {
        let blobs = self.0.resources.blobs();
        match self.0.backend.cache_get(&blobs, id).await {
            Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
            _ => format!("(artifact {id} could not be read)"),
        }
    }

    /// Stores `bytes` in the artifacts cache under their hash.
    async fn store(&self, bytes: Vec<u8>) -> Result<ArtifactId, String> {
        if bytes.len() > PART_SIZE {
            return Err(format!(
                "{} bytes is over the {PART_SIZE} byte limit",
                bytes.len()
            ));
        }
        let id = ArtifactId::of(&bytes);
        let blobs = self.0.resources.blobs();
        self.checked(
            "cache_put",
            &blobs,
            Some(id.as_str()),
            self.0.backend.cache_put(&blobs, id.as_str(), bytes),
        )
        .await?;
        Ok(id)
    }

    /// A task as the model sees it, with an artifact body read back as text.
    async fn show(&self, task: &Task) -> Value {
        let body = match &task.body {
            Some(Content::Text(text)) => Some(text.clone()),
            Some(Content::Artifact(id)) => {
                let blobs = self.0.resources.blobs();
                let got = self.0.backend.cache_get(&blobs, id.as_str()).await;
                Some(match got {
                    Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
                    _ => format!("(the body is artifact {id}, which could not be read)"),
                })
            }
            None => None,
        };
        json!({
            "id": task.id,
            "parent": task.parent,
            "title": task.title,
            "body": body,
            "lease": task.lease,
            "deadline_ms": task.deadline_ms,
        })
    }

    /// Runs a Felix operation. A failure goes on the run log as a refusal and
    /// comes back as the text the model sees.
    async fn checked<T>(
        &self,
        op: &str,
        resource: &str,
        key: Option<&str>,
        call: impl Future<Output = anyhow::Result<T>>,
    ) -> Result<T, String> {
        match call.await {
            Ok(value) => Ok(value),
            Err(e) => {
                self.refuse(op, resource, key, &e).await;
                Err(format!("{op} on {resource} failed: {e:#}"))
            }
        }
    }

    async fn refuse(&self, op: &str, resource: &str, key: Option<&str>, error: &anyhow::Error) {
        self.log(Record::Refused {
            op: op.into(),
            resource: resource.into(),
            key: key.map(str::to_owned),
            error: format!("{error:#}"),
        })
        .await;
    }

    async fn log(&self, record: Record) {
        let logged = Logged {
            at_ms: now_ms(),
            by: Key::new(self.0.agent.as_str()).expect("an agent name is a key"),
            record,
        };
        let stream = self.0.resources.log();
        if let Err(e) = self.0.backend.publish(&stream, logged.encode()).await {
            eprintln!("felix-mcp: could not write to the run log: {e:#}");
        }
    }
}

fn holding<'a>(held: &'a Option<Held>, task: &str) -> Result<&'a Held, String> {
    match held {
        Some(h) if h.task.id.as_str() == task => Ok(h),
        Some(h) => Err(format!("you hold task {}, not {task}", h.task.id)),
        None => Err("you do not hold a task; claim one first".into()),
    }
}

/// If the held task's claim was lost, drops the hold and says so once.
fn lost(held: &mut Option<Held>) -> Option<String> {
    let gone = held
        .as_ref()
        .is_some_and(|h| h.health.lock().expect("health lock").lost);
    if !gone {
        return None;
    }
    let h = held.take()?;
    h.extender.abort();
    Some(format!(
        "your claim on task {} lapsed and Felix may have given it to another worker. Stop working on it; \
         use task_claim for new work",
        h.task.id
    ))
}

fn with_warning(mut reply: Value, warning: Option<&str>) -> String {
    if let Some(w) = warning {
        reply["warning"] = w.into();
    }
    reply.to_string()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn next_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A held task's claim, extended on Felix.
struct TaskClaim<B> {
    tasks: Tasks<B>,
    task: TaskId,
    record: GroupRecord,
    attempt: u32,
}

impl<B: Backend> Claim for TaskClaim<B> {
    async fn extend(&self) -> Result<Duration, ExtendError> {
        let inner = &self.tasks.0;
        inner
            .backend
            .extend(
                &inner.resources.tasks(),
                WORKERS_GROUP,
                &self.record,
                inner.visibility,
            )
            .await
    }

    async fn extended(&self, visible: Duration) {
        self.tasks
            .log(Record::TaskExtended {
                task: self.task.clone(),
                offset: self.record.offset,
                attempt: self.attempt,
                deadline_ms: now_ms() + visible.as_millis() as u64,
            })
            .await;
    }

    async fn failed(&self, error: &anyhow::Error) {
        let stream = self.tasks.0.resources.tasks();
        self.tasks
            .refuse("group_extend", &stream, None, error)
            .await;
    }

    async fn lost(&self) {
        self.tasks
            .log(Record::TaskClaimLost {
                task: self.task.clone(),
                offset: self.record.offset,
                attempt: self.attempt,
            })
            .await;
    }
}

#[cfg(test)]
mod tests;
