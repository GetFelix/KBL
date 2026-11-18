//! The fold of `org.events`: the tables the tracker serves teams from.
//!
//! One function decides whether a record is acceptable: [`OrgState::check`].
//! The tracker calls it before it appends, so a refused call writes nothing,
//! and [`OrgState::apply`] calls it again on replay, so a log folds to the
//! state its writer saw. A record the check refuses is skipped, never half
//! applied. The check covers what the log itself can say (a name is unique,
//! a team exists, the last admin stays). Who may ask is the tracker's check on
//! the caller's role, made before this one.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::names::Key;
use crate::org::{
    MAX_AGENTS_PER_TEAM, MAX_CONNECTION_SOURCES, MAX_CONNECTIONS_PER_TEAM, MAX_GRANT_AGENTS,
    MAX_GRANTS_PER_TEAM, MAX_INGEST_PER_TEAM, MAX_LABEL_BYTES, MAX_ROLE_REPOS, ORG_FORMAT,
    ORG_FORMAT_V1, OrgLogged, OrgRecord, Role, TeamName, TokenHash,
};

#[cfg(test)]
mod tests;

/// Whether a record that passed [`OrgState::check`] changes anything. A record
/// that repeats what the log already says is accepted and changes nothing, so
/// a retried call answers `created: false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Changed,
    Unchanged,
}

/// Why a record was refused. `why` is the short code a route answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgRefusal {
    UnknownTeam,
    UnknownPerson,
    UnknownAgent,
    NameTaken,
    TokenInUse,
    SubjectTaken,
    SourceOwned,
    UnknownSource,
    BadLabel,
    RoleScope,
    LastAdmin,
    LastOrgAdmin,
    OwnGrant,
    TooMany,
    /// Stamped above the log's format, below its kind's, or a raise that
    /// does not go up.
    Format,
    /// A source another connection of the team already serves.
    SourceServed,
    BadBudget,
}

impl OrgRefusal {
    pub fn why(&self) -> &'static str {
        match self {
            OrgRefusal::UnknownTeam => "unknown_team",
            OrgRefusal::UnknownPerson => "unknown_person",
            OrgRefusal::UnknownAgent => "unknown_agent",
            OrgRefusal::NameTaken => "name_taken",
            OrgRefusal::TokenInUse => "token_in_use",
            OrgRefusal::SubjectTaken => "subject_taken",
            OrgRefusal::SourceOwned => "source_owned",
            OrgRefusal::UnknownSource => "unknown_source",
            OrgRefusal::BadLabel => "bad_label",
            OrgRefusal::RoleScope => "role_scope",
            OrgRefusal::LastAdmin => "last_admin",
            OrgRefusal::LastOrgAdmin => "last_org_admin",
            OrgRefusal::OwnGrant => "own_grant",
            OrgRefusal::TooMany => "too_many",
            OrgRefusal::Format => "format",
            OrgRefusal::SourceServed => "source_served",
            OrgRefusal::BadBudget => "bad_budget",
        }
    }
}

impl fmt::Display for OrgRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.why())
    }
}

impl std::error::Error for OrgRefusal {}

/// Whose token it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Person(Key),
    Agent { team: TeamName, agent: Key },
    Ingest { team: TeamName, name: Key },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    pub display_name: Option<String>,
    pub adopted: bool,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub token: TokenHash,
    pub subject: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub role: Role,
    /// Empty is every source of the team.
    pub repos: Vec<String>,
    pub granted_by: Key,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub token: TokenHash,
    pub added_by: Key,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The team whose Triage the grant opens.
    pub team: TeamName,
    pub from_team: TeamName,
    /// Empty is every agent of `from_team`.
    pub agents: Vec<Key>,
    /// Empty is every source of `team`.
    pub repos: Vec<String>,
    pub max_pending: Option<u32>,
}

/// The org as the log built it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub vendor: String,
    pub group: Key,
    pub pool: Key,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub per_hour: u64,
    pub kept_back: u64,
    pub wake_cap: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgState {
    /// The log's format: 1 until a `format_raised`.
    format: u32,
    connections: BTreeMap<(TeamName, Key), Connection>,
    budgets: BTreeMap<Key, Budget>,
    teams: BTreeMap<TeamName, Team>,
    people: BTreeMap<Key, Person>,
    org_admins: BTreeSet<Key>,
    members: BTreeMap<(TeamName, Key), Member>,
    agents: BTreeMap<(TeamName, Key), Agent>,
    ingest: BTreeMap<(TeamName, Key), TokenHash>,
    sources: BTreeMap<String, TeamName>,
    grants: Vec<Grant>,
}

/// The state after every record of `log` that the check accepts.
impl Default for OrgState {
    fn default() -> Self {
        Self {
            format: ORG_FORMAT_V1,
            connections: BTreeMap::new(),
            budgets: BTreeMap::new(),
            teams: BTreeMap::new(),
            people: BTreeMap::new(),
            org_admins: BTreeSet::new(),
            members: BTreeMap::new(),
            agents: BTreeMap::new(),
            ingest: BTreeMap::new(),
            sources: BTreeMap::new(),
            grants: Vec::new(),
        }
    }
}

pub fn fold<'a>(log: impl IntoIterator<Item = &'a OrgLogged>) -> OrgState {
    let mut state = OrgState::default();
    for logged in log {
        let _ = state.apply(logged);
    }
    state
}

impl OrgState {
    pub fn teams(&self) -> impl Iterator<Item = (&TeamName, &Team)> {
        self.teams.iter()
    }

    pub fn has_team(&self, team: &TeamName) -> bool {
        self.teams.contains_key(team)
    }

    pub fn person(&self, person: &Key) -> Option<&Person> {
        self.people.get(person)
    }

    pub fn is_org_admin(&self, person: &Key) -> bool {
        self.org_admins.contains(person)
    }

    pub fn member(&self, team: &TeamName, person: &Key) -> Option<&Member> {
        self.members.get(&(team.clone(), person.clone()))
    }

    /// The person's role in `team`. An org admin is an admin of every team.
    pub fn role_in(&self, team: &TeamName, person: &Key) -> Option<Role> {
        if !self.has_team(team) {
            None
        } else if self.is_org_admin(person) {
            Some(Role::OrgAdmin)
        } else {
            self.member(team, person).map(|m| m.role)
        }
    }

    /// The teams a person belongs to and their role in each. An org admin
    /// sees every team.
    pub fn teams_of(&self, person: &Key) -> Vec<(TeamName, Role)> {
        self.teams
            .keys()
            .filter_map(|t| self.role_in(t, person).map(|r| (t.clone(), r)))
            .collect()
    }

    pub fn agent(&self, team: &TeamName, agent: &Key) -> Option<&Agent> {
        self.agents.get(&(team.clone(), agent.clone()))
    }

    /// The team that owns `source`.
    /// Every agent of `team`, revoked ones too, by name.
    pub fn agents_in<'a>(
        &'a self,
        team: &'a TeamName,
    ) -> impl Iterator<Item = (&'a Key, &'a Agent)> {
        self.agents
            .iter()
            .filter(move |((t, _), _)| t == team)
            .map(|((_, a), agent)| (a, agent))
    }

    pub fn source_owner(&self, source: &str) -> Option<&TeamName> {
        self.sources.get(source)
    }

    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    /// The log's format.
    pub fn format(&self) -> u32 {
        self.format
    }

    /// The team's connection that worker group `group` serves, by name.
    pub fn connection_of_group(&self, team: &TeamName, group: &Key) -> Option<(&Key, &Connection)> {
        self.connections
            .iter()
            .find(|((t, _), c)| t == team && c.group == *group)
            .map(|((_, name), c)| (name, c))
    }

    /// Whether one of `team`'s connections spends `pool`. A token tied to a
    /// vendor counts only that vendor's connections.
    pub fn team_spends_pool(&self, team: &TeamName, pool: &Key, tied: Option<&str>) -> bool {
        self.connections
            .iter()
            .any(|((t, _), c)| t == team && c.pool == *pool && tied.is_none_or(|v| v == c.vendor))
    }

    /// How many sources every connection spending `pool` serves, across teams.
    pub fn pool_sources(&self, pool: &Key) -> usize {
        self.connections
            .values()
            .filter(|c| c.pool == *pool)
            .map(|c| c.sources.len())
            .sum()
    }

    pub fn budget(&self, pool: &Key) -> Option<Budget> {
        self.budgets.get(pool).copied()
    }

    /// Whose token `token` is. Revoked agents have none. Every table is
    /// compared whatever the answer, so the time taken says nothing about
    /// which entry matched.
    pub fn who(&self, token: &str) -> Option<Principal> {
        let hash = TokenHash::of(token);
        let mut found = None;
        for (person, p) in &self.people {
            if p.token.ct_eq(&hash) {
                found = Some(Principal::Person(person.clone()));
            }
        }
        for ((team, agent), a) in &self.agents {
            if a.token.ct_eq(&hash) && !a.revoked {
                found = Some(Principal::Agent {
                    team: team.clone(),
                    agent: agent.clone(),
                });
            }
        }
        for ((team, name), t) in &self.ingest {
            if t.ct_eq(&hash) {
                found = Some(Principal::Ingest {
                    team: team.clone(),
                    name: name.clone(),
                });
            }
        }
        found
    }

    fn token_in_use(&self, hash: &TokenHash) -> bool {
        self.people.values().any(|p| p.token == *hash)
            || self.agents.values().any(|a| a.token == *hash)
            || self.ingest.values().any(|t| t == hash)
    }

    fn admins_in(&self, team: &TeamName) -> usize {
        self.members
            .iter()
            .filter(|((t, _), m)| t == team && m.role == Role::Admin && m.repos.is_empty())
            .count()
    }

    fn owns_all(&self, team: &TeamName, repos: &[String]) -> bool {
        repos.iter().all(|r| self.sources.get(r) == Some(team))
    }

    fn need_team(&self, team: &TeamName) -> Result<(), OrgRefusal> {
        if self.has_team(team) {
            Ok(())
        } else {
            Err(OrgRefusal::UnknownTeam)
        }
    }

    /// Whether the log may take this record, and whether it would change
    /// anything. Pure: the state is not touched.
    pub fn check(&self, logged: &OrgLogged) -> Result<Applied, OrgRefusal> {
        use Applied::{Changed, Unchanged};
        if logged.v < logged.record.format() {
            return Err(OrgRefusal::Format);
        }
        if let OrgRecord::FormatRaised { to } = logged.record {
            // Never past what this build reads: a raise above it would leave the log
            // unreadable to every build that exists.
            return if to > self.format && to <= ORG_FORMAT && logged.v == to {
                Ok(Changed)
            } else {
                Err(OrgRefusal::Format)
            };
        }
        // Nothing lands that a build still allowed on the log cannot read.
        if logged.v > self.format {
            return Err(OrgRefusal::Format);
        }
        match &logged.record {
            OrgRecord::FormatRaised { .. } => Err(OrgRefusal::Format),
            OrgRecord::ConnectionSet {
                team,
                name,
                vendor,
                group,
                pool,
                sources,
            } => {
                self.need_team(team)?;
                if !label_ok(vendor) {
                    return Err(OrgRefusal::BadLabel);
                }
                // The vendor tie on an ingest token trusts this field, so it
                // must name every source's own vendor.
                let prefix = format!("{vendor}:");
                if !sources.iter().all(|s| s.starts_with(&prefix)) {
                    return Err(OrgRefusal::UnknownSource);
                }
                if sources.len() > MAX_CONNECTION_SOURCES {
                    return Err(OrgRefusal::TooMany);
                }
                let mut seen = BTreeSet::new();
                if !sources.iter().all(|s| seen.insert(s)) || !self.owns_all(team, sources) {
                    return Err(OrgRefusal::UnknownSource);
                }
                // One connection per source, so one group leases it.
                let served_elsewhere = self.connections.iter().any(|((t, n), c)| {
                    t == team && n != name && c.sources.iter().any(|s| seen.contains(s))
                });
                if served_elsewhere {
                    return Err(OrgRefusal::SourceServed);
                }
                // One connection per group, so a worker's config names one.
                if self
                    .connections
                    .iter()
                    .any(|((t, n), c)| t == team && n != name && c.group == *group)
                {
                    return Err(OrgRefusal::NameTaken);
                }
                let key = (team.clone(), name.clone());
                let next = Connection {
                    vendor: vendor.clone(),
                    group: group.clone(),
                    pool: pool.clone(),
                    sources: sources.clone(),
                };
                match self.connections.get(&key) {
                    Some(c) if *c == next => Ok(Unchanged),
                    Some(_) => Ok(Changed),
                    None if self.connections.keys().filter(|(t, _)| t == team).count()
                        >= MAX_CONNECTIONS_PER_TEAM =>
                    {
                        Err(OrgRefusal::TooMany)
                    }
                    None => Ok(Changed),
                }
            }
            OrgRecord::BudgetSet {
                pool,
                per_hour,
                kept_back,
                wake_cap,
            } => {
                if *per_hour == 0 || kept_back > per_hour || wake_cap > kept_back {
                    return Err(OrgRefusal::BadBudget);
                }
                let next = Budget {
                    per_hour: *per_hour,
                    kept_back: *kept_back,
                    wake_cap: *wake_cap,
                };
                Ok(if self.budgets.get(pool) == Some(&next) {
                    Unchanged
                } else {
                    Changed
                })
            }
            OrgRecord::TeamCreated { display_name, .. }
                if display_name.as_deref().is_some_and(|n| !label_ok(n)) =>
            {
                Err(OrgRefusal::BadLabel)
            }
            OrgRecord::TeamCreated {
                team,
                display_name,
                adopted,
            } => match self.teams.get(team) {
                None => Ok(Changed),
                Some(t) if t.display_name == *display_name && t.adopted == *adopted => {
                    Ok(Unchanged)
                }
                Some(_) => Err(OrgRefusal::NameTaken),
            },
            OrgRecord::PersonAdded {
                person,
                token_sha256,
                subject,
            } => {
                if subject.as_deref().is_some_and(|s| !label_ok(s)) {
                    return Err(OrgRefusal::BadLabel);
                }
                if let Some(existing) = self.people.get(person) {
                    return if existing.token == *token_sha256 && existing.subject == *subject {
                        Ok(Unchanged)
                    } else {
                        Err(OrgRefusal::NameTaken)
                    };
                }
                if self.token_in_use(token_sha256) {
                    return Err(OrgRefusal::TokenInUse);
                }
                if subject.is_some() && self.people.values().any(|p| p.subject == *subject) {
                    return Err(OrgRefusal::SubjectTaken);
                }
                Ok(Changed)
            }
            OrgRecord::RoleGranted {
                team,
                person,
                role,
                repos,
            } => {
                if !self.people.contains_key(person) {
                    return Err(OrgRefusal::UnknownPerson);
                }
                if repos.len() > MAX_ROLE_REPOS {
                    return Err(OrgRefusal::TooMany);
                }
                match (team, role) {
                    (None, Role::OrgAdmin) if repos.is_empty() => {
                        Ok(if self.is_org_admin(person) {
                            Unchanged
                        } else {
                            Changed
                        })
                    }
                    (Some(_), Role::Admin) if !repos.is_empty() => Err(OrgRefusal::RoleScope),
                    (Some(team), role) if *role != Role::OrgAdmin => {
                        self.need_team(team)?;
                        if !self.owns_all(team, repos) {
                            return Err(OrgRefusal::UnknownSource);
                        }
                        match self.member(team, person) {
                            Some(m) if m.role == *role && m.repos == *repos => Ok(Unchanged),
                            Some(m)
                                if m.role == Role::Admin
                                    && m.repos.is_empty()
                                    && (*role != Role::Admin || !repos.is_empty())
                                    && self.admins_in(team) == 1 =>
                            {
                                Err(OrgRefusal::LastAdmin)
                            }
                            _ => Ok(Changed),
                        }
                    }
                    _ => Err(OrgRefusal::RoleScope),
                }
            }
            OrgRecord::RoleRevoked { team: None, person } => {
                if !self.is_org_admin(person) {
                    Ok(Unchanged)
                } else if self.org_admins.len() == 1 {
                    Err(OrgRefusal::LastOrgAdmin)
                } else {
                    Ok(Changed)
                }
            }
            OrgRecord::RoleRevoked {
                team: Some(team),
                person,
            } => {
                self.need_team(team)?;
                match self.member(team, person) {
                    None => Ok(Unchanged),
                    Some(m)
                        if m.role == Role::Admin
                            && m.repos.is_empty()
                            && self.admins_in(team) == 1 =>
                    {
                        Err(OrgRefusal::LastAdmin)
                    }
                    Some(_) => Ok(Changed),
                }
            }
            OrgRecord::AgentAdded {
                team,
                agent,
                token_sha256,
            } => {
                self.need_team(team)?;
                if let Some(existing) = self.agent(team, agent) {
                    return if existing.token == *token_sha256 && !existing.revoked {
                        Ok(Unchanged)
                    } else {
                        Err(OrgRefusal::NameTaken)
                    };
                }
                if self.token_in_use(token_sha256) {
                    return Err(OrgRefusal::TokenInUse);
                }
                let live = self
                    .agents
                    .iter()
                    .filter(|((t, _), a)| t == team && !a.revoked)
                    .count();
                if live >= MAX_AGENTS_PER_TEAM {
                    return Err(OrgRefusal::TooMany);
                }
                Ok(Changed)
            }
            OrgRecord::AgentRevoked { team, agent } => {
                self.need_team(team)?;
                match self.agent(team, agent) {
                    None => Err(OrgRefusal::UnknownAgent),
                    Some(a) => Ok(if a.revoked { Unchanged } else { Changed }),
                }
            }
            OrgRecord::SourceAdded { team, source } => {
                self.need_team(team)?;
                if !label_ok(source) {
                    return Err(OrgRefusal::BadLabel);
                }
                match self.source_owner(source) {
                    None => Ok(Changed),
                    Some(owner) if owner == team => Ok(Unchanged),
                    Some(_) => Err(OrgRefusal::SourceOwned),
                }
            }
            OrgRecord::IngestAdded {
                team,
                name,
                token_sha256,
            } => {
                self.need_team(team)?;
                if let Some(existing) = self.ingest.get(&(team.clone(), name.clone())) {
                    return if existing == token_sha256 {
                        Ok(Unchanged)
                    } else {
                        Err(OrgRefusal::NameTaken)
                    };
                }
                if self.token_in_use(token_sha256) {
                    return Err(OrgRefusal::TokenInUse);
                }
                if self.ingest.keys().filter(|(t, _)| t == team).count() >= MAX_INGEST_PER_TEAM {
                    return Err(OrgRefusal::TooMany);
                }
                Ok(Changed)
            }
            OrgRecord::GrantAdded {
                team,
                from_team,
                agents,
                repos,
                max_pending,
            } => {
                self.need_team(team)?;
                self.need_team(from_team)?;
                if team == from_team {
                    return Err(OrgRefusal::OwnGrant);
                }
                if agents.len() > MAX_GRANT_AGENTS || repos.len() > MAX_ROLE_REPOS {
                    return Err(OrgRefusal::TooMany);
                }
                if !self.owns_all(team, repos) {
                    return Err(OrgRefusal::UnknownSource);
                }
                let grant = Grant {
                    team: team.clone(),
                    from_team: from_team.clone(),
                    agents: agents.clone(),
                    repos: repos.clone(),
                    max_pending: *max_pending,
                };
                if self.grants.contains(&grant) {
                    return Ok(Unchanged);
                }
                if self.grants.iter().filter(|g| g.team == *team).count() >= MAX_GRANTS_PER_TEAM {
                    return Err(OrgRefusal::TooMany);
                }
                Ok(Changed)
            }
        }
    }

    /// Takes a record into the tables if the check accepts it.
    pub fn apply(&mut self, logged: &OrgLogged) -> Result<Applied, OrgRefusal> {
        let applied = self.check(logged)?;
        if applied == Applied::Unchanged {
            return Ok(applied);
        }
        let by = logged.by.clone();
        match &logged.record {
            OrgRecord::TeamCreated {
                team,
                display_name,
                adopted,
            } => {
                self.teams.insert(
                    team.clone(),
                    Team {
                        display_name: display_name.clone(),
                        adopted: *adopted,
                        created_ms: logged.at_ms,
                    },
                );
            }
            OrgRecord::PersonAdded {
                person,
                token_sha256,
                subject,
            } => {
                self.people.insert(
                    person.clone(),
                    Person {
                        token: token_sha256.clone(),
                        subject: subject.clone(),
                    },
                );
            }
            OrgRecord::RoleGranted {
                team: None, person, ..
            } => {
                self.org_admins.insert(person.clone());
            }
            OrgRecord::RoleGranted {
                team: Some(team),
                person,
                role,
                repos,
            } => {
                self.members.insert(
                    (team.clone(), person.clone()),
                    Member {
                        role: *role,
                        repos: repos.clone(),
                        granted_by: by,
                    },
                );
            }
            OrgRecord::RoleRevoked { team: None, person } => {
                self.org_admins.remove(person);
            }
            OrgRecord::RoleRevoked {
                team: Some(team),
                person,
            } => {
                self.members.remove(&(team.clone(), person.clone()));
            }
            OrgRecord::AgentAdded {
                team,
                agent,
                token_sha256,
            } => {
                self.agents.insert(
                    (team.clone(), agent.clone()),
                    Agent {
                        token: token_sha256.clone(),
                        added_by: by,
                        revoked: false,
                    },
                );
            }
            OrgRecord::AgentRevoked { team, agent } => {
                if let Some(a) = self.agents.get_mut(&(team.clone(), agent.clone())) {
                    a.revoked = true;
                }
            }
            OrgRecord::SourceAdded { team, source } => {
                self.sources.insert(source.clone(), team.clone());
            }
            OrgRecord::IngestAdded {
                team,
                name,
                token_sha256,
            } => {
                self.ingest
                    .insert((team.clone(), name.clone()), token_sha256.clone());
            }
            OrgRecord::GrantAdded {
                team,
                from_team,
                agents,
                repos,
                max_pending,
            } => self.grants.push(Grant {
                team: team.clone(),
                from_team: from_team.clone(),
                agents: agents.clone(),
                repos: repos.clone(),
                max_pending: *max_pending,
            }),
            OrgRecord::FormatRaised { to } => self.format = *to,
            OrgRecord::ConnectionSet {
                team,
                name,
                vendor,
                group,
                pool,
                sources,
            } => {
                self.connections.insert(
                    (team.clone(), name.clone()),
                    Connection {
                        vendor: vendor.clone(),
                        group: group.clone(),
                        pool: pool.clone(),
                        sources: sources.clone(),
                    },
                );
            }
            OrgRecord::BudgetSet {
                pool,
                per_hour,
                kept_back,
                wake_cap,
            } => {
                self.budgets.insert(
                    pool.clone(),
                    Budget {
                        per_hour: *per_hour,
                        kept_back: *kept_back,
                        wake_cap: *wake_cap,
                    },
                );
            }
        }
        Ok(applied)
    }
}

/// A source, display name or subject: 1 to [`MAX_LABEL_BYTES`] bytes, no control characters.
fn label_ok(s: &str) -> bool {
    (1..=MAX_LABEL_BYTES).contains(&s.len()) && !s.chars().any(char::is_control)
}
