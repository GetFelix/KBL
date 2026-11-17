//! Fold snapshots kept on disk, so a boot folds only the records since the
//! newest one instead of all of `work.events`.
//!
//! A snapshot is derived data and never trusted: the log it was built from
//! must still match (`Saved::valid` hashes every record it covers), and a file
//! that is missing, damaged, from another fold revision or ahead of the log is
//! skipped for an older one, then for a full fold. The folder holds nothing the
//! log cannot rebuild, so removing it is always safe. See docs/work-tracker.md.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use kbl_core::artifact::ArtifactId;
use kbl_core::names::ItemId;
use kbl_core::work::WorkLogged;
use kbl_core::work_fold::{Saved, State};
use serde::{Deserialize, Serialize};

use super::request_keys::{Requests, Seen};
use super::{ClaimLog, Recall, Settings};

/// Files kept per team. The newest is used; the rest are the fallback when it
/// does not validate.
pub(super) const KEEP: usize = 3;

const PREFIX: &str = "snapshot-";
const SUFFIX: &str = ".json";

/// What the tracker keeps per record besides the fold, for the records the
/// snapshot covers. Texts are left out: they are a cache, and a miss reads the
/// record back from the log.
#[derive(Serialize, Deserialize)]
struct Seed {
    subjects: Vec<(u64, ItemId)>,
    ingest_edges: Vec<(ItemId, ItemId)>,
    attempts: Vec<((ItemId, u32), Vec<ClaimLog>)>,
    claims: Vec<(u64, (ItemId, u32))>,
    requests: Vec<(String, kbl_core::names::Key, Vec<Seen>)>,
}

impl Seed {
    fn of(recall: &Recall) -> Self {
        // Sorted, so the same state writes the same bytes.
        let mut subjects: Vec<_> = (recall.subjects.iter())
            .map(|(o, i)| (*o, i.clone()))
            .collect();
        subjects.sort();
        let mut ingest_edges: Vec<_> = recall.ingest_edges.iter().cloned().collect();
        ingest_edges.sort();
        let mut attempts: Vec<_> = (recall.attempts.iter())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        attempts.sort_by(|a, b| a.0.cmp(&b.0));
        let mut claims: Vec<_> = recall.claims.iter().map(|(o, k)| (*o, k.clone())).collect();
        claims.sort();
        Self {
            subjects,
            ingest_edges,
            attempts,
            claims,
            requests: recall.requests.parts(),
        }
    }

    fn into_recall(self) -> Recall {
        Recall {
            subjects: self.subjects.into_iter().collect(),
            ingest_edges: self.ingest_edges.into_iter().collect(),
            attempts: self.attempts.into_iter().collect::<HashMap<_, _>>(),
            claims: self.claims.into_iter().collect(),
            requests: Requests::from_parts(self.requests),
            ..Recall::default()
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Body {
    saved: Saved,
    seed: Seed,
}

/// The snapshot folder of one team.
#[derive(Clone)]
pub(super) struct Store {
    dir: PathBuf,
    every: u64,
}

impl Store {
    pub(super) fn new(settings: &Settings) -> Option<Self> {
        let root = settings.snapshot_dir.as_ref()?;
        let team = if settings.team.is_empty() {
            "default"
        } else {
            settings.team.as_str()
        };
        Some(Self {
            dir: root.join(team),
            every: settings.snapshot_every,
        })
    }

    /// The newest snapshot that still describes `log`, with the recall that
    /// went with it. Files that do not are logged and left: a short read or a
    /// failover can make the newest ones invalid for a while, and they are
    /// good again once the log is back. [`Store::prune`] retires them by number.
    pub(super) fn restore(&self, log: &[(u64, WorkLogged)]) -> Option<(Saved, Recall)> {
        if fs::symlink_metadata(&self.dir).is_ok_and(|m| m.file_type().is_symlink()) {
            eprintln!(
                "kbl-tracker: {} is a symlink; snapshots are not used",
                self.dir.display()
            );
            return None;
        }
        self.clear_temps();
        for path in self.files().into_iter().rev() {
            match read(&path).filter(|b| b.saved.valid(log)) {
                Some(Body { saved, seed }) => return Some((saved, seed.into_recall())),
                None => {
                    eprintln!(
                        "kbl-tracker: snapshot {} does not match work.events; ignored",
                        path.display()
                    );
                }
            }
        }
        if self.dir.exists() {
            eprintln!(
                "kbl-tracker: no usable snapshot in {}; folding work.events from the start",
                self.dir.display()
            );
        }
        None
    }

    /// Saves the fold of all of `log` if `every` records came since `from`, the
    /// snapshot it was started from, then drops all but the newest [`KEEP`].
    /// A failure is logged: a snapshot is only a shortcut.
    pub(super) fn save(
        &self,
        log: &[(u64, WorkLogged)],
        from: Option<&Saved>,
        state: &State,
        recall: &Recall,
    ) {
        let have = from.map_or(0, Saved::records);
        let new = (log.len() as u64).saturating_sub(have);
        if self.every == 0 || new < self.every {
            return;
        }
        let Some(saved) = Saved::capture(log, from, state) else {
            return;
        };
        let name = format!("{PREFIX}{:020}{SUFFIX}", saved.records());
        let body = Body {
            saved,
            seed: Seed::of(recall),
        };
        let result = prepare(&self.dir)
            .and_then(|()| stage(&self.dir, &name, &encode(&body)))
            .and_then(|tmp| commit(&tmp, &self.dir.join(&name), &self.dir));
        match result {
            Ok(()) => self.prune(),
            Err(e) => eprintln!(
                "kbl-tracker: could not save a snapshot in {}: {e}",
                self.dir.display()
            ),
        }
    }

    /// Snapshot files, oldest first.
    fn files(&self) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(SUFFIX))
            })
            .collect();
        files.sort();
        files
    }

    fn prune(&self) {
        let files = self.files();
        for old in &files[..files.len().saturating_sub(KEEP)] {
            let _ = fs::remove_file(old);
        }
        self.clear_temps();
    }

    /// A crash between staging and the rename leaves a temp file behind.
    fn clear_temps(&self) {
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(".tmp") {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
}

/// The file: the digest of the body on the first line, then the body. The
/// digest catches a damaged file; the log check decides whether it is current.
fn encode(body: &Body) -> Vec<u8> {
    let json = serde_json::to_vec(body).expect("a snapshot serializes");
    let mut out = ArtifactId::of(&json).as_str().as_bytes().to_vec();
    out.push(b'\n');
    out.extend(json);
    out
}

fn read(path: &Path) -> Option<Body> {
    let bytes = fs::read(path).ok()?;
    let at = bytes.iter().position(|b| *b == b'\n')?;
    let (digest, json) = (&bytes[..at], &bytes[at + 1..]);
    if digest != ArtifactId::of(json).as_str().as_bytes() {
        return None;
    }
    serde_json::from_slice(json).ok()
}

/// Writes `bytes` to a temp file in `dir` and syncs it. Nothing a reader sees
/// has changed until [`commit`].
fn stage(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(tmp)
}

/// Renames the staged file into place and syncs the folder, so the new name
/// survives a power cut.
fn commit(tmp: &Path, to: &Path, dir: &Path) -> std::io::Result<()> {
    fs::rename(tmp, to)?;
    fs::File::open(dir)?.sync_all()
}

/// Creates the team folder if needed and makes it private to the tracker's
/// user, since it holds the team's whole state. A symlink is refused: chmod
/// would follow it onto whatever it points at.
fn prepare(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        // The new name has to survive a power cut too.
        if let Some(parent) = dir.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other("not a plain folder (symlink?)"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
