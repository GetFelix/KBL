//! The MCP server and the tools it lists.

use agents_core::names::{AgentName, RunId};
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde::Serialize;

use crate::felix::Felix;

#[derive(Clone)]
pub(crate) struct Server {
    run: RunId,
    agent: AgentName,
    felix: Felix,
}

impl Server {
    pub(crate) fn new(run: RunId, agent: AgentName, felix: Felix) -> Self {
        Self { run, agent, felix }
    }
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
