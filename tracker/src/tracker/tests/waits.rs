//! From format 14 a question or a wait frees the agent without using a try.

use kbl_core::work::ReleaseWhy;
use kbl_core::work_fold::Stage;

use super::*;
use crate::tracker::Reply;

fn text(s: &str) -> kbl_core::content::Content {
    kbl_core::content::Content::Text(s.into())
}

async fn at_14(fake: &Fake) -> Tracker<Fake> {
    let t = start(fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 14 })
        .await
        .unwrap();
    t
}

async fn ask(t: &Tracker<Fake>, who: &str, id: &str) -> Result<Value, Error> {
    t.ask(
        &agent(who),
        &item(id),
        text("which region?"),
        vec![],
        None,
        Vec::new(),
    )
    .await
}

async fn stage(t: &Tracker<Fake>, id: &str) -> Option<Stage> {
    t.0.core.lock().await.state.stage(&item(id))
}

#[tokio::test]
async fn a_question_frees_the_agent_and_the_answer_hands_the_item_out_again() {
    let fake = Fake::default();
    let t = at_14(&fake).await;
    seed(&t, "a1", 2).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
    let asked = ask(&t, "w1", "a1").await.unwrap();
    assert_eq!(asked["claim"], "ended");
    let q = asked["offset"].as_u64().unwrap();
    assert_eq!(stage(&t, "a1").await, Some(Stage::NeedsYou));
    // w1 is free for other work, and nobody gets a1 while it waits.
    seed(&t, "a2", 2).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a2"));
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
    t.person_answer(
        key("gab"),
        &item("a1"),
        Some(q),
        text("eastus"),
        vec![],
        Reply::default(),
    )
    .await
    .unwrap();
    let next = claim(&t, "w2").await;
    assert_eq!(claimed_id(&next).as_deref(), Some("a1"));
    assert_eq!(next["attempt"], 1, "the question used no try");
}

#[tokio::test]
async fn a_wait_is_not_handed_straight_back_out() {
    let fake = Fake::default();
    let t = at_14(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    for why in [ReleaseWhy::WaitsOnOutside, ReleaseWhy::WaitsOnPerson] {
        t.release(
            &agent("w1"),
            &item("a1"),
            None,
            Some("no credit".into()),
            Some(why),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(claimed_id(&claim(&t, "w2").await), None);
        t.person_record(
            key("gab"),
            WorkRecord::Flagged {
                item: item("a1"),
                flagged: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
    }
}

#[tokio::test(start_paused = true)]
async fn an_agent_whose_claim_lapsed_can_still_ask_once() {
    let fake = Fake::default();
    let t = at_14(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    pass(&t, Duration::from_secs(200)).await;
    t.sweep().await;
    let asked = ask(&t, "w1", "a1").await.unwrap();
    assert_eq!(asked["late"], true);
    assert_eq!(stage(&t, "a1").await, Some(Stage::NeedsYou));
    assert!(
        ask(&t, "w1", "a1").await.is_err(),
        "the late window is spent"
    );
    // A late failure is refused: only a wait is a note.
    seed(&t, "a2", 2).await;
    claim(&t, "w2").await;
    pass(&t, Duration::from_secs(200)).await;
    t.sweep().await;
    let failed = t
        .release(
            &agent("w2"),
            &item("a2"),
            None,
            None,
            Some(ReleaseWhy::Failed),
            Vec::new(),
        )
        .await;
    assert!(failed.is_err());
}

#[tokio::test]
async fn every_work_next_reply_says_to_ask_in_kbl() {
    let fake = Fake::default();
    let t = at_14(&fake).await;
    let empty = claim(&t, "w1").await;
    assert!(empty["questions"].as_str().unwrap().contains("work_ask"));
    seed(&t, "a1", 2).await;
    let got = claim(&t, "w1").await;
    assert!(
        got["questions"]
            .as_str()
            .unwrap()
            .contains("never in your host's chat")
    );
}
