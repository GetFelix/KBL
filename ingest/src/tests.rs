use super::*;
use crate::testing::{Fake, Script, status};

#[tokio::test]
async fn a_failed_poll_keeps_the_reconcile_flag() {
    let gh = Script::default();
    gh.on("GET", "/issues?", status(500, &[]));
    let rule = serde_json::json!({
        "name": "r", "repo": "GetFelix/felix", "labels_all": ["agent-ok"], "mode": "admit"
    });
    let cfg = Config::parse(
        &serde_json::json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "github": { "token_env": "G" },
            "rules": [rule],
        })
        .to_string(),
    )
    .unwrap();
    let repos = cfg.repos();
    let ingest = Ingest::new(
        Github::new("https://api.github.com", gh, vec![]),
        Fake::default(),
        Settings {
            rules: cfg.rules,
            repos,
            reconcile_every: 1000,
            release_compares: 20,
        },
    );
    let wake = webhook::Wake::default();
    wake.reconcile.store(true, Ordering::SeqCst);
    let lessee = lease::Lessee::file_managed("github");
    assert!(
        run(&ingest, &lessee, &wake, Duration::ZERO, true)
            .await
            .is_err()
    );
    assert!(wake.reconcile.load(Ordering::SeqCst));
}

#[test]
fn the_webhook_listens_only_on_loopback_without_a_tls_proxy() {
    let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
    check_listen(addr("127.0.0.1:8791"), false).unwrap();
    check_listen(addr("[::1]:8791"), false).unwrap();
    check_listen(addr("[::ffff:127.0.0.1]:8791"), false).unwrap();
    assert!(check_listen(addr("0.0.0.0:8791"), false).is_err());
    assert!(check_listen(addr("[::]:8791"), false).is_err());
    check_listen(addr("0.0.0.0:8791"), true).unwrap();
}

#[test]
fn check_is_a_subcommand_and_a_plain_run_still_needs_a_config() {
    let cli = Cli::try_parse_from(["kbl-ingest", "check", "--config", "c.json", "--json"]).unwrap();
    assert!(matches!(cli.command, Some(Command::Check(_))));
    let run = Cli::try_parse_from(["kbl-ingest", "--config", "c.json", "--once"]).unwrap();
    assert!(run.command.is_none() && run.config.is_some() && run.once);
    assert!(Cli::try_parse_from(["kbl-ingest", "--once"]).is_err());
}
