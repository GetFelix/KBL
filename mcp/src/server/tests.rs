use super::*;

#[test]
fn lists_the_run_info_tool_with_a_description() {
    let tools = Server::tool_router().list_all();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "decide",
            "edit",
            "record_step",
            "run_info",
            "task_claim",
            "task_complete",
            "task_give_up",
            "task_post",
            "task_release",
            "work_answer",
            "work_ask",
            "work_complete",
            "work_context",
            "work_extend",
            "work_finding",
            "work_link",
            "work_next",
            "work_post",
            "work_progress",
            "work_release",
            "work_rules",
            "work_triage",
            "work_verdict"
        ]
    );
    assert!(tools.iter().all(|tool| tool.description.is_some()));
}

#[test]
fn a_decision_names_one_of_the_five_phases() {
    for p in PHASES {
        assert!(check_phase(p).is_ok());
    }
    let err = check_phase("guess").unwrap_err();
    assert!(
        err.contains("understand") && err.contains("verify"),
        "{err}"
    );
    assert!(check_phase("").is_err());
}

#[test]
fn a_decision_without_so_far_why_or_commit_is_refused() {
    assert!(check_decision("fix", "found it", "cheapest", "abc123").is_ok());
    assert_eq!(
        check_decision("fix", " ", "cheapest", "abc123").unwrap_err(),
        "refused: missing_field: so_far is needed"
    );
    assert_eq!(
        check_decision("fix", "found it", "", "abc123").unwrap_err(),
        "why is needed"
    );
    // The tracker's code, so a client matching on `missing_field` sees the
    // same refusal whether kbl-mcp or the tracker turned it away.
    let no_commit = check_decision("fix", "found it", "cheapest", "").unwrap_err();
    assert!(
        no_commit.starts_with("refused: missing_field"),
        "{no_commit}"
    );
    assert!(no_commit.contains("commit your work first"), "{no_commit}");
    assert!(check_decision("guess", "found it", "cheapest", "abc123").is_err());
}

#[test]
fn artifacts_in_a_reply_are_swapped_for_their_text() {
    let mut reply = json!({
        "context": [
            { "entry": "log", "text": { "artifact": "b-1" } },
            { "entry": "short", "text": { "text": "kept" } }
        ],
        "notes": [{ "text": { "artifact": "b-2" } }]
    });
    let mut ids = Vec::new();
    collect_artifacts(&reply, &mut ids);
    ids.sort();
    assert_eq!(ids, ["b-1", "b-2"]);
    let texts = [("b-1", "the long log"), ("b-2", "a long note")]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    swap_artifacts(&mut reply, &texts);
    assert_eq!(reply["context"][0]["text"]["text"], "the long log");
    assert_eq!(reply["context"][1]["text"]["text"], "kept");
    assert_eq!(reply["notes"][0]["text"]["text"], "a long note");
}

#[test]
fn a_finding_and_a_verdict_name_one_of_their_words() {
    assert!(check_one_of("verdict", "fix", &["fix", "defer", "dismiss"]).is_ok());
    let err = check_one_of("verdict", "ship", &["merge", "block"]).unwrap_err();
    assert!(err.contains("merge, block"));
}

#[test]
fn every_write_tool_takes_a_request_key_and_sends_it_on() {
    let writes = [
        "decide",
        "edit",
        "record_step",
        "work_answer",
        "work_ask",
        "work_context",
        "work_finding",
        "work_link",
        "work_post",
        "work_progress",
        "work_triage",
        "work_verdict",
    ];
    let tools = Server::tool_router().list_all();
    for name in writes {
        let tool = tools.iter().find(|t| t.name == name).expect(name);
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        assert!(schema["properties"]["request_key"].is_object(), "{name}");
    }
    // The terminal calls replay by claim, so they take none.
    for name in ["work_complete", "work_release", "work_next", "work_extend"] {
        let tool = tools.iter().find(|t| t.name == name).expect(name);
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        assert!(schema["properties"]["request_key"].is_null(), "{name}");
    }
    let sent = with_key(json!({ "item": "a1" }), Some("k-1".into()));
    assert_eq!(sent, json!({ "item": "a1", "request_key": "k-1" }));
    assert_eq!(
        with_key(json!({ "item": "a1" }), None),
        json!({ "item": "a1" })
    );
}
