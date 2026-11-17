//! `export`, `import` and `digest` for `work.events` and `org.events`: a backup
//! of a log and the check that a restore rebuilt the same state. See
//! docs/backup-restore.md.

use std::fs;
use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use kbl_core::org::{ORG_EVENTS, ORG_NAMESPACE, OrgReadError};
use kbl_core::work::WORK_EVENTS;
use kbl_core::work_fold::State;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use crate::felix::{Broker, ConditionalWrite, Session, Target, is_leaderless};

/// The file's first line names it, so a file of something else is refused.
const MAGIC: &str = "kbl-backup";
const VERSION: u64 = 1;

/// The log a command works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Log {
    /// A team's `work.events`, in the namespace the target names.
    Work,
    /// The organization's `org.events`, in its own namespace.
    Org,
}

impl Log {
    fn stream(self) -> &'static str {
        match self {
            Log::Work => WORK_EVENTS,
            Log::Org => ORG_EVENTS,
        }
    }

    fn target(self, mut target: Target) -> Target {
        if self == Log::Org {
            target.namespace = ORG_NAMESPACE.to_owned();
        }
        target
    }

    /// The fold's canonical text and its SHA-256. For the org log it is the
    /// records with their offsets: the log is the whole record of what the
    /// org fold reads, and the fold itself is not serializable.
    fn digest(self, records: &[(u64, Vec<u8>)]) -> Result<(String, String)> {
        match self {
            Log::Work => canonical(&crate::fold_log(records)?),
            Log::Org => {
                let mut text = String::new();
                for (offset, payload) in records {
                    if let Err(OrgReadError::Newer { v }) = kbl_core::org::read(payload) {
                        bail!(
                            "{ORG_EVENTS} has a format {v} record at offset {offset}; update kbl-tool"
                        );
                    }
                    text.push_str(&format!("{offset}\t{}\n", String::from_utf8_lossy(payload)));
                }
                let digest = hex(&Sha256::digest(text.as_bytes()));
                Ok((text, digest))
            }
        }
    }
}

/// What a restore needs from Felix, so its rules can be tested without a broker.
pub(crate) trait LogStore {
    /// Every record with its offset, and the offset after the committed log's end.
    async fn read_to_tip(&self, stream: &str) -> Result<(Vec<(u64, Vec<u8>)>, u64)>;
    async fn publish_if(
        &self,
        stream: &str,
        payload: Vec<u8>,
        expected: u64,
    ) -> Result<ConditionalWrite>;
}

impl LogStore for Broker {
    async fn read_to_tip(&self, stream: &str) -> Result<(Vec<(u64, Vec<u8>)>, u64)> {
        Broker::read_to_tip(self, stream).await
    }
    async fn publish_if(
        &self,
        stream: &str,
        payload: Vec<u8>,
        expected: u64,
    ) -> Result<ConditionalWrite> {
        Broker::publish_if(self, stream, payload, expected).await
    }
}

#[derive(Args)]
pub(crate) struct ExportArgs {
    /// The file to write. It is replaced if it exists.
    file: String,
}

#[derive(Args)]
pub(crate) struct ImportArgs {
    /// A file written by `export`.
    file: String,
}

#[derive(Args)]
pub(crate) struct DigestArgs {
    /// Also write the fold's canonical text here, to see what differs when two digests do.
    #[arg(long, value_name = "FILE")]
    state: Option<String>,
    /// Digest only the records this backup file covers, so a record written
    /// since the export does not make the two disagree.
    #[arg(long, value_name = "FILE")]
    backup: Option<String>,
}

pub(crate) async fn export(log: Log, target: Target, args: ExportArgs) -> Result<()> {
    let broker = Session::sign_in(log.target(target)).await?.broker().await?;
    let (records, tip) = broker.read_to_tip(log.stream()).await?;
    let (text, shown) = render(log, &records, tip)?;
    write_file(&args.file, &text)?;
    println!("exported {} records to {}", records.len(), args.file);
    println!("{shown}");
    Ok(())
}

/// The backup file for `records` read up to `tip`, and the digest it prints.
fn render(log: Log, records: &[(u64, Vec<u8>)], tip: u64) -> Result<(String, Value)> {
    let first = records.first().map_or(tip, |(offset, _)| *offset);
    // One line per offset from the first record to the tip; a blank offset
    // (a refused conditional publish on a Raft shard) is an empty line.
    let mut lines = vec![""; (tip - first) as usize];
    let mut last = None;
    for (offset, payload) in records {
        ensure!(
            last.is_none_or(|l| *offset > l) && (first..tip).contains(offset),
            "{} reads out of order at offset {offset}",
            log.stream()
        );
        last = Some(*offset);
        let text = std::str::from_utf8(payload)
            .with_context(|| format!("the record at offset {offset} is not text"))?;
        ensure!(
            !text.contains('\n') && serde_json::from_str::<Value>(text).is_ok(),
            "the record at offset {offset} is not one line of JSON"
        );
        lines[(offset - first) as usize] = text;
    }
    let header = json!({
        "backup": MAGIC,
        "version": VERSION,
        "stream": log.stream(),
        "first_offset": first,
        "next_offset": tip,
        "records": records.len(),
        "sha256": hash_lines(&lines),
    });
    let mut out = format!("{header}\n");
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    Ok((out, digest_json(log, records)?.1))
}

pub(crate) async fn import(log: Log, target: Target, args: ImportArgs) -> Result<()> {
    let text = fs::read_to_string(&args.file).with_context(|| format!("read {}", args.file))?;
    let backup = parse_backup(log, &text)?;
    let broker = Session::sign_in(log.target(target)).await?.broker().await?;
    let done = restore(&broker, log.stream(), &backup).await?;
    println!("{done} into {}", log.stream());
    Ok(())
}

/// Loads `backup` into the log. The log may be empty or hold the start of this
/// same backup, which a failed restore leaves behind; it carries on after it.
async fn restore(broker: &impl LogStore, stream: &str, backup: &Backup<'_>) -> Result<String> {
    let (existing, tip) = broker.read_to_tip(stream).await?;
    for (offset, payload) in &existing {
        ensure!(
            backup
                .line(*offset)
                .is_some_and(|l| l.as_bytes() == payload),
            "{stream} holds a record at offset {offset} that this backup does not. A restore goes into an empty log, or one that holds the start of this same backup."
        );
    }
    ensure!(
        tip <= backup.next,
        "{stream} ends at offset {tip}, past the backup's end at {}; it holds more than this backup",
        backup.next
    );
    let have: Vec<u64> = existing.iter().map(|(o, _)| *o).collect();
    for offset in backup.record_offsets().filter(|o| *o < tip) {
        ensure!(
            have.contains(&offset),
            "{stream} has no record at offset {offset}, where the backup has one; it holds something else"
        );
    }
    if tip == backup.next {
        return Ok(format!("{stream} already holds this backup; nothing to do"));
    }
    for offset in tip..backup.next {
        match backup.line(offset) {
            Some(line) => {
                write_at(broker, stream, offset, line).await?;
            }
            None => blank(broker, stream, offset).await?,
        }
    }
    let (restored, end) = broker.read_to_tip(stream).await?;
    let wanted: Vec<(u64, &str)> = backup.records().collect();
    ensure!(
        end == backup.next
            && restored.len() == wanted.len()
            && restored
                .iter()
                .zip(&wanted)
                .all(|((o, p), (wo, wl))| o == wo && p == wl.as_bytes()),
        "after the restore {stream} does not read back as the backup"
    );
    Ok(format!(
        "imported {} records ({} offsets)",
        wanted.len(),
        backup.next - tip
    ))
}

/// How long a restore keeps trying while the broker has no lease on the shard.
const LEASE_WAIT: Duration = Duration::from_secs(60);

/// Writes `line` at exactly `offset`. A refusal that says the shard has no
/// leader right now (a lost lease, a move, a shard still opening) is waited out.
/// Before each retry the log is read back: if the first attempt landed, the
/// record is there and the retry never happens, so nothing is written twice. A
/// conditional publish also cannot land anywhere but `offset`.
async fn write_at(broker: &impl LogStore, stream: &str, offset: u64, line: &str) -> Result<()> {
    let deadline = Instant::now() + LEASE_WAIT;
    let mut wait = Duration::from_millis(250);
    let mut check = false;
    loop {
        if check {
            match broker.read_to_tip(stream).await {
                Ok((records, tip)) => {
                    if records
                        .iter()
                        .any(|(o, p)| *o == offset && p == line.as_bytes())
                    {
                        return Ok(());
                    }
                    ensure!(
                        tip <= offset,
                        "{stream} holds something else at offset {offset} where the backup has a record; stop other writers and restore into a fresh log"
                    );
                }
                Err(e) if Instant::now() < deadline && is_leaderless(&e) => {
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(4));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        match broker
            .publish_if(stream, line.as_bytes().to_vec(), offset)
            .await
        {
            Ok(ConditionalWrite::Written { offset: at }) => {
                ensure!(
                    at == offset,
                    "the record for offset {offset} landed at offset {at}; the log is now out of step with the backup. Restore into a fresh log."
                );
                return Ok(());
            }
            Ok(ConditionalWrite::Refused { tail }) => bail!(
                "{stream} no longer ends at offset {offset} (it names {tail}); another writer is on it. Stop other writers and restore into a fresh log."
            ),
            Err(e) if Instant::now() < deadline && is_leaderless(&e) => {
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(4));
                check = true;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Leaves offset `at` with no record. On a Raft shard a refused conditional
/// publish stores a blank entry at the offset it would have taken, and names
/// the offset after it. A broker that names `at` took no offset, so it cannot
/// make a blank.
async fn blank(broker: &impl LogStore, stream: &str, at: u64) -> Result<()> {
    // No log ever expects this offset, so the publish is always refused.
    match broker.publish_if(stream, b"{}".to_vec(), u64::MAX).await? {
        ConditionalWrite::Refused { tail } if tail == at + 1 => Ok(()),
        ConditionalWrite::Refused { tail } => bail!(
            "the backup has a blank at offset {at}, and this broker consumed no offset when it refused a conditional publish (it names {tail}), so the blank cannot be reproduced. Only a Raft shard can restore this backup. The records before offset {at} are in; run the import against a Raft broker to carry on."
        ),
        ConditionalWrite::Written { offset } => bail!(
            "a conditional publish that should have been refused was written at offset {offset}; stop other writers and check {stream}"
        ),
    }
}

pub(crate) async fn digest(log: Log, target: Target, args: DigestArgs) -> Result<()> {
    let limit = match &args.backup {
        Some(path) => {
            let text = fs::read_to_string(path).with_context(|| format!("read {path}"))?;
            Some(parse_backup(log, &text)?.next)
        }
        None => None,
    };
    let broker = Session::sign_in(log.target(target)).await?.broker().await?;
    let (mut records, _) = broker.read_to_tip(log.stream()).await?;
    if let Some(next) = limit {
        records.retain(|(offset, _)| *offset < next);
    }
    let (canonical, shown) = digest_json(log, &records)?;
    if let Some(path) = args.state {
        write_file(&path, &format!("{canonical}\n"))?;
    }
    println!("{shown}");
    Ok(())
}

fn digest_json(log: Log, records: &[(u64, Vec<u8>)]) -> Result<(String, Value)> {
    let (canonical, digest) = log.digest(records)?;
    let at = records.last().map(|(o, _)| *o);
    Ok((
        canonical,
        json!({ "records": records.len(), "at": at, "sha256": digest }),
    ))
}

/// A backup file read back: one line for each offset from `first` up to `next`.
/// A line with nothing on it is an offset that held no record.
struct Backup<'a> {
    first: u64,
    next: u64,
    lines: Vec<&'a str>,
}

impl<'a> Backup<'a> {
    /// The record at `offset`, if the backup has one there.
    fn line(&self, offset: u64) -> Option<&'a str> {
        let at = usize::try_from(offset.checked_sub(self.first)?).ok()?;
        self.lines.get(at).copied().filter(|l| !l.is_empty())
    }

    fn records(&self) -> impl Iterator<Item = (u64, &'a str)> + '_ {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.is_empty())
            .map(|(i, l)| (self.first + i as u64, *l))
    }

    fn record_offsets(&self) -> impl Iterator<Item = u64> + '_ {
        self.records().map(|(o, _)| o)
    }
}

/// The backup's lines, after checking its header against them.
fn parse_backup(log: Log, text: &str) -> Result<Backup<'_>> {
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().context("the backup file is empty")?)
        .context("the first line is not a backup header")?;
    ensure!(header["backup"] == MAGIC, "this is not a kbl backup file");
    ensure!(
        header["version"] == VERSION,
        "this backup is of another version; use the kbl-tool that wrote it"
    );
    ensure!(
        header["stream"] == log.stream(),
        "this is a backup of {}, not {}",
        header["stream"],
        log.stream()
    );
    let lines: Vec<&str> = lines.collect();
    let first = header["first_offset"]
        .as_u64()
        .context("the header has no first_offset")?;
    let next = header["next_offset"]
        .as_u64()
        .context("the header has no next_offset")?;
    ensure!(
        next >= first && next - first == lines.len() as u64,
        "the header covers offsets {first} to {next} and the file holds {} lines: it was cut short or edited",
        lines.len()
    );
    let records = lines.iter().filter(|l| !l.is_empty()).count();
    ensure!(
        header["records"] == records as u64,
        "the header says {} records and the file holds {records}: it was cut short or edited",
        header["records"],
    );
    ensure!(
        header["sha256"] == hash_lines(&lines),
        "the records do not match the header's checksum: the file was edited or damaged"
    );
    Ok(Backup { first, next, lines })
}

fn hash_lines(lines: &[&str]) -> String {
    let mut hash = Sha256::new();
    for line in lines {
        hash.update(line.as_bytes());
        hash.update(b"\n");
    }
    hex(&hash.finalize())
}

/// The fold as JSON with every object's keys in order, and its SHA-256. The
/// state's own `PartialEq` is not what a restore is held to: this is every
/// field it serializes, compared as bytes.
pub(crate) fn canonical(state: &State) -> Result<(String, String)> {
    // serde_json's maps are ordered by key, so equal states print equal bytes.
    let text = serde_json::to_string(&serde_json::to_value(state)?)?;
    let digest = hex(&Sha256::digest(text.as_bytes()));
    Ok((text, digest))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `text` to a file only its owner can read: a backup holds every
/// record, and the org log holds who may do what.
fn write_file(path: &str, text: &str) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("write {path}"))?;
    // The mode above applies only to a file this call creates.
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    file.write_all(text.as_bytes())?;
    file.sync_all().with_context(|| format!("write {path}"))
}

#[cfg(test)]
mod tests;
