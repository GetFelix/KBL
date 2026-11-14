use clap::Parser;

use super::*;

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(
        [
            "kbl-tool",
            "run",
            "create",
            "demo",
            "--goal",
            "g",
            "--budget-usd",
            "5",
        ]
        .iter()
        .chain(args),
    )
}

#[test]
fn workers_default_to_four_and_stay_between_one_and_sixty_four() {
    let Command::Run(RunCommand::Create(args)) = parse(&[]).unwrap().command else {
        panic!("not a create");
    };
    assert_eq!((args.workers, args.budget_micros), (4, 5_000_000));
    assert!(parse(&["--workers", "0"]).is_err());
    assert!(parse(&["--workers", "65"]).is_err());
    assert!(parse(&["--workers", "64"]).is_ok());
}

#[test]
fn log_takes_a_run_id() {
    let cli = Cli::try_parse_from(["kbl-tool", "run", "log", "demo"]).unwrap();
    assert!(matches!(cli.command, Command::Run(RunCommand::Log { run }) if run == "demo"));
}

#[test]
fn work_log_prints_the_trackers_log() {
    let cli = Cli::try_parse_from(["kbl-tool", "work", "log"]).unwrap();
    assert!(matches!(cli.command, Command::Work(WorkCommand::Log)));
}

#[test]
fn agent_commands_parse_and_the_team_defaults_to_the_namespace() {
    let cli =
        Cli::try_parse_from(["kbl-tool", "agent", "add", "w1", "--tracker-token", "t"]).unwrap();
    let Command::Agent(agents::AgentCommand::Add { name, .. }) = cli.command else {
        panic!("not an add");
    };
    assert_eq!(name, "w1");
    for args in [
        &["kbl-tool", "agent", "list", "--tracker-token", "t"][..],
        &[
            "kbl-tool",
            "agent",
            "revoke",
            "w1",
            "--team",
            "alpha",
            "--tracker-token",
            "t",
        ][..],
    ] {
        assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
    }
}

#[test]
fn agent_list_shows_holds_and_revoked_agents() {
    let body = serde_json::json!({ "agents": [
        { "agent": "w1", "revoked": false, "holds": ["a1"] },
        { "agent": "w2", "revoked": true, "holds": null },
        { "agent": "w3", "revoked": false, "holds": [] },
        { "agent": "old", "token_file": true, "revoked": false, "holds": null },
    ]});
    assert_eq!(
        agents::list_lines(&body),
        [
            "w1  holds a1",
            "w2  revoked",
            "w3  holds nothing",
            "old (token file)  holds unknown: the team is not serving",
        ]
    );
    assert_eq!(
        agents::list_lines(&serde_json::json!({ "agents": [] })),
        ["no agents"]
    );
}
