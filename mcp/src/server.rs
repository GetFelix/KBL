//! The MCP server and the tools it lists.

use std::time::Duration;

use agents_core::names::{AgentName, Resources, RunId};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::felix::Felix;
use crate::tasks::{Post, Tasks};

#[derive(Clone)]
pub(crate) struct Server {
    run: RunId,
    agent: AgentName,
    felix: Felix,
    tasks: Tasks<Felix>,
}

impl Server {
    pub(crate) fn new(run: RunId, agent: AgentName, felix: Felix) -> Self {
        let tasks = Tasks::new(felix.clone(), Resources::new(run.clone()), agent.clone());
        Self {
            run,
            agent,
            felix,
            tasks,
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
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("felix-mcp", env!("CARGO_PKG_VERSION")))
    }
}

#[cfg(test)]
mod tests;
