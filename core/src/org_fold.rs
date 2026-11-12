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
    MAX_AGENTS_PER_TEAM, MAX_GRANT_AGENTS, MAX_GRANTS_PER_TEAM, MAX_INGEST_PER_TEAM,
    MAX_LABEL_BYTES, MAX_ROLE_REPOS, OrgLogged, OrgRecord, Role, TeamName, TokenHash,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgState {
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
    pub fn source_owner(&self, source: &str) -> Option<&TeamName> {
        self.sources.get(source)
    }

    pub fn grants(&self) -> &[Grant] {
        &self.grants
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
            .filter(|((t, _), m)| t == team && m.role == Role::Admin)
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
        match &logged.record {
            OrgRecord::TeamCreated { display_name, .. }
                if display_name.as_deref().is_some_and(|n| !label_ok(n)) =>
            {
                Err(OrgRefusal::BadLabel)
            }
            OrgRecord::TeamCreated { team, .. } => {
                Ok(if self.has_team(team) { Unchanged } else { Changed })
            }
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
                    (None, Role::OrgAdmin) if repos.is_empty() => Ok(if self.is_org_admin(person) {
                        Unchanged
                    } else {
                        Changed
                    }),
                    (Some(team), role) if *role != Role::OrgAdmin => {
                        self.need_team(team)?;
                        if !self.owns_all(team, repos) {
                            return Err(OrgRefusal::UnknownSource);
                        }
                        match self.member(team, person) {
                            Some(m) if m.role == *role && m.repos == *repos => Ok(Unchanged),
                            Some(m)
                                if m.role == Role::Admin
                                    && *role != Role::Admin
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
                    Some(m) if m.role == Role::Admin && self.admins_in(team) == 1 => {
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
                if self.agents.keys().filter(|(t, _)| t == team).count() >= MAX_AGENTS_PER_TEAM {
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
        }
        Ok(applied)
    }
}

/// A source, display name or subject: 1 to [`MAX_LABEL_BYTES`] bytes, no control characters.
fn label_ok(s: &str) -> bool {
    (1..=MAX_LABEL_BYTES).contains(&s.len()) && !s.chars().any(char::is_control)
}
