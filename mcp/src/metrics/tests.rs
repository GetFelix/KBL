use rmcp::model::{CallToolResult, ContentBlock};

use super::*;

fn read(name: &str, has: &[&str]) -> f64 {
    let text = kbl_metrics::install("kbl-mcp-test").render();
    text.lines()
        .filter(|l| l.starts_with(&format!("{name}{{")) && has.iter().all(|h| l.contains(h)))
        .filter_map(|l| l.rsplit_once(' ')?.1.parse::<f64>().ok())
        .sum()
}

#[test]
fn calls_are_counted_by_tool_and_outcome_and_unknown_names_share_one_label() {
    let calls = |tool: &str, outcome: &str| {
        read(
            "kbl_mcp_tool_calls_total",
            &[
                &format!("tool=\"{tool}\""),
                &format!("outcome=\"{outcome}\""),
            ],
        )
    };
    let (ok, err, proto, unknown) = (
        calls("work_next", "ok"),
        calls("work_next", "error"),
        calls("work_next", "protocol_error"),
        calls("unknown", "protocol_error"),
    );
    let took = Duration::from_millis(30);
    let done: Result<CallToolResponse, ErrorData> =
        Ok(CallToolResult::success(vec![ContentBlock::text("secret-claim-text")]).into());
    let failed: Result<CallToolResponse, ErrorData> =
        Ok(CallToolResult::error(vec![ContentBlock::text("secret-claim-text")]).into());
    let refused: Result<CallToolResponse, ErrorData> =
        Err(ErrorData::invalid_params("tool not found", None));
    tool_finished("work_next", true, took, &done);
    tool_finished("work_next", true, took, &failed);
    tool_finished("work_next", true, took, &refused);
    tool_finished("made-up-by-a-client", false, took, &refused);

    assert_eq!(calls("work_next", "ok") - ok, 1.0);
    assert_eq!(calls("work_next", "error") - err, 1.0);
    assert_eq!(calls("work_next", "protocol_error") - proto, 1.0);
    assert_eq!(calls("unknown", "protocol_error") - unknown, 1.0);
    let text = kbl_metrics::install("kbl-mcp-test").render();
    assert!(!text.contains("made-up-by-a-client") && !text.contains("secret-claim-text"));
    assert!(read("kbl_mcp_tool_errors_total", &["tool=\"work_next\""]) >= 2.0);
    assert!(read("kbl_mcp_tool_seconds_count", &["tool=\"work_next\""]) >= 3.0);
}
