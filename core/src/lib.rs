//! The rules every Felix Agents program shares: what a run's resources are
//! called, what a record on the run log looks like, and what a task is.
//!
//! `kbl-mcp`, `kbl-run` and `kbl-tool` link this crate, and the viewer
//! uses it as WebAssembly, so nothing here touches the clock, the network or
//! the file system. Only [`legacy_env`] reads the environment, once, at startup.
//!
//! - [`codec`]: JSON encode and decode for everything below.
//! - [`names`]: the run id and key rules, and the nine resources of a run.
//! - [`artifact`]: content-hash ids for big payloads, and the inline limit.
//! - [`content`]: a short text kept inline, or a pointer to an artifact.
//! - [`task`]: the task that goes on the tasks stream.
//! - [`record`]: one record per step on the run log.
//! - [`work`]: the records on `work.events`, and the key shapes of items, edges, context and budgets.
//! - [`work_fold`]: the pure fold from a prefix of `work.events` to item state, with snapshots.
//! - [`overlap`]: open items that expect to touch the same files, from their `files` context entries.
//! - [`org`]: the records on `org.events`: teams, people, roles, agents, sources, grants, and token hashes.
//! - [`org_fold`]: the fold from `org.events` to the tables the tracker serves teams from.
//! - [`story`]: an item's decisions by phase with their edits and evidence, and the flags where claims and recorded steps disagree.
//! - [`legacy_env`]: the old `AGENTS_` variable names, adopted at startup.
//! - [`redact`]: known secret patterns, cut from what an agent reports before it is written.
//! - [`stored`]: the values kept in the results, leases and presence caches.

pub mod artifact;
pub mod codec;
pub mod content;
pub mod legacy_env;
pub mod names;
pub mod org;
pub mod org_fold;
pub mod overlap;
pub mod record;
pub mod redact;
pub mod stored;
pub mod story;
pub mod task;
pub mod work;
pub mod work_fold;
