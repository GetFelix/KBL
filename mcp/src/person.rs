//! The person tools: a person's card actions and My queue, for a person's own
//! MCP host. Docs: docs/work-tracker.md "Person tools over MCP".
//!
//! This is a separate server (`kbl-mcp person`), started with a person's tracker
//! token. The agent server never has these tools and never holds that token.
//! `connect` asks the tracker whom the token belongs to and refuses to build a
//! server for anyone but a person, so an agent's token yields no tools at all.
//! The tracker still checks every route, and writes the person's name on the
//! record; this process only forwards.

use std::sync::Arc;

use kbl_core::artifact::INLINE_LIMIT;
use kbl_core::org::TeamName;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool as McpTool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};

use crate::work::{CallError, TrackerApi, message};
use table::{Kind, Param, Route, TOOLS, Tool};

mod table;

pub(crate) struct Person<T> {
    api: T,
    /// The one team these tools act in. None for a token in a single team.
    team: Option<TeamName>,
    tools: Vec<McpTool>,
}

impl<T: TrackerApi> Person<T> {
    /// Fails unless the tracker says the token is a person's, and in `team`
    /// when one is named. The only way to get a `Person`.
    pub(crate) async fn connect(api: T, team: Option<TeamName>) -> Result<Self, String> {
        let me = api.get("/v1/me").await.map_err(|e| match e {
            CallError::Down(m) => m,
            CallError::Refused(m) | CallError::Lost(m) => format!(
                "the tracker did not take this as a person's token ({m}). Person tools need a person's token; an agent's token is for `kbl-mcp` without `person`"
            ),
        })?;
        if me["person"].as_str().is_none() {
            return Err("the tracker did not name a person for this token".into());
        }
        if let Some(t) = &team {
            let in_team = me["teams"]
                .as_array()
                .is_some_and(|ts| ts.iter().any(|x| x["team"] == t.as_str()));
            if !in_team {
                return Err(format!("this token is not in team {t}"));
            }
        }
        Ok(Self {
            api,
            team,
            tools: TOOLS.iter().map(describe).collect(),
        })
    }

    pub(crate) fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Runs the named tool. None when there is no such tool.
    pub(crate) async fn run(
        &self,
        name: &str,
        args: Option<Map<String, Value>>,
    ) -> Option<Result<String, String>> {
        let tool = TOOLS.iter().find(|t| t.name == name)?;
        Some(self.call(tool, args.unwrap_or_default()).await)
    }

    async fn call(&self, tool: &Tool, args: Map<String, Value>) -> Result<String, String> {
        let keyed = matches!(tool.route, Route::Post(_));
        let body = check(tool.params, args, keyed)?;
        // An edit of nothing would still append a record.
        if tool.name == "edit_proposal"
            && !["title", "tier", "depends_on", "split"]
                .iter()
                .any(|k| body.contains_key(*k))
        {
            return Err("give at least one of title, tier, depends_on or split".into());
        }
        let reply = match tool.route {
            Route::Post(path) => self.api.call(&self.path(path), Value::Object(body)).await,
            Route::Get(path) => {
                let mut path = self.path(path);
                if let Some(limit) = body.get("limit") {
                    path.push_str(&format!("?limit={limit}"));
                }
                self.api.get(&path).await
            }
        };
        reply.map(|r| r.to_string()).map_err(message)
    }

    /// `/v1/work/x` as is, or in the named team.
    fn path(&self, route: &str) -> String {
        match &self.team {
            Some(t) => route.replacen("/v1/", &format!("/v1/teams/{t}/"), 1),
            None => route.to_owned(),
        }
    }
}

/// Checks the arguments against the entry and builds the body the route takes.
/// A post may also carry a `request_key`.
fn check(
    params: &[Param],
    mut args: Map<String, Value>,
    keyed: bool,
) -> Result<Map<String, Value>, String> {
    let mut body = Map::new();
    for p in params {
        match args.remove(p.name).filter(|v| !v.is_null()) {
            None if p.required => return Err(format!("{} is needed", p.name)),
            None => {}
            Some(v) => {
                body.insert(p.name.to_owned(), convert(p, v)?);
            }
        }
    }
    if keyed && let Some(k) = args.remove("request_key").filter(|v| !v.is_null()) {
        if !k.is_string() {
            return Err("request_key must be a string".into());
        }
        body.insert("request_key".into(), k);
    }
    if let Some(extra) = args.keys().next() {
        return Err(format!("{extra} is not a parameter of this tool"));
    }
    Ok(body)
}

fn convert(p: &Param, v: Value) -> Result<Value, String> {
    let wrong = |what: &str| format!("{} must be {what}", p.name);
    match p.kind {
        Kind::Text => v.is_string().then_some(v).ok_or_else(|| wrong("a string")),
        Kind::List => match v.as_array() {
            Some(a) if a.iter().all(Value::is_string) => Ok(v),
            _ => Err(wrong("a list of strings")),
        },
        Kind::Number => v
            .as_u64()
            .map(Value::from)
            .ok_or_else(|| wrong("a whole number from 0")),
        Kind::Bool => v
            .is_boolean()
            .then_some(v)
            .ok_or_else(|| wrong("true or false")),
        Kind::Words(words) => match v.as_str() {
            Some(w) if words.contains(&w) => Ok(v),
            _ => Err(wrong(&format!("one of {}", words.join(", ")))),
        },
        Kind::Content => match v.as_str() {
            Some(t) if t.len() <= INLINE_LIMIT => Ok(json!({ "text": t })),
            Some(_) => Err(format!("{} is longer than {INLINE_LIMIT} bytes", p.name)),
            None => Err(wrong("a string")),
        },
        Kind::Texts => match v.as_array() {
            Some(a) if a.iter().all(Value::is_string) => Ok(v),
            _ => Err(wrong("a list of strings")),
        },
        Kind::Assignee => match v.as_str().map(|s| s.strip_prefix("agent:")) {
            Some(Some(agent)) if !agent.is_empty() => Ok(json!({ "agent": agent })),
            Some(None) if v.as_str().is_some_and(|n| !n.is_empty()) => Ok(json!({ "person": v })),
            _ => Err(wrong("a person's name, or agent:<name>")),
        },
    }
}

fn describe(tool: &Tool) -> McpTool {
    let mut props = Map::new();
    let mut required = Vec::new();
    for p in tool.params {
        let mut s = match p.kind {
            Kind::Text | Kind::Content | Kind::Assignee => json!({ "type": "string" }),
            Kind::Number => json!({ "type": "integer", "minimum": 0 }),
            Kind::Bool => json!({ "type": "boolean" }),
            Kind::List => json!({ "type": "array", "items": { "type": "string" } }),
            Kind::Words(w) => json!({ "type": "string", "enum": w }),
            Kind::Texts => json!({ "type": "array", "items": { "type": "string" } }),
        };
        s["description"] = p.about.into();
        props.insert(p.name.to_owned(), s);
        if p.required {
            required.push(p.name);
        }
    }
    if matches!(tool.route, Route::Post(_)) {
        props.insert(
            "request_key".into(),
            json!({
                "type": "string",
                "description": "Your own key for this write, 1 to 64 characters of A-Z, a-z, 0-9, - and _. If a call may have landed and you did not see the answer, send it again with the same key and the same arguments: the tracker answers with the first write instead of writing twice."
            }),
        );
    }
    let schema = json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    });
    let Value::Object(schema) = schema else {
        unreachable!("a json object literal")
    };
    McpTool::new(tool.name, tool.about, Arc::new(schema))
}

impl<T: TrackerApi> ServerHandler for Person<T> {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: self.tools.clone(),
            ..ListToolsResult::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let started = std::time::Instant::now();
        let name = request.name.to_string();
        let ran = self.run(&name, request.arguments).await;
        let result = match ran {
            None => Err(ErrorData::invalid_params(
                format!("no tool named {name}"),
                None,
            )),
            Some(Ok(text)) => Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                ContentBlock::text(text),
            ]))),
            Some(Err(text)) => Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                ContentBlock::text(text),
            ]))),
        };
        crate::metrics::tool_finished(&name, result.is_ok(), started.elapsed(), &result);
        result
    }

    fn get_tool(&self, name: &str) -> Option<McpTool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("kbl-mcp-person", env!("CARGO_PKG_VERSION")),
        )
    }
}

#[cfg(test)]
mod tests;
