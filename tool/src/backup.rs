//! `work export`, `work import` and `work digest`: a backup of `work.events`
//! and the check that a restore rebuilt the same state. See docs/backup-restore.md.

use std::fs;
use std::io::Write;

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use kbl_core::work::WORK_EVENTS;
use kbl_core::work_fold::State;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::felix::{Session, Target};

/// The file's first line names it, so a file of something else is refused.
const MAGIC: &str = "kbl-backup";
const VERSION: u64 = 1;

#[derive(Args)]
pub(crate) struct ExportArgs {
    /// The file to write. It is replaced if it exists.
    file: String,
}

#[derive(Args)]
pub(crate) struct ImportArgs {
    /// A file written by `work export`.
    file: String,
}

#[derive(Args)]
pub(crate) struct DigestArgs {
    /// Also write the fold's canonical JSON here, to see what differs when two digests do.
    #[arg(long, value_name = "FILE")]
    state: Option<String>,
}

pub(crate) async fn export(target: Target, args: ExportArgs) -> Result<()> {
    let broker = Session::sign_in(target).await?.broker().await?;
    let records = broker.read_all(WORK_EVENTS).await?;
    let first = records.first().map_or(0, |(offset, _)| *offset);
    let mut lines = Vec::with_capacity(records.len());
    for (index, (offset, payload)) in records.iter().enumerate() {
        // A restore publishes in order and takes the offsets the broker gives,
        // so the fold only matches if the log has none missing.
        ensure!(
            *offset == first + index as u64,
            "{WORK_EVENTS} skips from offset {} to {offset}; it cannot be restored",
            first + index as u64 - 1
        );
        let text = std::str::from_utf8(payload)
            .with_context(|| format!("the record at offset {offset} is not text"))?;
        ensure!(
            !text.contains('\n') && serde_json::from_str::<Value>(text).is_ok(),
            "the record at offset {offset} is not one line of JSON"
        );
        lines.push(text);
    }
    let header = json!({
        "backup": MAGIC,
        "version": VERSION,
        "stream": WORK_EVENTS,
        "first_offset": first,
        "records": lines.len(),
        "sha256": hash_lines(&lines),
    });
    let mut out = format!("{header}\n");
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    write_file(&args.file, &out)?;
    println!("exported {} records to {}", lines.len(), args.file);
    Ok(())
}

pub(crate) async fn import(target: Target, args: ImportArgs) -> Result<()> {
    let text = fs::read_to_string(&args.file).with_context(|| format!("read {}", args.file))?;
    let (first, lines) = parse_backup(&text)?;
    let broker = Session::sign_in(target).await?.broker().await?;
    let existing = broker.read_all(WORK_EVENTS).await?.len();
    if existing != 0 {
        bail!(
            "{WORK_EVENTS} already holds {existing} records. A restore goes into an empty log, so offsets match."
        );
    }
    for (index, line) in lines.iter().enumerate() {
        let offset = broker
            .publish(WORK_EVENTS, line.as_bytes().to_vec())
            .await?;
        let wanted = first + index as u64;
        ensure!(
            offset == wanted,
            "record {index} landed at offset {offset} where the backup had {wanted}; the fold would differ"
        );
    }
    println!("imported {} records into {WORK_EVENTS}", lines.len());
    Ok(())
}

pub(crate) async fn digest(target: Target, args: DigestArgs) -> Result<()> {
    let broker = Session::sign_in(target).await?.broker().await?;
    let records = broker.read_all(WORK_EVENTS).await?;
    let state = crate::fold_log(&records)?;
    let (canonical, digest) = canonical(&state)?;
    if let Some(path) = args.state {
        write_file(&path, &format!("{canonical}\n"))?;
    }
    println!(
        "{}",
        json!({ "records": records.len(), "at": state.at, "sha256": digest })
    );
    Ok(())
}

/// The first offset and the backup's records, after checking its header against them.
fn parse_backup(text: &str) -> Result<(u64, Vec<&str>)> {
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().context("the backup file is empty")?)
        .context("the first line is not a backup header")?;
    ensure!(header["backup"] == MAGIC, "this is not a kbl backup file");
    ensure!(
        header["version"] == VERSION && header["stream"] == WORK_EVENTS,
        "this backup is of another version or stream; use the kbl-tool that wrote it"
    );
    let records: Vec<&str> = lines.collect();
    ensure!(
        header["records"] == records.len() as u64,
        "the header says {} records and the file holds {}: it was cut short or edited",
        header["records"],
        records.len()
    );
    ensure!(
        header["sha256"] == hash_lines(&records),
        "the records do not match the header's checksum: the file was edited or damaged"
    );
    let first = header["first_offset"]
        .as_u64()
        .context("the header has no first_offset")?;
    Ok((first, records))
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

fn write_file(path: &str, text: &str) -> Result<()> {
    let mut file = fs::File::create(path).with_context(|| format!("write {path}"))?;
    file.write_all(text.as_bytes())?;
    file.sync_all().with_context(|| format!("write {path}"))
}

#[cfg(test)]
mod tests;
