//! The rules every Felix Agents program shares: what a run's resources are
//! called, what a record on the run log looks like, and what a task is.
//!
//! `felix-mcp`, `agents-run` and `agents-tool` link this crate, and the viewer
//! uses it as WebAssembly, so nothing here touches the clock, the network or
//! the file system.
//!
//! - [`codec`]: JSON encode and decode for everything below.
//! - [`names`]: the run id and key rules, and the nine resources of a run.
//! - [`artifact`]: content-hash ids for big payloads, and the inline limit.
//! - [`content`]: a short text kept inline, or a pointer to an artifact.
//! - [`task`]: the task that goes on the tasks stream.
//! - [`record`]: one record per step on the run log.
//! - [`stored`]: the values kept in the results, leases and presence caches.

pub mod artifact;
pub mod codec;
pub mod content;
pub mod names;
pub mod record;
pub mod stored;
pub mod task;
