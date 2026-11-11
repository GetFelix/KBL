use std::sync::Mutex as StdMutex;

use super::*;

/// A tracker that answers from a script and remembers what it was asked.
#[derive(Clone, Default)]
struct Fake(Arc<StdMutex<Script>>);

#[derive(Default)]
struct Script {
    calls: Vec<(String, Value)>,
    extend: Option<&'static str>,
    complete: Option<&'static str>,
}

impl Fake {
    fn calls(&self, path: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(p, _)| p == path)
            .count()
    }
}

impl TrackerApi for Fake {
    async fn call(&self, path: &str, body: Value) -> Result<Value, CallError> {
        let mut s = self.0.lock().unwrap();
        s.calls.push((path.to_owned(), body));
        match path {
            "/v1/work/next" => Ok(json!({ "claimed": true, "item": { "id": "a1" }, "attempt": 1 })),
            "/v1/work/extend" => match s.extend {
                Some("lost") => Err(CallError::Lost("lapsed".into())),
                Some("down") => Err(CallError::Down("no tracker".into())),
                _ => Ok(json!({ "item": "a1" })),
            },
            _ => match s.complete {
                Some("refused") => Err(CallError::Refused("open_children".into())),
                _ => Ok(json!({ "item": "a1", "status": "done" })),
            },
        }
    }
}

fn work(fake: &Fake) -> Work<Fake> {
    Work::new(
        fake.clone(),
        Some(Duration::from_secs(90)),
        Some(Duration::from_secs(30)),
    )
}

#[tokio::test(start_paused = true)]
async fn a_held_item_is_extended_every_30_seconds() {
    let fake = Fake::default();
    let w = work(&fake);
    w.next(None).await.unwrap();
    tokio::time::sleep(Duration::from_secs(95)).await;
    assert_eq!(fake.calls("/v1/work/extend"), 3);
}

#[tokio::test(start_paused = true)]
async fn completing_stops_the_extensions() {
    let fake = Fake::default();
    let w = work(&fake);
    w.next(None).await.unwrap();
    tokio::time::sleep(Duration::from_secs(35)).await;
    w.complete("a1", 0).await.unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(fake.calls("/v1/work/extend"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_refused_complete_keeps_the_claim_alive() {
    let fake = Fake::default();
    let w = work(&fake);
    w.next(None).await.unwrap();
    fake.0.lock().unwrap().complete = Some("refused");
    assert!(w.complete("a1", 0).await.is_err());
    tokio::time::sleep(Duration::from_secs(65)).await;
    assert_eq!(fake.calls("/v1/work/extend"), 2);
}

#[tokio::test(start_paused = true)]
async fn a_lost_claim_is_reported_by_the_next_call() {
    let fake = Fake::default();
    let w = work(&fake);
    w.next(None).await.unwrap();
    fake.0.lock().unwrap().extend = Some("lost");
    tokio::time::sleep(Duration::from_secs(35)).await;
    let err = w.next(None).await.unwrap_err();
    assert!(err.contains("lapsed"), "{err}");
    // Said once: the agent may ask for work again.
    assert!(w.next(None).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn a_second_claim_while_holding_is_refused() {
    let fake = Fake::default();
    let w = work(&fake);
    w.next(None).await.unwrap();
    let err = w.next(None).await.unwrap_err();
    assert!(err.contains("already hold a1"), "{err}");
    assert_eq!(fake.calls("/v1/work/next"), 1);
}

#[tokio::test(start_paused = true)]
async fn extensions_that_keep_failing_warn_the_agent() {
    let fake = Fake::default();
    let w = work(&fake);
    w.next(None).await.unwrap();
    fake.0.lock().unwrap().extend = Some("down");
    tokio::time::sleep(Duration::from_secs(65)).await;
    let err = w.next(None).await.unwrap_err();
    assert!(err.contains("claim at risk"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn tools_default_to_the_held_item_and_pass_the_tracker_reason_through() {
    let fake = Fake::default();
    let w = work(&fake);
    assert_eq!(w.held_item().await, None);
    w.next(None).await.unwrap();
    assert_eq!(w.held_item().await.as_deref(), Some("a1"));
    // A tool call is forwarded to its route with the body as given.
    let reply = w
        .tool("/v1/work/post", json!({ "item": "a1", "key": "k" }))
        .await
        .unwrap();
    assert_eq!(reply["item"], "a1");
    assert_eq!(fake.calls("/v1/work/post"), 1);
    // A tool refusal does not release the claim: it is still extended.
    fake.0.lock().unwrap().complete = Some("refused");
    let err = w.tool("/v1/work/decide", json!({})).await.unwrap_err();
    assert_eq!(err, "open_children");
    tokio::time::sleep(Duration::from_secs(35)).await;
    assert_eq!(fake.calls("/v1/work/extend"), 1);
    assert_eq!(w.held_item().await.as_deref(), Some("a1"));
}
