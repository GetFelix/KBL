//! Which rule an issue belongs to. The first rule that matches wins.

use std::collections::BTreeMap;

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
        // A JQL filter or saved query ran upstream, which says which rules
        // selected the issue.
        if self.jql.is_some() || self.ado_query.is_some() {
            return issue.scopes.contains(&self.name);
        }
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

    /// The repository agents work in. None only for a Jira or Azure DevOps
    /// issue whose components or area path map to nothing and whose rule
    /// names no `target_repo`: it goes to Needs you until a mapping exists.
    pub(crate) fn target_for(&self, issue: &Issue) -> Option<String> {
        if self.ado {
            return self.area_repo(issue).or_else(|| self.target_repo.clone());
        }
        if self.jql.is_none() {
            return Some(
                self.target_repo
                    .clone()
                    .unwrap_or_else(|| issue.id.repo.clone()),
            );
        }
        issue
            .components
            .iter()
            .find_map(|c| {
                self.components
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(c))
                    .map(|(_, repo)| repo.clone())
            })
            .or_else(|| self.target_repo.clone())
    }

    /// The repository of the longest listed area path that is the issue's
    /// area or an ancestor of it. Paths compare without regard to case, and
    /// `Proj\Team` is not an ancestor of `Proj\Teamster`.
    fn area_repo(&self, issue: &Issue) -> Option<String> {
        let area = |s: &str| s.trim_matches('\\').to_ascii_lowercase();
        issue
            .components
            .iter()
            .flat_map(|c| {
                let c = area(c);
                self.area_paths.iter().filter_map(move |(k, repo)| {
                    let k = area(k);
                    (c == k || c.strip_prefix(&k).is_some_and(|r| r.starts_with('\\')))
                        .then(|| (k.len(), repo.clone()))
                })
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, repo)| repo)
    }

    /// The tier the issue's priority labels ask for, or the rule's default.
    /// A Jira rule also maps the priority name, with Highest to Lowest as 0
    /// to 3 unless the rule's map says otherwise; an Azure DevOps rule maps
    /// Priority 1 to 4 to 0 to 3.
    pub(crate) fn tier_for(&self, issue: &Issue) -> u8 {
        let named = issue.priority.iter().filter_map(|p| {
            self.priority_labels
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(p))
                .map(|(_, t)| *t)
                .or_else(|| {
                    (self.jql.is_some() || self.ado)
                        .then(|| default_tier_of(p))
                        .flatten()
                })
        });
        named
            .chain(issue.labels.iter().filter_map(|l| {
                self.priority_labels
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(l))
                    .map(|(_, t)| *t)
            }))
            .chain(issue.milestone.iter().filter_map(|m| {
                self.milestone_tiers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(m))
                    .map(|(_, t)| *t)
            }))
            .min()
            .unwrap_or(self.default_tier)
    }

    /// The type and area the issue's labels map to: the first label, in the
    /// order upstream lists them, that the rule maps for each.
    pub(crate) fn mapped_class(&self, issue: &Issue) -> (Option<String>, Option<String>) {
        let pick = |map: &BTreeMap<String, String>| {
            issue.labels.iter().find_map(|l| {
                map.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(l))
                    .map(|(_, v)| v.clone())
            })
        };
        (pick(&self.label_types), pick(&self.label_areas))
    }
}

fn default_tier_of(priority: &str) -> Option<u8> {
    match priority.to_ascii_lowercase().as_str() {
        "highest" | "blocker" | "critical" | "1" => Some(0),
        "high" | "major" | "2" => Some(1),
        "medium" | "3" => Some(2),
        "low" | "lowest" | "minor" | "trivial" | "4" => Some(3),
        _ => None,
    }
}

/// The first rule, in the order listed, that the issue matches.
pub(crate) fn first_match<'a>(rules: &'a [Rule], issue: &Issue) -> Option<&'a Rule> {
    rules.iter().find(|r| r.matches(issue))
}

#[cfg(test)]
mod tests;
