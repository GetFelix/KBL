//! The triage acceptance policy: rules, read from a file, that let the
//! tracker accept a finished triage on its own. A rule only reads the
//! fields triage recorded; there is no model here. The fold decides whether
//! an item may be accepted at all (`State::policy_blocker`), so a rule can
//! only narrow that, never widen it.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use kbl_core::work::TIERS;
use kbl_core::work_fold::Item;
use serde::Deserialize;

/// Longest rule name; the same bound the fold puts on the accepted record.
const MAX_NAME: usize = 120;
/// Longest list a rule carries.
const MAX_LIST: usize = 64;

/// Rules, tried in order. The first that matches names the acceptance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TriagePolicy {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    name: String,
    /// Priority tiers (0 the highest) the triage may have chosen.
    #[serde(default)]
    tiers: Vec<u8>,
    /// The triage must have recorded one of these areas.
    #[serde(default)]
    areas: Vec<String>,
    /// The triage must not have recorded any of these areas.
    #[serde(default)]
    deny_areas: Vec<String>,
    /// The triage must have recorded one of these types.
    #[serde(default)]
    types: Vec<String>,
    /// The triage must have recorded one of these sizes.
    #[serde(default)]
    sizes: Vec<String>,
    /// The item must need none of these.
    #[serde(default)]
    deny_needs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    rules: Vec<Rule>,
}

impl TriagePolicy {
    /// Reads the policy file: `{"rules": [{"name": ..., ...}]}`.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read the triage policy {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse the triage policy {}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let file: File = serde_json::from_str(text)?;
        let mut seen = std::collections::HashSet::new();
        for rule in &file.rules {
            let name = &rule.name;
            ensure!(
                !name.is_empty() && name.len() <= MAX_NAME && name.chars().all(|c| !c.is_control()),
                "a rule needs a name of 1 to {MAX_NAME} plain characters"
            );
            ensure!(
                seen.insert(name.as_str()),
                "the rule {name:?} is listed twice"
            );
            // A rule with no condition would accept every plain triage.
            ensure!(
                !(rule.tiers.is_empty()
                    && rule.areas.is_empty()
                    && rule.deny_areas.is_empty()
                    && rule.types.is_empty()
                    && rule.sizes.is_empty()
                    && rule.deny_needs.is_empty()),
                "the rule {name:?} lists no condition; list tiers to match every tier"
            );
            ensure!(
                rule.tiers.iter().all(|t| *t < TIERS),
                "the rule {name:?} names a tier above {}",
                TIERS - 1
            );
            for list in [
                &rule.areas,
                &rule.deny_areas,
                &rule.types,
                &rule.sizes,
                &rule.deny_needs,
            ] {
                ensure!(
                    list.len() <= MAX_LIST && list.iter().all(|v| !v.is_empty()),
                    "the rule {name:?} has a list that is too long or holds an empty value"
                );
            }
        }
        Ok(Self { rules: file.rules })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The name of the first rule that matches the triage `it` carries.
    /// Values compare without regard to case, as the vocabulary does.
    pub(crate) fn matching(&self, it: &Item) -> Option<&str> {
        let tier = it.proposal.as_ref()?.tier;
        let value = |t: &Option<kbl_core::work_fold::Tag>| t.as_ref().map(|t| t.value.clone());
        let (item_type, area, size) = (
            value(&it.class.item_type),
            value(&it.class.area),
            value(&it.class.size),
        );
        let has = |list: &[String], v: Option<&str>| {
            v.is_some_and(|v| list.iter().any(|l| l.eq_ignore_ascii_case(v)))
        };
        let (item_type, area, size) = (item_type.as_deref(), area.as_deref(), size.as_deref());
        self.rules
            .iter()
            .find(|r| {
                (r.tiers.is_empty() || r.tiers.contains(&tier))
                    && (r.areas.is_empty() || has(&r.areas, area))
                    && !has(&r.deny_areas, area)
                    && (r.types.is_empty() || has(&r.types, item_type))
                    && (r.sizes.is_empty() || has(&r.sizes, size))
                    && !it.class.needs().iter().any(|n| has(&r.deny_needs, Some(n)))
            })
            .map(|r| r.name.as_str())
    }
}

#[cfg(test)]
mod tests;
