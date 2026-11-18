use clap::Parser;
use serde_json::Value;

use super::*;
use crate::{Cli, Command};

fn init(args: &[&str]) -> InitCommand {
    let cli = Cli::try_parse_from(["kbl-tool", "source", "init"].iter().chain(args)).unwrap();
    let Command::Source(SourceCommand::Init(init)) = cli.command else {
        panic!("not source init");
    };
    init
}

fn render_args(args: &[&str]) -> Result<Value> {
    let (text, _, _) = render(init(args), &mut Prompt { terminal: false })?;
    Ok(serde_json::from_str(&text).unwrap())
}

#[test]
fn a_github_starter_names_the_repo_and_label() {
    let v = render_args(&["github", "--repo", "GetFelix/felix", "--cap", "3"]).unwrap();
    let rule = &v["rules"][0];
    assert_eq!(rule["repo"], "GetFelix/felix");
    assert_eq!(rule["labels_all"][0], "agent-ok");
    assert_eq!(rule["mode"], "admit");
    assert_eq!(rule["cap"], 3);
    assert_eq!(v["github"]["token_env"], "GITHUB_TOKEN");
}

#[test]
fn a_jira_starter_maps_the_project_to_a_repo_and_defaults_the_jql_to_the_label() {
    let v = render_args(&[
        "jira",
        "--site",
        "acme",
        "--api",
        "https://acme.atlassian.net",
        "--project",
        "FEL",
        "--email",
        "kbl@acme.example",
        "--target-repo",
        "GetFelix/felix",
        "--mode",
        "propose",
    ])
    .unwrap();
    let rule = &v["rules"][0];
    assert_eq!(rule["repo"], "acme/FEL");
    assert_eq!(rule["jql"], "labels = \"agent-ok\"");
    assert_eq!(rule["mode"], "propose");
    assert_eq!(rule["target_repo"], "GetFelix/felix");
    assert_eq!(v["jira"]["site"], "acme");
}

#[test]
fn a_missing_value_without_a_terminal_names_the_flag() {
    let e = render_args(&["github"]).unwrap_err().to_string();
    assert!(e.contains("--repo"), "{e}");
    let e = render_args(&["jira", "--site", "acme"])
        .unwrap_err()
        .to_string();
    assert!(e.contains("--api"), "{e}");
}

#[test]
fn values_cannot_break_out_of_the_json() {
    let v = render_args(&["github", "--repo", "a/b", "--label", "x\", \"cap\": 99, \""]).unwrap();
    assert_eq!(v["rules"][0]["cap"], 5);
    assert_eq!(v["rules"][0]["labels_all"][0], "x\", \"cap\": 99, \"");
}

#[test]
fn bad_names_are_refused() {
    assert!(render_args(&["github", "--repo", "nope"]).is_err());
    assert!(render_args(&["github", "--repo", "a/b/c"]).is_err());
    let jira = |site: &str, project: &str, target: &str| {
        render_args(&[
            "jira",
            "--site",
            site,
            "--api",
            "https://x",
            "--project",
            project,
            "--email",
            "e",
            "--target-repo",
            target,
        ])
    };
    assert!(jira("a/b", "FEL", "o/n").is_err());
    assert!(jira("a", "F/L", "o/n").is_err());
    assert!(jira("a", "FEL", "n").is_err());
    jira("a", "FEL", "o/n").unwrap();
}

#[test]
fn a_file_is_not_overwritten_without_force() {
    let dir = std::env::temp_dir().join(format!("kbl-source-init-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ingest.json");
    let _ = std::fs::remove_file(&path);
    write(&path, "one", false).unwrap();
    assert!(write(&path, "two", false).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
    write(&path, "two", true).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
}

#[test]
fn triage_skip_is_written_to_the_rule_and_defaults_to_run() {
    let v = render_args(&["github", "--repo", "a/b"]).unwrap();
    assert_eq!(v["rules"][0]["triage"], "run");
    let v = render_args(&["github", "--repo", "a/b", "--triage", "skip"]).unwrap();
    assert_eq!(v["rules"][0]["triage"], "skip");
}

#[test]
fn triage_skip_with_propose_is_refused() {
    let e = render_args(&[
        "github", "--repo", "a/b", "--triage", "skip", "--mode", "propose",
    ])
    .unwrap_err()
    .to_string();
    assert!(e.contains("--triage skip"), "{e}");
}
