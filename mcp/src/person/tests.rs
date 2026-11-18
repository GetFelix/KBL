use std::sync::{Arc, Mutex};

use super::*;
use crate::work::CallError;

/// A path and the body it was posted, or none for a read.
type Seen = (String, Option<Value>);

/// A tracker that knows one person token and one agent token, and keeps what it was asked.
#[derive(Clone, Default)]
struct Fake {
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Who `/v1/me` names. None answers as it does for an agent's token.
    person: Option<&'static str>,
    teams: Vec<&'static str>,
}

impl Fake {
    fn person() -> Self {
        Self {
            person: Some("gab"),
            teams: vec!["a"],
            ..Self::default()
        }
    }

    fn agent() -> Self {
        Self::default()
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl TrackerApi for Fake {
    async fn call(&self, path: &str, body: Value) -> Result<Value, CallError> {
        self.seen
            .lock()
            .unwrap()
            .push((path.to_owned(), Some(body)));
        match self.person {
            Some(_) => Ok(json!({ "by": "gab" })),
            None => Err(CallError::Refused(
                "this call is for a person's token".into(),
            )),
        }
    }

    async fn get(&self, path: &str) -> Result<Value, CallError> {
        self.seen.lock().unwrap().push((path.to_owned(), None));
        match (path, self.person) {
            ("/v1/me", Some(p)) => {
                let teams: Vec<_> = self.teams.iter().map(|t| json!({ "team": t })).collect();
                Ok(json!({ "person": p, "teams": teams }))
            }
            (_, Some(_)) => Ok(json!({ "count": 0, "queue": [] })),
            (_, None) => Err(CallError::Refused(
                "this call is for a person's token".into(),
            )),
        }
    }
}

async fn person(fake: &Fake) -> Person<Fake> {
    Person::connect(fake.clone(), None).await.unwrap()
}

#[tokio::test]
async fn an_agent_token_gets_no_person_server() {
    let fake = Fake::agent();
    let Err(e) = Person::connect(fake.clone(), None).await else {
        panic!("an agent's token built a person server");
    };
    assert!(e.contains("person's token"), "{e}");
    // It asked who the token is and did nothing else.
    assert_eq!(fake.seen(), [("/v1/me".to_owned(), None)]);
}

#[tokio::test]
async fn a_person_token_lists_every_tool() {
    let p = person(&Fake::person()).await;
    let names: Vec<_> = p.tools().iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "my_queue",
            "accept_proposal",
            "reject_proposal",
            "answer_question",
            "set_tier",
            "flag_item",
            "unflag_item",
            "add_note",
            "retry_item",
            "snooze_item",
            "block_item",
            "lift_block",
            "park_item",
            "unpark_item",
            "unblock_item",
            "relate_items",
            "unrelate_items",
            "request_review",
            "request_changes",
            "mark_merged",
            "rate_summary",
            "take_item",
            "reassign_item",
            "steer_agent",
            "close_item",
            "edit_proposal",
        ]
    );
    assert!(p.tools().iter().all(|t| t.description.is_some()));
    let snooze = p.get_tool("snooze_item").unwrap();
    assert_eq!(snooze.input_schema["required"], json!(["item", "until_ms"]));
    assert_eq!(
        snooze.input_schema["properties"]["until_ms"]["type"],
        "integer"
    );
}

#[tokio::test]
async fn the_agent_tools_are_not_here() {
    let p = person(&Fake::person()).await;
    for name in ["work_next", "work_complete", "task_post", "run_info"] {
        assert!(p.run(name, None).await.is_none(), "{name}");
        assert!(p.get_tool(name).is_none(), "{name}");
    }
}

#[tokio::test]
async fn each_tool_posts_to_its_route_with_its_body() {
    let cases: [(&str, Value, &str, Value); 27] = [
        (
            "accept_proposal",
            json!({ "item": "a1", "tier": 2 }),
            "/v1/work/proposals/accept",
            json!({ "item": "a1", "tier": 2 }),
        ),
        (
            "reject_proposal",
            json!({ "item": "a1" }),
            "/v1/work/proposals/reject",
            json!({ "item": "a1" }),
        ),
        (
            "answer_question",
            json!({ "item": "a1", "chosen": "yes", "question": 4 }),
            "/v1/work/questions/answer",
            json!({ "item": "a1", "chosen": "yes", "question": 4 }),
        ),
        (
            "set_tier",
            json!({ "item": "a1", "tier": 0 }),
            "/v1/work/tier",
            json!({ "item": "a1", "tier": 0 }),
        ),
        (
            "flag_item",
            json!({ "item": "a1" }),
            "/v1/work/flag",
            json!({ "item": "a1" }),
        ),
        (
            "unflag_item",
            json!({ "item": "a1" }),
            "/v1/work/unflag",
            json!({ "item": "a1" }),
        ),
        (
            "add_note",
            json!({ "item": "a1", "text": "look at the lock", "input": "review_comment" }),
            "/v1/work/note",
            json!({ "item": "a1", "text": { "text": "look at the lock" }, "input": "review_comment" }),
        ),
        (
            "retry_item",
            json!({ "item": "a1" }),
            "/v1/work/retry",
            json!({ "item": "a1" }),
        ),
        (
            "snooze_item",
            json!({ "item": "a1", "until_ms": 5 }),
            "/v1/work/snooze",
            json!({ "item": "a1", "until_ms": 5 }),
        ),
        (
            "block_item",
            json!({ "item": "a1", "reason": "no credit", "resource": "azure-vms", "until_ms": 9 }),
            "/v1/work/block",
            json!({ "item": "a1", "reason": "no credit", "resource": "azure-vms", "until_ms": 9 }),
        ),
        (
            "lift_block",
            json!({ "item": "a1" }),
            "/v1/work/lift-block",
            json!({ "item": "a1" }),
        ),
        (
            "park_item",
            json!({ "item": "a1" }),
            "/v1/work/park",
            json!({ "item": "a1" }),
        ),
        (
            "unpark_item",
            json!({ "item": "a1" }),
            "/v1/work/unpark",
            json!({ "item": "a1" }),
        ),
        (
            "unblock_item",
            json!({ "item": "a1", "to": "a2" }),
            "/v1/work/unblock",
            json!({ "item": "a1", "to": "a2" }),
        ),
        (
            "relate_items",
            json!({ "item": "a1", "to": "a2" }),
            "/v1/work/relate",
            json!({ "item": "a1", "to": "a2" }),
        ),
        (
            "unrelate_items",
            json!({ "item": "a1", "to": "a2" }),
            "/v1/work/unrelate",
            json!({ "item": "a1", "to": "a2" }),
        ),
        (
            "request_review",
            json!({ "item": "a1", "reviewer": "sam" }),
            "/v1/work/request-review",
            json!({ "item": "a1", "reviewer": "sam" }),
        ),
        (
            "request_changes",
            json!({ "item": "a1" }),
            "/v1/work/request-changes",
            json!({ "item": "a1" }),
        ),
        (
            "mark_merged",
            json!({ "item": "a1" }),
            "/v1/work/mark-merged",
            json!({ "item": "a1" }),
        ),
        (
            "rate_summary",
            json!({ "item": "a1", "attempt": 1, "accurate": true }),
            "/v1/work/rate-summary",
            json!({ "item": "a1", "attempt": 1, "accurate": true }),
        ),
        (
            "take_item",
            json!({ "item": "a1" }),
            "/v1/work/take",
            json!({ "item": "a1" }),
        ),
        (
            "reassign_item",
            json!({ "item": "a1", "to": "ana" }),
            "/v1/work/reassign",
            json!({ "item": "a1", "to": { "person": "ana" } }),
        ),
        (
            "reassign_item",
            json!({ "item": "a1", "to": "agent:w1" }),
            "/v1/work/reassign",
            json!({ "item": "a1", "to": { "agent": "w1" } }),
        ),
        (
            "reassign_item",
            json!({ "item": "a1" }),
            "/v1/work/reassign",
            json!({ "item": "a1" }),
        ),
        (
            "steer_agent",
            json!({ "item": "a1", "attempt": 2, "text": "use the writer lock" }),
            "/v1/work/steer",
            json!({ "item": "a1", "attempt": 2, "text": { "text": "use the writer lock" } }),
        ),
        (
            "close_item",
            json!({ "item": "a1", "why": "completed" }),
            "/v1/work/close",
            json!({ "item": "a1", "why": "completed" }),
        ),
        (
            "edit_proposal",
            json!({ "item": "a1", "title": "t", "tier": 1, "depends_on": ["b2"], "split": ["x", "y"], "proposal": 7 }),
            "/v1/work/proposals/edit",
            json!({ "item": "a1", "title": "t", "tier": 1, "depends_on": ["b2"], "split": ["x", "y"], "proposal": 7 }),
        ),
    ];
    for (tool, args, path, body) in cases {
        let fake = Fake::person();
        let p = person(&fake).await;
        let Value::Object(args) = args else { panic!() };
        p.run(tool, Some(args)).await.unwrap().unwrap();
        assert_eq!(fake.seen()[1], (path.to_owned(), Some(body)), "{tool}");
    }
}

#[tokio::test]
async fn every_table_entry_is_covered_by_a_case() {
    // 25 writes above and the queue below: a new entry needs its case.
    assert_eq!(TOOLS.len(), 26);
}

#[tokio::test]
async fn my_queue_reads_with_its_limit() {
    let fake = Fake::person();
    let p = person(&fake).await;
    let Value::Object(args) = json!({ "limit": 5 }) else {
        panic!()
    };
    p.run("my_queue", Some(args)).await.unwrap().unwrap();
    p.run("my_queue", None).await.unwrap().unwrap();
    let seen = fake.seen();
    assert_eq!(seen[1], ("/v1/work/queue?limit=5".to_owned(), None));
    assert_eq!(seen[2], ("/v1/work/queue".to_owned(), None));
}

#[tokio::test]
async fn a_team_goes_in_the_path() {
    let fake = Fake::person();
    let p = Person::connect(fake.clone(), Some(TeamName::new("a").unwrap()))
        .await
        .unwrap();
    let Value::Object(args) = json!({ "item": "a1" }) else {
        panic!()
    };
    p.run("park_item", Some(args)).await.unwrap().unwrap();
    p.run("my_queue", None).await.unwrap().unwrap();
    let seen = fake.seen();
    assert_eq!(seen[1].0, "/v1/teams/a/work/park");
    assert_eq!(seen[2].0, "/v1/teams/a/work/queue");
}

#[tokio::test]
async fn a_team_the_token_is_not_in_is_refused_at_start() {
    let Err(e) = Person::connect(Fake::person(), Some(TeamName::new("b").unwrap())).await else {
        panic!("connected to a team the token is not in");
    };
    assert!(e.contains("not in team b"), "{e}");
}

#[tokio::test]
async fn a_bad_call_never_reaches_the_tracker() {
    let fake = Fake::person();
    let p = person(&fake).await;
    let bad = [
        ("snooze_item", json!({ "item": "a1" }), "until_ms is needed"),
        ("block_item", json!({ "item": "a1" }), "reason is needed"),
        (
            "snooze_item",
            json!({ "item": "a1", "until_ms": -1 }),
            "until_ms must be",
        ),
        (
            "add_note",
            json!({ "item": "a1", "text": "x", "input": "answer" }),
            "input must be one of",
        ),
        (
            "park_item",
            json!({ "item": "a1", "team": "b" }),
            "team is not a parameter",
        ),
        ("flag_item", json!({ "item": 4 }), "item must be a string"),
        (
            "reassign_item",
            json!({ "item": "a1", "to": "" }),
            "to must be a person's name",
        ),
        (
            "steer_agent",
            json!({ "item": "a1", "text": "x" }),
            "attempt is needed",
        ),
        (
            "close_item",
            json!({ "item": "a1", "why": "merged" }),
            "why must be one of",
        ),
        ("close_item", json!({ "item": "a1" }), "why is needed"),
        (
            "edit_proposal",
            json!({ "item": "a1", "split": ["x", 2] }),
            "split must be a list of strings",
        ),
        (
            "add_note",
            json!({ "item": "a1", "text": "x".repeat(INLINE_LIMIT + 1) }),
            "longer than",
        ),
    ];
    for (tool, args, want) in bad {
        let Value::Object(args) = args else { panic!() };
        let e = p.run(tool, Some(args)).await.unwrap().unwrap_err();
        assert!(e.contains(want), "{tool}: {e}");
    }
    // A read has no request key, and a write's key must be a string.
    let Value::Object(args) = json!({ "request_key": "k" }) else {
        panic!()
    };
    assert!(p.run("my_queue", Some(args)).await.unwrap().is_err());
    let Value::Object(args) = json!({ "item": "a1", "request_key": 3 }) else {
        panic!()
    };
    assert!(p.run("park_item", Some(args)).await.unwrap().is_err());
    assert_eq!(fake.seen().len(), 1, "only /v1/me");
}

#[tokio::test]
async fn a_request_key_is_sent_on() {
    let fake = Fake::person();
    let p = person(&fake).await;
    let Value::Object(args) = json!({ "item": "a1", "request_key": "k-1" }) else {
        panic!()
    };
    p.run("retry_item", Some(args)).await.unwrap().unwrap();
    assert_eq!(
        fake.seen()[1].1,
        Some(json!({ "item": "a1", "request_key": "k-1" }))
    );
}

#[tokio::test]
async fn a_refusal_comes_back_as_the_trackers_reason() {
    // The token stops being a person's after the start, as when it is rotated.
    let mut fake = Fake::person();
    let p = person(&fake).await;
    fake.person = None;
    let p = Person { api: fake, ..p };
    let Value::Object(args) = json!({ "item": "a1" }) else {
        panic!()
    };
    let e = p.run("flag_item", Some(args)).await.unwrap().unwrap_err();
    assert_eq!(e, "this call is for a person's token");
}

#[test]
fn every_tool_name_and_route_is_unique() {
    let mut names: Vec<_> = TOOLS.iter().map(|t| t.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), TOOLS.len());
}
