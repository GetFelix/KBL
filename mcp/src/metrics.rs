//! MCP tool metrics. The `tool` label is a name from the server's own tool
//! list; a call to any other name is counted as `unknown`, so a client cannot
//! mint labels. Arguments and results are never recorded.

use std::time::Duration;

use rmcp::ErrorData;
use rmcp::model::CallToolResponse;

/// Counts a finished call and times it. `known` is whether the router has the tool.
pub(crate) fn tool_finished(
    tool: &str,
    known: bool,
    took: Duration,
    result: &Result<CallToolResponse, ErrorData>,
) {
    let tool = if known {
        tool.to_owned()
    } else {
        "unknown".to_owned()
    };
    let outcome = match result {
        Ok(CallToolResponse::Complete(r)) if r.is_error == Some(true) => "error",
        Ok(_) => "ok",
        Err(_) => "protocol_error",
    };
    metrics::counter!("kbl_mcp_tool_calls_total", "tool" => tool.clone(), "outcome" => outcome)
        .increment(1);
    metrics::histogram!("kbl_mcp_tool_seconds", "tool" => tool.clone()).record(took.as_secs_f64());
    if outcome != "ok" {
        metrics::counter!("kbl_mcp_tool_errors_total", "tool" => tool, "outcome" => outcome)
            .increment(1);
    }
}

#[cfg(test)]
mod tests;
