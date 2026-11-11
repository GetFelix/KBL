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
            "work_link",
            "work_next",
            "work_post",
            "work_progress",
            "work_release"
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
