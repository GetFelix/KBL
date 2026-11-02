use agents_core::codec::Codec;

use super::*;

fn new_run(workers: u32) -> NewRun {
    NewRun {
        run: RunId::new("demo").unwrap(),
        goal: "review PR 12".into(),
        budget_micros: 5_000_000,
        workers,
    }
}

#[test]
fn a_run_has_a_planner_then_numbered_workers() {
    let names: Vec<String> = new_run(3)
        .agents()
        .unwrap()
        .iter()
        .map(|a| format!("{}:{}", a.name.as_str(), a.role))
        .collect();
    assert_eq!(
        names,
        ["planner:planner", "w1:worker", "w2:worker", "w3:worker"]
    );
}

#[test]
fn the_batch_names_every_resource_of_the_run_once() {
    let run = new_run(1);
    let body = batch(&run.resources());
    let mut names: Vec<String> = body["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["stream"].as_str().unwrap().to_owned())
        .chain(
            body["caches"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["cache"].as_str().unwrap().to_owned()),
        )
        .collect();
    names.sort();
    let mut expected = run.resources().all().to_vec();
    expected.sort();
    assert_eq!(names, expected);
}

#[test]
fn the_streams_are_durable_and_single_shard() {
    let body = batch(&new_run(1).resources());
    for stream in body["streams"].as_array().unwrap() {
        assert_eq!(stream["durable"], true);
        assert_eq!(stream["shards"], 1);
    }
}

#[test]
fn run_started_carries_the_goal_budget_and_agents() {
    let logged = new_run(2).started(1_700_000_000_000).unwrap();
    let decoded = Logged::decode(&logged.encode()).unwrap();
    assert_eq!(decoded.by.as_str(), "agents-tool");
    assert_eq!(decoded.at_ms, 1_700_000_000_000);
    let Record::RunStarted {
        goal,
        budget_micros,
        agents,
    } = decoded.record
    else {
        panic!("not run_started");
    };
    assert_eq!(goal, "review PR 12");
    assert_eq!(budget_micros, 5_000_000);
    assert_eq!(agents.len(), 3);
}

#[test]
fn a_budget_is_dollars_to_the_micro() {
    assert_eq!(parse_budget("5"), Ok(5_000_000));
    assert_eq!(parse_budget("0.25"), Ok(250_000));
    assert_eq!(parse_budget("12.345678"), Ok(12_345_678));
    assert_eq!(parse_budget("0.000001"), Ok(1));
}

#[test]
fn a_budget_that_is_not_an_amount_is_refused() {
    for text in [
        "",
        ".5",
        "5.",
        "-1",
        "1e3",
        "1.0000001",
        "abc",
        "1,5",
        "0",
        "0.0",
    ] {
        assert!(parse_budget(text).is_err(), "{text}");
    }
    assert!(parse_budget("99999999999999999999").is_err());
}
