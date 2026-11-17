use super::*;

fn held(by: &str, epoch: u64, expires_ms: u64) -> Lease {
    Lease {
        holder: Some(by.to_owned()),
        epoch,
        expires_ms,
    }
}

#[test]
fn a_take_raises_the_epoch_and_a_renew_keeps_it() {
    let taken = next(None, "a", step(None, "a", true, 0), 0).unwrap();
    assert_eq!(taken.epoch, 1);
    let renewed = next(Some(&taken), "a", step(Some(&taken), "a", true, 10), 10).unwrap();
    assert_eq!((renewed.epoch, renewed.expires_ms), (1, 10 + LEASE_MS));
}

#[test]
fn a_live_lease_does_not_move_until_the_take_margin_past_its_expiry() {
    let a = held("a", 4, 1_000);
    assert_eq!(step(Some(&a), "b", true, 999), Step::Leave);
    // Expired, but a request of the old holder may still be in flight, or
    // this tracker's clock may run ahead of the one that wrote it.
    assert_eq!(step(Some(&a), "b", true, 1_000), Step::Leave);
    let free_at = 1_000 + TAKE_MARGIN_MS;
    assert_eq!(step(Some(&a), "b", true, free_at - 1), Step::Leave);
    assert_eq!(step(Some(&a), "b", true, free_at), Step::Take);
    let b = next(Some(&a), "b", Step::Take, free_at).unwrap();
    assert_eq!((b.holder.as_deref(), b.epoch), (Some("b"), 5));
    // The old holder's renew is refused: it no longer holds it.
    assert_eq!(step(Some(&b), "a", true, free_at + 1), Step::Leave);
}

#[test]
fn a_released_lease_is_free_after_the_margin() {
    let released = Lease {
        holder: None,
        epoch: 7,
        expires_ms: 100,
    };
    assert_eq!(step(Some(&released), "b", true, 100), Step::Leave);
    let at = 100 + TAKE_MARGIN_MS;
    assert_eq!(step(Some(&released), "b", true, at), Step::Take);
    assert_eq!(next(Some(&released), "b", Step::Take, at).unwrap().epoch, 8);
}

#[test]
fn a_worker_not_assigned_never_takes_or_renews() {
    let a = held("a", 1, 1_000);
    assert_eq!(step(Some(&a), "a", false, 0), Step::Leave);
    assert_eq!(step(None, "a", false, 0), Step::Leave);
}

#[test]
fn rendezvous_moves_only_the_share_of_a_worker_that_joins_or_leaves() {
    let sources: Vec<String> = (0..200).map(|i| format!("github:o/r{i}")).collect();
    let three = ["w1", "w2", "w3"];
    let four = ["w1", "w2", "w3", "w4"];
    let mut moved = 0;
    for s in &sources {
        let before = owner(s, three).unwrap();
        let after = owner(s, four).unwrap();
        if before != after {
            // Joining moves sources only to the new worker.
            assert_eq!(after, "w4");
            moved += 1;
        }
        // Leaving moves only the leaver's sources.
        let without_w2 = owner(s, ["w1", "w3"]).unwrap();
        if before != "w2" {
            assert_eq!(without_w2, before);
        }
    }
    assert!((20..=80).contains(&moved), "w4 took {moved} of 200");
    assert_eq!(owner("x", Vec::<&str>::new()), None);
}
