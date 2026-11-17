//! Trust in agents' summaries: how many of the summaries a person has marked
//! were marked accurate. A team figure over the marks the log holds; nothing
//! here names the agent, the model or the person who marked.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::State;
use crate::names::ItemId;

/// Marks below this many in a window are too few to read a share from.
pub const MIN_SAMPLE: u64 = 10;

/// When each attempt's standing mark was written, by item and attempt. A
/// re-mark replaces the time, so an attempt is in one window only. Not in the
/// wire form, so a state decoded from it has marks with no time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct MarkTimes(BTreeMap<ItemId, BTreeMap<u32, u64>>);

impl PartialEq for MarkTimes {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for MarkTimes {}

impl MarkTimes {
    pub(super) fn set(&mut self, item: &ItemId, attempt: u32, at_ms: u64) {
        self.0
            .entry(item.clone())
            .or_default()
            .insert(attempt, at_ms);
    }
}

/// Summary marks over a window, for one team.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustStats {
    pub from_ms: u64,
    pub to_ms: u64,
    /// Attempts whose standing mark was written inside the window.
    pub rated: u64,
    pub accurate: u64,
    pub not_accurate: u64,
    /// `accurate` as a share of `rated`, in thousandths; none with nothing rated.
    pub accurate_permille: Option<u64>,
    /// Finished attempts nobody has marked, as of now, whatever the window.
    pub unrated: u64,
    /// Fewer marks than this is too few to read a share from.
    pub min_sample: u64,
    /// Marks with no time, so left out of the window: the state was decoded
    /// from its wire form.
    pub undated: u64,
}

impl State {
    /// Standing summary marks written in `from_ms..to_ms`, and the finished
    /// attempts with none. Each attempt counts once, by its latest mark.
    pub fn summary_trust(&self, from_ms: u64, to_ms: u64) -> TrustStats {
        let mut s = TrustStats {
            from_ms,
            to_ms,
            min_sample: MIN_SAMPLE,
            ..TrustStats::default()
        };
        for it in self.items.values() {
            for p in it.paths.iter().filter(|p| p.done) {
                let Some(accurate) = p.summary_accurate else {
                    s.unrated += 1;
                    continue;
                };
                let Some(at) = self
                    .mark_times
                    .0
                    .get(&it.id)
                    .and_then(|m| m.get(&p.attempt))
                else {
                    s.undated += 1;
                    continue;
                };
                if (from_ms..to_ms).contains(at) {
                    s.rated += 1;
                    if accurate {
                        s.accurate += 1;
                    } else {
                        s.not_accurate += 1;
                    }
                }
            }
        }
        s.accurate_permille = (s.rated > 0).then(|| s.accurate * 1000 / s.rated);
        s
    }
}
