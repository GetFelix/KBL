use clap::Parser;

use super::*;

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(
        [
            "agents-tool",
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
    let cli = Cli::try_parse_from(["agents-tool", "run", "log", "demo"]).unwrap();
    assert!(matches!(cli.command, Command::Run(RunCommand::Log { run }) if run == "demo"));
}

#[test]
fn work_log_prints_the_trackers_log() {
    let cli = Cli::try_parse_from(["agents-tool", "work", "log"]).unwrap();
    assert!(matches!(cli.command, Command::Work(WorkCommand::Log)));
}
