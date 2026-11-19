//! A failed check on an agent's pull request becomes `fix_check` work, up to
//! the team's `auto_fix_tries`.

use kbl_core::work::{FailedCheck, PrChecks, PrPhase, PrReview, PrSnapshot, WorkKind};
use kbl_core::work_fold::NeedsYou;

use super::*;

const PR: &str = "GetFelix/felix#9";

fn red() -> WorkRecord {
    WorkRecord::PrStatus {
        item: item("a1"),
        status: PrSnapshot {
            pr: PR.into(),
            phase: PrPhase::Open,
            review: PrReview::Unreviewed,
            checks: PrChecks::Red,
            failing: Some(FailedCheck {
                name: "lint".into(),
                url: "https://github.com/GetFelix/felix/actions/runs/1/job/2".into(),
            }),
        },
    }
}

fn failed(head: &str, fix: bool) -> WorkRecord {
    WorkRecord::CheckFailed {
        item: item("a1"),
        pr: PR.into(),
        check: "lint".into(),
        url: "https://github.com/GetFelix/felix/actions/runs/1/job/2".into(),
        head: head.into(),
        log: "error: unused import `Foo`".into(),
        fix,
    }
}

async fn fix_checks(t: &Tracker<Fake>) -> Vec<ItemId> {
    let core = t.0.core.lock().await;
    core.state.items[&item("a1")]
        .children
        .iter()
        .filter(|c| core.state.items[*c].work == WorkKind::FixCheck)
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_red_check_opens_one_fix_an_agent_claims_with_the_log() {
    let fake = Fake::default();
    let t = start(&fake).await;
    fixed(&t, Some(PR)).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 23 })
        .await
        .unwrap();
    let ingest = key("ingest-gh");
    t.ingest_record(ingest.clone(), red()).await.unwrap();
    // Ingest may not decide that a failure is work.
    let e = t.ingest_record(ingest.clone(), failed("abc", true)).await;
    assert!(matches!(e, Err(Error::Refused { why }) if why == "not_allowed"));
    t.ingest_record(ingest.clone(), failed("abc", false))
        .await
        .unwrap();
    // A re-poll that reports it again opens nothing more.
    let again = t.ingest_record(ingest.clone(), failed("abc", false)).await;
    assert!(matches!(again, Err(Error::Refused { why }) if why == "duplicate"));
    let fixes = fix_checks(&t).await;
    assert_eq!(fixes.len(), 1);
    let got = claim(&t, "w2").await;
    assert_eq!(got["item"]["id"], fixes[0].as_str(), "{got}");
    assert_eq!(got["item"]["fix_check"]["head"], "abc", "{got}");
    assert_eq!(got["item"]["pr"], PR, "{got}");
    let ctx = t.context_read(&agent("w2"), &fixes[0]).await.unwrap();
    assert!(ctx.to_string().contains("unused import"), "{ctx}");
    // Its push fails the same check: the one try is spent, so a person reads it.
    t.complete(&agent("w2"), &fixes[0], None, Vec::new())
        .await
        .unwrap();
    t.ingest_record(ingest, failed("def", false)).await.unwrap();
    assert_eq!(fix_checks(&t).await.len(), 1);
    let core = t.0.core.lock().await;
    assert_eq!(
        core.state.needs_you(&item("a1")),
        Some(NeedsYou::CheckFailed)
    );
    assert_eq!(core.state.items[&item("a1")].check_failures.len(), 2);
}
