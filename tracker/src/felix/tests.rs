use anyhow::Context;
use felix_client::{CursorErrorReason, SubscribeCursorError};

use super::past_the_tail;

fn refused(reason: CursorErrorReason) -> anyhow::Error {
    anyhow::Error::from(SubscribeCursorError {
        reason,
        requested: 9,
        available: 4,
    })
}

/// Only an offset past the tail reads as an empty tip. Retention's refusal
/// and any other failure stay errors, so the connection is dropped for them.
#[test]
fn only_a_read_past_the_tail_is_the_tip() {
    assert!(past_the_tail(&refused(CursorErrorReason::InFuture)));
    let wrapped = Err::<(), _>(refused(CursorErrorReason::InFuture)).context("read");
    assert!(past_the_tail(&wrapped.unwrap_err()));
    assert!(!past_the_tail(&refused(CursorErrorReason::TooOld)));
    assert!(!past_the_tail(&anyhow::anyhow!("the connection dropped")));
}
