//! What `org.events` holds: which teams exist, who is in them, and the hash of
//! every token. See [Teams in design.md](../../docs/design.md#teams).
//!
//! Like `work.events`, the log is the record and a fold ([`crate::org_fold`])
//! turns it into tables. The tracker is its only writer. No token is ever on
//! it, only [`TokenHash`], the SHA-256 of a 32-byte random token.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::codec::{Codec, DecodeError};
use crate::names::{InvalidName, Key, is_valid_key};

/// The stream every change to the org goes on.
pub const ORG_EVENTS: &str = "org.events";

/// The namespace that holds [`ORG_EVENTS`]. No team may take this name.
pub const ORG_NAMESPACE: &str = "kbl";

/// The cache that holds the org writer's lease.
pub const ORG_TRACKER_CACHE: &str = "org.tracker";

/// The format of every record before one is added that an older fold would
/// read differently. A record in this format is written without `v`.
pub const ORG_FORMAT_V1: u32 = 1;

/// The highest format this build reads and writes.
pub const ORG_FORMAT: u32 = ORG_FORMAT_V1;

/// Most agents (live and revoked) one team may have. A revoked name stays
/// taken, so the count only grows.
pub const MAX_AGENTS_PER_TEAM: usize = 256;

/// Most ingest tokens one team may have.
pub const MAX_INGEST_PER_TEAM: usize = 32;

/// Most grants one receiving team may hold.
pub const MAX_GRANTS_PER_TEAM: usize = 256;

/// Longest source, repo or display name, in bytes.
pub const MAX_LABEL_BYTES: usize = 256;

/// Most repos one member's role, or one grant, may list.
pub const MAX_ROLE_REPOS: usize = 64;

/// Most agents one grant may list.
pub const MAX_GRANT_AGENTS: usize = 64;

/// A team's name, which is also the name of its Felix namespace. It follows
/// the key rule and is not [`ORG_NAMESPACE`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TeamName(String);

impl TeamName {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidName> {
        let s = s.into();
        if is_valid_key(&s) && s != ORG_NAMESPACE {
            Ok(Self(s))
        } else {
            Err(InvalidName(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TeamName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for TeamName {
    type Err = InvalidName;
    fn from_str(s: &str) -> Result<Self, InvalidName> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for TeamName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// The SHA-256 of a token, as 64 lowercase hex characters.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TokenHash(String);

impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TokenHash({}..)", &self.0[..8])
    }
}

impl TokenHash {
    /// The hash of `token`.
    pub fn of(token: &str) -> Self {
        let digest = Sha256::digest(token.as_bytes());
        let mut hex = String::with_capacity(64);
        for b in digest {
            hex.push_str(&format!("{b:02x}"));
        }
        Self(hex)
    }

    /// Parses the hex form a record carries.
    pub fn parse(s: &str) -> Result<Self, InvalidTokenHash> {
        let ok = s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(InvalidTokenHash)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `token` hashes to this. The comparison does not stop at the
    /// first differing byte.
    pub fn matches(&self, token: &str) -> bool {
        Self::of(token).ct_eq(self)
    }

    /// Equality that takes the same time wherever the hashes differ.
    pub fn ct_eq(&self, other: &Self) -> bool {
        self.0
            .bytes()
            .zip(other.0.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

/// A string that is not a token hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTokenHash;

impl fmt::Display for InvalidTokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a token hash is 64 lowercase hex characters")
    }
}

impl std::error::Error for InvalidTokenHash {}

impl<'de> Deserialize<'de> for TokenHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// What a member may do in a team, lowest first. Each role adds to the one
/// before it. `OrgAdmin` is the one role outside any team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Reviewer,
    Triager,
    Admin,
    OrgAdmin,
}

/// What a record says happened. `kind` is the variant name in snake case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrgRecord {
    /// A team and its namespace. `adopted` marks the namespace a single-team
    /// instance already used, whose log was not touched.
    TeamCreated {
        team: TeamName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        adopted: bool,
    },
    /// Someone who signs in. A person belongs to the org, not to a team.
    PersonAdded {
        person: Key,
        token_sha256: TokenHash,
        /// The OIDC subject the person signs in with, when they use the dashboard.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
    },
    /// A person's role in a team, or `org_admin` with no team. A later one for
    /// the same person and team replaces the earlier. `repos` narrows the role
    /// to some of the team's sources; empty is all of them.
    RoleGranted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        team: Option<TeamName>,
        person: Key,
        role: Role,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        repos: Vec<String>,
    },
    /// A person leaves a team, or loses `org_admin`.
    RoleRevoked {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        team: Option<TeamName>,
        person: Key,
    },
    /// An agent and its token, bound to one team.
    AgentAdded {
        team: TeamName,
        agent: Key,
        token_sha256: TokenHash,
    },
    /// The agent's token stops working and its name is never reused.
    AgentRevoked { team: TeamName, agent: Key },
    /// An upstream repo or project a team owns, such as `github:GetFelix/felix`.
    SourceAdded { team: TeamName, source: String },
    /// An ingest token bound to one team.
    IngestAdded {
        team: TeamName,
        name: Key,
        token_sha256: TokenHash,
    },
    /// Lets `from_team`'s agents file prerequisites into `team`'s Triage.
    GrantAdded {
        team: TeamName,
        from_team: TeamName,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agents: Vec<Key>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        repos: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_pending: Option<u32>,
    },
}

impl OrgRecord {
    /// The `kind` field this record is written with.
    pub fn kind(&self) -> &'static str {
        match self {
            OrgRecord::TeamCreated { .. } => "team_created",
            OrgRecord::PersonAdded { .. } => "person_added",
            OrgRecord::RoleGranted { .. } => "role_granted",
            OrgRecord::RoleRevoked { .. } => "role_revoked",
            OrgRecord::AgentAdded { .. } => "agent_added",
            OrgRecord::AgentRevoked { .. } => "agent_revoked",
            OrgRecord::SourceAdded { .. } => "source_added",
            OrgRecord::IngestAdded { .. } => "ingest_added",
            OrgRecord::GrantAdded { .. } => "grant_added",
        }
    }

    /// The lowest format whose readers fold this record as this build does.
    pub fn format(&self) -> u32 {
        // No wildcard: a new kind does not compile until it names its format.
        match self {
            OrgRecord::TeamCreated { .. }
            | OrgRecord::PersonAdded { .. }
            | OrgRecord::RoleGranted { .. }
            | OrgRecord::RoleRevoked { .. }
            | OrgRecord::AgentAdded { .. }
            | OrgRecord::AgentRevoked { .. }
            | OrgRecord::SourceAdded { .. }
            | OrgRecord::IngestAdded { .. }
            | OrgRecord::GrantAdded { .. } => ORG_FORMAT_V1,
        }
    }
}

/// A record as it sits on `org.events`: its format, when, and who asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgLogged {
    /// Left out at [`ORG_FORMAT_V1`]. Decoding fails above [`ORG_FORMAT`]: use
    /// [`read`] to tell that from a malformed record.
    #[serde(
        default = "format_v1",
        skip_serializing_if = "is_format_v1",
        deserialize_with = "readable_format"
    )]
    pub v: u32,
    pub at_ms: u64,
    /// The person who asked, or the tracker's own name when it acted alone.
    /// The tracker sets it from the token, never from the payload.
    pub by: Key,
    #[serde(flatten)]
    pub record: OrgRecord,
}

impl OrgLogged {
    /// A record to append, stamped with the lowest format that reads it.
    pub fn new(at_ms: u64, by: Key, record: OrgRecord) -> Self {
        Self {
            v: record.format(),
            at_ms,
            by,
            record,
        }
    }
}

/// Why a payload on `org.events` was not read.
#[derive(Debug)]
pub enum OrgReadError {
    /// The record is of a format above [`ORG_FORMAT`]. The reader must stop:
    /// skipping it would fold a state its writer did not mean.
    Newer {
        v: u32,
    },
    Malformed(DecodeError),
}

impl fmt::Display for OrgReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrgReadError::Newer { v } => write!(
                f,
                "the org record has format {v} and this build reads up to {ORG_FORMAT}: upgrade this reader"
            ),
            OrgReadError::Malformed(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for OrgReadError {}

/// Decodes a payload of `org.events`, and says when it failed because the
/// record is newer than this build.
pub fn read(payload: &[u8]) -> Result<OrgLogged, OrgReadError> {
    #[derive(Deserialize)]
    struct Envelope {
        v: u64,
    }
    OrgLogged::decode(payload).map_err(|e| match serde_json::from_slice::<Envelope>(payload) {
        Ok(Envelope { v }) if v > u64::from(ORG_FORMAT) => OrgReadError::Newer {
            v: u32::try_from(v).unwrap_or(u32::MAX),
        },
        _ => OrgReadError::Malformed(e),
    })
}

fn format_v1() -> u32 {
    ORG_FORMAT_V1
}

fn is_format_v1(v: &u32) -> bool {
    *v == ORG_FORMAT_V1
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn readable_format<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(d)?;
    if (ORG_FORMAT_V1..=ORG_FORMAT).contains(&v) {
        Ok(v)
    } else {
        Err(serde::de::Error::custom(format_args!(
            "org record format {v} is not one this build reads ({ORG_FORMAT_V1} to {ORG_FORMAT})"
        )))
    }
}
