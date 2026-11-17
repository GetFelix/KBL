//! Snapshots on disk: a boot from one is the fold of the log, and a file that
//! does not describe the log is never used.

use std::sync::atomic::{AtomicU32, Ordering};

use kbl_core::work::{HumanKind, RequestKey};

use super::*;
use crate::felix::Broker;
use crate::tracker::tests::{Fake, claim, item, key, seed, settings, start, start_with};
use crate::tracker::{Actor, Tracker, WORK_EVENTS, WorkRecord, json};

static N: AtomicU32 = AtomicU32::new(0);

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kbl-snapshots-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn with_dir(dir: &Path, every: u64) -> Settings {
    Settings {
        snapshot_dir: Some(dir.to_owned()),
        snapshot_every: every,
        ..settings()
    }
}

async fn entries(fake: &Fake) -> Vec<(u64, WorkLogged)> {
    fake.read_from(WORK_EVENTS, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|(o, p)| (o, kbl_core::codec::Codec::decode(&p).unwrap()))
        .collect()
}

async fn note(t: &Tracker<Fake>, request: &str) {
    let req = RequestKey::new(key(request), "/v1/work/note", &json!({ "text": request }));
    t.keyed(Actor::Person, &key("gab"), Some(req), async {
        t.person_record(
            key("gab"),
            WorkRecord::HumanInput {
                item: item("a1"),
                attempt: None,
                input: HumanKind::Note,
                text: kbl_core::content::Content::Text(request.into()),
            },
        )
        .await
    })
    .await
    .unwrap();
}

/// A log with claims, a request key and a queue entry, and a snapshot of its
/// first records in `dir`. Returns the fake and the records the log has.
async fn logged(dir: &Path) -> (Fake, usize) {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    seed(&t, "b1", 2).await;
    let got = claim(&t, "w1").await;
    assert_eq!(got["claimed"], true);
    note(&t, "k1").await;
    let n = entries(&fake).await.len();
    // The boot after these records saves a snapshot of them.
    let t = start_with(&fake, with_dir(dir, 2)).await;
    drop(t);
    (fake, n)
}

fn store(dir: &Path) -> Store {
    Store::new(&with_dir(dir, 2)).unwrap()
}

fn files(dir: &Path) -> Vec<PathBuf> {
    store(dir).files()
}

#[tokio::test]
async fn a_boot_from_a_snapshot_equals_a_full_fold() {
    let dir = scratch();
    let (fake, n) = logged(&dir).await;
    let (saved, _) = store(&dir)
        .restore(&entries(&fake).await)
        .expect("a snapshot");
    assert_eq!(saved.records() as usize, n);

    // More records land after the snapshot; the next boot folds only those.
    let t = start_with(&fake, settings()).await;
    seed(&t, "c1", 1).await;
    note(&t, "k2").await;
    drop(t);

    let from_snapshot = start_with(&fake, with_dir(&dir, 1_000_000)).await;
    let full = start_with(&fake, settings()).await;
    let (a, b) = (from_snapshot.0.core.lock().await, full.0.core.lock().await);
    assert_eq!(a.state, b.state);
    assert_eq!(a.tail, b.tail);
    assert_eq!(
        format!("{:?}", a.state.queue(0)),
        format!("{:?}", b.state.queue(0))
    );
    assert_eq!(
        format!("{:?}", a.state.time_to_human_action(0, u64::MAX)),
        format!("{:?}", b.state.time_to_human_action(0, u64::MAX)),
    );
    // What the tracker keeps per record is the same too, including the
    // request key and the claim that came before the snapshot.
    assert_eq!(
        serde_json::to_string(&Seed::of(&a.recall)).unwrap(),
        serde_json::to_string(&Seed::of(&b.recall)).unwrap(),
    );
    assert!(
        a.recall
            .requests
            .parts()
            .iter()
            .any(|(_, _, w)| w.len() == 2)
    );
    assert!(!a.recall.attempts.is_empty());
}

#[tokio::test]
async fn a_damaged_snapshot_falls_back_to_an_older_one_then_to_a_full_fold() {
    let dir = scratch();
    let (fake, n) = logged(&dir).await;
    // Another snapshot, further on.
    let t = start_with(&fake, settings()).await;
    seed(&t, "c1", 1).await;
    drop(t);
    drop(start_with(&fake, with_dir(&dir, 2)).await);
    let log = entries(&fake).await;
    let (old, new) = match &files(&dir)[..] {
        [old, new] => (old.clone(), new.clone()),
        other => panic!("two snapshots: {other:?}"),
    };
    assert_eq!(
        store(&dir).restore(&log).unwrap().0.records() as usize,
        log.len()
    );

    // Flipped byte: the digest catches it.
    let mut bytes = fs::read(&new).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 1;
    fs::write(&new, bytes).unwrap();
    assert_eq!(store(&dir).restore(&log).unwrap().0.records() as usize, n);
    assert!(new.exists(), "a rejected file is kept");

    // Truncated.
    let bytes = fs::read(&old).unwrap();
    fs::write(&old, &bytes[..bytes.len() / 2]).unwrap();
    assert!(store(&dir).restore(&log).is_none());
}

#[tokio::test]
async fn a_snapshot_of_another_fold_revision_is_not_used() {
    let dir = scratch();
    let (fake, _) = logged(&dir).await;
    let path = files(&dir).pop().unwrap();
    // Valid digest, valid shape, older revision.
    let mut body: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap().split_off(
            fs::read(&path)
                .unwrap()
                .iter()
                .position(|b| *b == b'\n')
                .unwrap()
                + 1,
        ),
    )
    .unwrap();
    body["saved"]["snapshot"]["rev"] = 1.into();
    let json = serde_json::to_vec(&body).unwrap();
    let mut out = ArtifactId::of(&json).as_str().as_bytes().to_vec();
    out.push(b'\n');
    out.extend(json);
    fs::write(&path, out).unwrap();
    assert!(read(&path).is_some(), "the file itself reads fine");
    assert!(store(&dir).restore(&entries(&fake).await).is_none());
}

#[tokio::test]
async fn a_log_edited_below_the_snapshot_rejects_it() {
    let dir = scratch();
    let (fake, _) = logged(&dir).await;
    let mut log = entries(&fake).await;
    // Same count, same offsets, same last record: only an earlier record differs.
    log[1].1 = log[3].1.clone();
    assert!(store(&dir).restore(&log).is_none());

    // A log shorter than the snapshot, as after a trim or a lost shard tail.
    let (fake, _) = logged(&dir).await;
    let mut log = entries(&fake).await;
    log.pop();
    assert!(store(&dir).restore(&log).is_none());
}

#[tokio::test]
async fn a_crash_while_writing_leaves_the_old_snapshot() {
    let dir = scratch();
    let (fake, n) = logged(&dir).await;
    let log = entries(&fake).await;
    let before = files(&dir);

    // A newer snapshot reaches the staged file and the process dies before the rename.
    let t = start_with(&fake, settings()).await;
    seed(&t, "c1", 1).await;
    let state = t.0.core.lock().await.state.clone();
    let body = Body {
        saved: Saved::capture(&entries(&fake).await, None, &state).unwrap(),
        seed: Seed::of(&t.0.core.lock().await.recall),
    };
    let tmp = stage(&dir.join("default"), "snapshot-99.json", &encode(&body)).unwrap();
    assert!(tmp.exists());
    // Half a file, as if the write was cut short.
    fs::write(&tmp, &encode(&body)[..20]).unwrap();

    assert_eq!(files(&dir), before);
    assert_eq!(
        store(&dir).restore(&log[..n]).unwrap().0.records() as usize,
        n
    );

    // A restore clears the leftover.
    assert!(!tmp.exists());
}

#[tokio::test]
async fn a_leftover_temp_file_is_cleared_by_the_next_save() {
    let dir = scratch();
    let (fake, _) = logged(&dir).await;
    let tmp = dir.join("default").join(".snapshot-99.json.tmp");
    fs::write(&tmp, b"half").unwrap();
    let t = start_with(&fake, settings()).await;
    seed(&t, "c1", 1).await;
    drop(t);
    drop(start_with(&fake, with_dir(&dir, 2)).await);
    assert!(!tmp.exists());
}

#[tokio::test]
async fn a_shorter_log_skips_the_newest_snapshot_and_a_regrown_log_uses_it_again() {
    let dir = scratch();
    let (fake, _) = logged(&dir).await;
    let full = entries(&fake).await;
    let before = files(&dir);

    // A failover or a short read: the snapshot covers records the log lacks.
    assert!(store(&dir).restore(&full[..full.len() - 1]).is_none());
    assert_eq!(files(&dir), before, "the file is kept");

    assert_eq!(
        store(&dir).restore(&full).unwrap().0.records() as usize,
        full.len()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_team_folder_is_neither_used_nor_chmodded() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch();
    let target = scratch();
    fs::create_dir_all(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&target, dir.join("default")).unwrap();

    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    seed(&t, "b1", 2).await;
    drop(start_with(&fake, with_dir(&dir, 1)).await);

    assert!(files(&dir).is_empty());
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(fs::read_dir(&target).unwrap().next().is_none());
}

#[tokio::test]
async fn only_the_newest_snapshots_are_kept() {
    let dir = scratch();
    let (fake, _) = logged(&dir).await;
    for i in 0..KEEP + 2 {
        let t = start_with(&fake, settings()).await;
        seed(&t, &format!("x{i}"), 1).await;
        drop(t);
        drop(start_with(&fake, with_dir(&dir, 2)).await);
    }
    assert_eq!(files(&dir).len(), KEEP);
}

#[tokio::test]
async fn no_directory_means_no_snapshots() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    assert!(Store::new(&settings()).is_none());
}
