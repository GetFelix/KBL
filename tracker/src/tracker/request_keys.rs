//! Request keys: a client may send a key with a non-terminal write, and a retry
//! with the same key answers with the first write instead of writing again.
//! See docs/work-tracker.md.
//!
//! The key rides on the record the call wrote (`WorkLogged::req`), so a
//! restarted or newly elected tracker finds it again when it folds the log.
//! Each caller has a window of its last [`WINDOW`] keys; older keys are
//! forgotten. A write that lands is the only thing that remembers a key, so a
//! call that failed can be sent again as it was.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use kbl_core::names::{ItemId, Key};
use kbl_core::work::{Actor, RequestKey};
use serde_json::{Value, json};

use super::*;

/// How many keys the tracker remembers per caller.
pub(crate) const WINDOW: usize = 256;

tokio::task_local! {
    /// The key of the call in progress, until its first record lands. Read by
    /// `append`, so no route has to pass it down.
    static CURRENT: RefCell<Option<RequestKey>>;
}

/// One remembered write.
struct Seen {
    key: Key,
    sig: String,
    offset: u64,
    kind: &'static str,
    item: Option<ItemId>,
}

pub(super) type Caller = (&'static str, Key);

fn caller(actor: Actor, by: &Key) -> Caller {
    let who = match actor {
        Actor::Agent => "agent",
        Actor::Person => "person",
        Actor::Tracker => "tracker",
    };
    (who, by.clone())
}

/// The keys each caller has used lately, oldest first.
#[derive(Default)]
pub(super) struct Requests(HashMap<Caller, VecDeque<Seen>>);

impl Requests {
    pub(super) fn remember(&mut self, offset: u64, logged: &WorkLogged) {
        let Some(req) = &logged.req else { return };
        if logged.actor == Actor::Tracker {
            return;
        }
        let window = self.0.entry(caller(logged.actor, &logged.by)).or_default();
        // The same key written twice, as after the window forgot it: the later one counts.
        window.retain(|s| s.key != req.key);
        window.push_back(Seen {
            key: req.key.clone(),
            sig: req.sig.clone(),
            offset,
            kind: logged.record.kind(),
            item: logged.record.item().cloned(),
        });
        while window.len() > WINDOW {
            window.pop_front();
        }
    }

    fn find(&self, who: &Caller, key: &Key) -> Option<&Seen> {
        self.0.get(who)?.iter().find(|s| &s.key == key)
    }
}

/// The key of the call in progress, if its record has not landed yet.
pub(super) fn current() -> Option<RequestKey> {
    CURRENT.try_with(|c| c.borrow().clone()).ok().flatten()
}

/// The first record of the call is written: later records of it carry no key.
pub(super) fn landed() {
    let _ = CURRENT.try_with(|c| c.borrow_mut().take());
}

impl<B: Broker> Tracker<B> {
    /// Runs a write. With a request key, a call that sent the key before is
    /// answered with its first record (`replayed: true`), and the same key
    /// with another body is refused as `request_key_reused`.
    pub(crate) async fn keyed(
        &self,
        actor: Actor,
        by: &Key,
        req: Option<RequestKey>,
        write: impl Future<Output = Result<Value, Error>>,
    ) -> Result<Value, Error> {
        let Some(req) = req else {
            return write.await;
        };
        let who = caller(actor, by);
        // Two calls with one key at once would both miss the window.
        let _running = Running::take(&self.0.running, &who, &req.key)?;
        // The window must be the log's, so a state gone stale is rebuilt first.
        self.begin().await?;
        {
            let core = self.0.core.lock().await;
            if let Some(seen) = core.recall.requests.find(&who, &req.key) {
                if seen.sig != req.sig {
                    return Err(Error::Refused {
                        why: "request_key_reused".into(),
                    });
                }
                return Ok(json!({
                    "item": seen.item,
                    "kind": seen.kind,
                    "offset": seen.offset,
                    "replayed": true,
                }));
            }
        }
        CURRENT.scope(RefCell::new(Some(req)), write).await
    }
}

/// Marks a key as being written, until dropped.
struct Running<'a> {
    set: &'a StdMutex<HashSet<(Caller, Key)>>,
    id: (Caller, Key),
}

impl<'a> Running<'a> {
    fn take(
        set: &'a StdMutex<HashSet<(Caller, Key)>>,
        who: &Caller,
        key: &Key,
    ) -> Result<Self, Error> {
        let id = (who.clone(), key.clone());
        if !set.lock().expect("running lock").insert(id.clone()) {
            return Err(Error::Unavailable(
                "a call with this request key is still running; try again".into(),
            ));
        }
        Ok(Self { set, id })
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.set.lock().expect("running lock").remove(&self.id);
    }
}
