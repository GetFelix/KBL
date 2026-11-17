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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use kbl_core::artifact::ArtifactId;
use kbl_core::names::ItemId;
use kbl_core::work::WorkLogged;
use kbl_core::work_fold::{Anchor, Lineage, Saved, State};
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
pub(super) struct Seed {
    subjects: Vec<(u64, ItemId)>,
    ingest_edges: Vec<(ItemId, ItemId)>,
    attempts: Vec<((ItemId, u32), Vec<ClaimLog>)>,
    claims: Vec<(u64, (ItemId, u32))>,
    requests: Vec<(String, kbl_core::names::Key, Vec<Seen>)>,
}

impl Seed {
    fn of(recall: &Recall) -> Self {
        Self::copy(recall).sorted()
    }

    /// A copy of what the snapshot keeps, in no order. Cheap enough to take
    /// under the core lock; [`Seed::sorted`] runs off it.
    pub(super) fn copy(recall: &Recall) -> Self {
        Self {
            subjects: recall
                .subjects
                .iter()
                .map(|(o, i)| (*o, i.clone()))
                .collect(),
            ingest_edges: recall.ingest_edges.iter().cloned().collect(),
            attempts: (recall.attempts.iter())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            claims: recall.claims.iter().map(|(o, k)| (*o, k.clone())).collect(),
            requests: recall.requests.parts(),
        }
    }

    /// Sorted, so the same state writes the same bytes.
    pub(super) fn sorted(mut self) -> Self {
        self.subjects.sort();
        self.ingest_edges.sort();
        self.attempts.sort_by(|a, b| a.0.cmp(&b.0));
        self.claims.sort();
        self
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

/// Saves from the running tracker. It keeps the chain over the log up to its
/// anchor and the records applied since, so a save hashes only those and never
/// reads the log back. Lives in the core and is touched under its lock.
pub(super) struct Periodic {
    id: u64,
    /// The rebuild generation this belongs to; see [`Store::begin_generation`].
    generation: u64,
    store: Store,
    anchor: Anchor,
    /// Records landed since `anchor`, oldest first. While a save runs this
    /// is capped (see [`TAIL_CAP`]); past that, saving stops until the next rebuild.
    tail: Vec<(u64, WorkLogged)>,
    saving: bool,
}

/// A save in progress: the copy taken at a record boundary.
pub(super) struct Job {
    id: u64,
    generation: u64,
    store: Store,
    anchor: Anchor,
    tail: Vec<(u64, WorkLogged)>,
    state: State,
    seed: Seed,
}

/// How many saves' worth of records may pile up behind a slow disk, and the
/// fewest records that may, so a small `every` is not given up on by a burst.
const TAIL_CAP: u64 = 4;
const TAIL_CAP_MIN: u64 = 10_000;

static PERIODIC_IDS: AtomicU64 = AtomicU64::new(1);

impl Periodic {
    /// `anchor` is where the chain over `log` stands, as [`Store::save`] left
    /// it. None when saving is off.
    pub(super) fn new(
        store: Store,
        generation: u64,
        anchor: Anchor,
        log: &[(u64, WorkLogged)],
    ) -> Option<Self> {
        if store.every == 0 {
            return None;
        }
        let from = usize::try_from(anchor.records()).ok()?;
        Some(Self {
            id: PERIODIC_IDS.fetch_add(1, Ordering::Relaxed),
            generation,
            store,
            tail: log.get(from..)?.to_vec(),
            anchor,
            saving: false,
        })
    }

    /// The record at `offset` has landed and `state` and `recall` include it,
    /// and nothing after it. Returns the copy to save once `every` records
    /// came since the last save. `Err` means this cannot keep up (the state is
    /// not at the record, or a save is far behind) and saving is off until the
    /// next rebuild.
    pub(super) fn note(
        &mut self,
        offset: u64,
        logged: &WorkLogged,
        state: &State,
        recall: &Recall,
    ) -> Result<Option<Job>, &'static str> {
        if state.at != Some(offset) {
            return Err("the state is not at the record that landed");
        }
        self.tail.push((offset, logged.clone()));
        let every = self.store.every;
        if self.saving {
            let cap = every.saturating_mul(TAIL_CAP).max(TAIL_CAP_MIN);
            return if self.tail.len() as u64 > cap {
                Err("a save is far behind")
            } else {
                Ok(None)
            };
        }
        if (self.tail.len() as u64) < every {
            return Ok(None);
        }
        Ok(self.due(state, recall))
    }

    /// The copy for the next save, if `every` records are waiting.
    fn due(&mut self, state: &State, recall: &Recall) -> Option<Job> {
        if (self.tail.len() as u64) < self.store.every {
            return None;
        }
        self.saving = true;
        Some(Job {
            id: self.id,
            generation: self.generation,
            store: self.store.clone(),
            anchor: self.anchor.clone(),
            tail: std::mem::take(&mut self.tail),
            state: state.clone(),
            seed: Seed::copy(recall),
        })
    }

    /// The save of `job_id` ended; `anchor` is where its chain reached. A save
    /// that could not build a snapshot stops saving (`Err`). Records that piled
    /// up behind it start the next save at once, if `state` is at the last of
    /// them: it is not while a write that failed sits applied.
    pub(super) fn finished(
        &mut self,
        job_id: u64,
        anchor: Option<Anchor>,
        state: &State,
        recall: &Recall,
    ) -> Result<Option<Job>, ()> {
        if self.id != job_id {
            return Ok(None);
        }
        self.anchor = anchor.ok_or(())?;
        self.saving = false;
        let at_boundary = self.tail.last().is_some_and(|(o, _)| state.at == Some(*o));
        Ok(if at_boundary {
            self.due(state, recall)
        } else {
            None
        })
    }
}

impl Job {
    pub(super) fn id(&self) -> u64 {
        self.id
    }

    /// Hashes the records since the anchor, writes the file, and returns the
    /// extended anchor. Blocking: run it off the async workers. Nothing is
    /// written if a rebuild started since the job was taken.
    pub(super) fn run(self) -> Option<Anchor> {
        let saved = self.anchor.extend(&self.tail, &self.state)?;
        self.store
            .write(self.generation, &saved, &self.seed.sorted(), None);
        Some(saved.anchor())
    }
}

/// The snapshot folder of one team.
#[derive(Clone)]
pub(super) struct Store {
    dir: PathBuf,
    every: u64,
    /// The rebuild generation of this folder, shared by every `Store` for it.
    /// Held while a file is written or pruned, so a rebuild's bump waits out a
    /// write in flight and no write of an older generation lands after it.
    gate: Arc<Mutex<u64>>,
}

/// One gate per folder for the life of the process: each rebuild makes a new
/// `Store`, and the saves of the one before it must see the bump.
fn gate_for(dir: &Path) -> Arc<Mutex<u64>> {
    static GATES: Mutex<Vec<(PathBuf, Arc<Mutex<u64>>)>> = Mutex::new(Vec::new());
    let mut gates = GATES.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = &canonical(dir);
    if let Some((_, g)) = gates.iter().find(|(d, _)| d == dir) {
        return g.clone();
    }
    let g = Arc::new(Mutex::new(0));
    gates.push((dir.to_owned(), g.clone()));
    g
}

/// The folder's path with symlinks and `..` resolved as far as it exists, so
/// two spellings of one folder share a gate. The folder may not exist yet.
fn canonical(dir: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = dir;
    loop {
        if let Ok(real) = fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |p, part| p.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            }
            _ => return dir.to_owned(),
        }
    }
}

impl Store {
    pub(super) fn new(settings: &Settings) -> Option<Self> {
        let root = settings.snapshot_dir.as_ref()?;
        let team = if settings.team.is_empty() {
            "default"
        } else {
            settings.team.as_str()
        };
        let dir = root.join(team);
        Some(Self {
            gate: gate_for(&dir),
            dir,
            every: settings.snapshot_every,
        })
    }

    /// Starts a new rebuild generation and returns it. A save job of an older
    /// generation writes and prunes nothing from here on: it was taken from a
    /// state the rebuild replaces. Blocking: waits out a write in flight.
    pub(super) fn begin_generation(&self) -> u64 {
        let mut current = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        *current += 1;
        *current
    }

    /// The newest snapshot that still describes `log`, with the recall that
    /// went with it. Files that do not are logged and left: a short read or a
    /// failover can make the newest ones invalid for a while, and they are
    /// good again once the log is back. Files of a log that diverged from the
    /// one restored are dropped, see [`Store::prune`].
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
                Some(Body { saved, seed }) => {
                    let _held = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
                    self.prune(&path, Some(log));
                    return Some((saved, seed.into_recall()));
                }
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
    /// A failure is logged: a snapshot is only a shortcut. Returns where the
    /// chain over `log` stands, saved or not, so a running tracker can extend
    /// it from the records after it.
    pub(super) fn save(
        &self,
        generation: u64,
        log: &[(u64, WorkLogged)],
        from: Option<&Saved>,
        state: &State,
        recall: &Recall,
    ) -> Anchor {
        let have = from.map_or(0, Saved::records);
        let new = (log.len() as u64).saturating_sub(have);
        let behind = || from.map_or_else(Anchor::default, Saved::anchor);
        if self.every == 0 || new < self.every {
            return behind();
        }
        let Some(saved) = Saved::capture(log, from, state) else {
            return behind();
        };
        self.write(generation, &saved, &Seed::of(recall), Some(log));
        saved.anchor()
    }

    /// Writes `saved` and prunes, unless a rebuild began since `generation`.
    ///
    /// With the `log` the file was built from, files that log contradicts are
    /// removed first, see [`Store::prune`].
    pub(super) fn write(
        &self,
        generation: u64,
        saved: &Saved,
        seed: &Seed,
        log: Option<&[(u64, WorkLogged)]>,
    ) {
        let held = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if *held != generation {
            return;
        }
        let name = format!("{PREFIX}{:020}{SUFFIX}", saved.records());
        let result = prepare(&self.dir)
            .and_then(|()| stage(&self.dir, &name, &encode(saved, seed)))
            .and_then(|tmp| commit(&tmp, &self.dir.join(&name), &self.dir));
        match result {
            Ok(()) => self.prune(&self.dir.join(&name), log),
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

    /// Drops the files `log` contradicts, then all but the newest [`KEEP`],
    /// always keeping `fresh`. After retention trims the log, old files carry
    /// higher record counts than new ones and can never validate again. A file
    /// the log merely does not reach yet is kept, and so is one that could not
    /// be read: those only count against the limit, after the verified ones.
    /// Without a log (a running tracker's save, whose log only grows) only the
    /// count applies.
    /// The caller holds the gate.
    fn prune(&self, fresh: &Path, log: Option<&[(u64, WorkLogged)]>) {
        for path in self.files() {
            if path == fresh {
                continue;
            }
            let gone =
                log.is_some_and(|log| read_lineage(&path).is_some_and(|l| l.diverges_from(log)));
            if gone {
                let _ = fs::remove_file(path);
            }
        }
        // Verified files outrank the rest: a file ahead of a short log, or one
        // that could not be read, must not push a checked file out.
        let verified = |p: &Path| match log {
            Some(log) => read_lineage(p).is_some_and(|l| l.within(log)),
            None => true,
        };
        let mut ranked: Vec<_> = self
            .files()
            .into_iter()
            .filter(|f| f.as_path() != fresh)
            .map(|f| (verified(&f), f))
            .collect();
        ranked.sort_by(|a, b| b.cmp(a));
        for (_, old) in ranked.iter().skip(KEEP.saturating_sub(1)) {
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
fn encode(saved: &Saved, seed: &Seed) -> Vec<u8> {
    #[derive(Serialize)]
    struct BodyRef<'a> {
        saved: &'a Saved,
        seed: &'a Seed,
    }
    let json = serde_json::to_vec(&BodyRef { saved, seed }).expect("a snapshot serializes");
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

/// The chain points of a file, without its digest check or its state.
fn read_lineage(path: &Path) -> Option<Lineage> {
    #[derive(Deserialize)]
    struct Head {
        saved: Lineage,
    }
    let bytes = fs::read(path).ok()?;
    let at = bytes.iter().position(|b| *b == b'\n')?;
    serde_json::from_slice::<Head>(&bytes[at + 1..])
        .ok()
        .map(|h| h.saved)
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
