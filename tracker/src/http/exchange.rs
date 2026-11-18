//! One-time codes for workers a hook starts: the hosted agent trades its code
//! for the worker's token once, so the token never travels through the hook.
//! Codes live in memory only; after a restart the supervisor starts again.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kbl_core::org::TokenHash;

/// How long a code can be traded.
pub(super) const CODE_TTL: Duration = Duration::from_secs(600);
/// Codes waiting at once, across teams, so the map cannot grow unbounded.
pub(super) const MAX_CODES: usize = 1024;

struct Pending {
    worker: String,
    token: String,
    until: Instant,
}

/// Codes by their hash, so a memory dump of the map holds no live code.
#[derive(Default)]
pub(super) struct Codes(Mutex<HashMap<String, Pending>>);

impl Codes {
    /// Holds `token` behind a new code and answers the code, or None when
    /// too many codes are waiting.
    pub(super) fn issue(&self, worker: &str, token: String, now: Instant) -> Option<String> {
        let code = draw()?;
        let mut codes = self.0.lock().unwrap_or_else(|e| e.into_inner());
        codes.retain(|_, p| p.until > now);
        if codes.len() >= MAX_CODES {
            return None;
        }
        codes.insert(
            TokenHash::of(&code).as_str().to_owned(),
            Pending {
                worker: worker.to_owned(),
                token,
                until: now + CODE_TTL,
            },
        );
        Some(code)
    }

    /// The worker and token behind `code`, once: the code is gone after this
    /// whether it was still good or not.
    pub(super) fn redeem(&self, code: &str, now: Instant) -> Option<(String, String)> {
        let mut codes = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let p = codes.remove(TokenHash::of(code).as_str())?;
        (p.until > now).then_some((p.worker, p.token))
    }
}

fn draw() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(format!(
        "kblc_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

#[cfg(test)]
mod tests;
