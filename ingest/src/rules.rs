//! Which rule an issue belongs to. The first rule that matches wins.

use crate::config::Rule;
use crate::source::Issue;

impl Rule {
    /// Whether the issue passes this rule's filter. Everything listed must
    /// hold; labels compare without regard to case, as GitHub does.
    pub(crate) fn matches(&self, issue: &Issue) -> bool {
        issue.id.repo.eq_ignore_ascii_case(&self.repo) && self.filter(issue)
    }

    /// The filter without the repository. An imported issue that is
    /// transferred keeps its item, so only the rest decides whether it stays
    /// in scope.
    pub(crate) fn filter(&self, issue: &Issue) -> bool {
        let has = |l: &str| issue.labels.iter().any(|x| x.eq_ignore_ascii_case(l));
        self.labels_all.iter().all(|l| has(l))
            && (self.labels_any.is_empty() || self.labels_any.iter().any(|l| has(l)))
            && !self.labels_excluded.iter().any(|l| has(l))
            && self
                .milestone
                .as_ref()
                .is_none_or(|m| issue.milestone.as_ref().is_some_and(|x| x == m))
            && self
                .assignee
                .as_ref()
                .is_none_or(|a| issue.assignees.iter().any(|x| x.eq_ignore_ascii_case(a)))
            && self.issue_type.as_ref().is_none_or(|t| {
                issue
                    .kind
                    .as_ref()
                    .is_some_and(|x| x.eq_ignore_ascii_case(t))
            })
    }

    /// The tier the issue's priority labels ask for, or the rule's default.
    pub(crate) fn tier_for(&self, issue: &Issue) -> u8 {
        issue
            .labels
            .iter()
            .filter_map(|l| {
                self.priority_labels
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(l))
                    .map(|(_, t)| *t)
            })
            .min()
            .unwrap_or(self.default_tier)
    }
}

/// The first rule, in the order listed, that the issue matches.
pub(crate) fn first_match<'a>(rules: &'a [Rule], issue: &Issue) -> Option<&'a Rule> {
    rules.iter().find(|r| r.matches(issue))
}

#[cfg(test)]
mod tests;
