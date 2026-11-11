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
        },
    );
    let wake = webhook::Wake::default();
    wake.reconcile.store(true, Ordering::SeqCst);
    assert!(run(&ingest, &wake, Duration::ZERO, true).await.is_err());
    assert!(wake.reconcile.load(Ordering::SeqCst));
}
