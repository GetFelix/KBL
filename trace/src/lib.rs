//! OpenTelemetry traces for every KBL service. A service calls [`init`] once
//! and adds spans with the `tracing` macros. With no OTLP endpoint set,
//! nothing is installed: spans are disabled at their call site, and no trace
//! context is read or written. See docs/observability.md for the spans and
//! the attribute policy.
//!
//! Attributes are route templates, tool names, source kinds, team names and
//! result words, never item ids, text, tokens or user names.

mod context;
mod setup;
#[cfg(feature = "test-util")]
mod testing;

pub use context::{adopt_parent, headers};
pub use setup::{Guard, init};
#[cfg(feature = "test-util")]
pub use testing::{Capture, Recorded, capture};
