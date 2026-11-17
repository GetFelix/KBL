use anyhow::Context;
use felix_client::{CursorErrorReason, SubscribeCursorError};

use super::{pages_to_tip, past_the_tail};

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

/// Like Felix's read: a page ends after its last record, and the empty page
/// after it names the stop, which a generation start can push past the last
/// record. The tip is that stop, so the next append lands where Felix wants it.
#[tokio::test]
async fn the_tip_is_the_empty_pages_offset_past_skipped_offsets() {
    // Records at 0..=2; a new shard leader's generation start took offset 3.
    let page = |from: u64| async move {
        Ok(match from {
            0 => ((0..3).map(|o| (o, vec![1])).collect(), 3),
            3 => (Vec::new(), 4),
            _ => return Err(refused(CursorErrorReason::InFuture)),
        })
    };
    let (records, tip) = pages_to_tip("s", 0, page).await.unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(tip, 4);
    // Read from past the stop, Felix refuses the offset: that is the tip too.
    assert_eq!(pages_to_tip("s", 5, page).await.unwrap(), (Vec::new(), 5));
}

/// A Raft shard's refused conditional publish leaves a blank, so the log can
/// end in several: the empty page after the last record names the stop past
/// all of them, and a write at the tip lands after the blanks.
#[tokio::test]
async fn the_tip_passes_trailing_blanks() {
    // Records at 0..=2; blanks at 3, 4 and 5.
    let page = |from: u64| async move {
        Ok(match from {
            0 => ((0..3).map(|o| (o, vec![1])).collect(), 3),
            3..=5 => (Vec::new(), 6),
            _ => return Err(refused(CursorErrorReason::InFuture)),
        })
    };
    let (records, tip) = pages_to_tip("s", 0, page).await.unwrap();
    assert_eq!((records.len(), tip), (3, 6));
    // Rebuilt from a cursor that sits on a blank.
    assert_eq!(pages_to_tip("s", 4, page).await.unwrap(), (Vec::new(), 6));
}
