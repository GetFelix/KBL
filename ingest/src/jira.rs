//! The Jira adapter: issues selected by each rule's JQL filter, mapped to the
//! same shape as any source's, and written back as comments, remote links
//! and workflow transitions. It uses the v2 REST API, whose text fields are
//! plain strings.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use serde_json::{Value, json};

use crate::source::{
    Changes, Closed, Comment, Cursor, Fetched, Issue, IssueRef, Links, PullRequest, Releases,
    Source, SourceError, WriteBack,
};

pub(crate) mod transport;

pub(crate) use crate::github::{Req, Res, Transport};

/// Pages followed in one listing before the cursor stops where they ended.
const MAX_PAGES: usize = 20;
const PAGE: usize = 100;
/// The longest a rate-limit wait is trusted to be.
const MAX_WAIT: Duration = Duration::from_secs(3600);
/// Used when Jira refuses with 429 and names no wait.
const DEFAULT_WAIT: Duration = Duration::from_secs(30);
const FIELDS: &str = "summary,description,status,resolution,labels,priority,components,\
                      issuetype,reporter,assignee,updated,fixVersions";
/// Resolutions that mean the work was done. Any other resolution, such as
/// Won't Do or Duplicate, counts as dropped.
const DONE: &[&str] = &["done", "fixed", "complete", "completed", "resolved"];

/// What the adapter needs of a rule: its name, its project (`site/PROJ`) and
/// its JQL filter.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub(crate) name: String,
    pub(crate) repo: String,
    pub(crate) jql: String,
}

pub(crate) struct Jira<T> {
    site: String,
    api: String,
    http: T,
    own: Vec<String>,
    scopes: Vec<Scope>,
    pr_base: String,
    browse: String,
    limited_until: Mutex<Option<Instant>>,
}

impl<T: Transport> Jira<T> {
    pub(crate) fn new(
        site: &str,
        api: &str,
        http: T,
        own: Vec<String>,
        scopes: Vec<Scope>,
        pr_base: &str,
    ) -> Self {
        Self {
            site: site.to_owned(),
            api: api.trim_end_matches('/').to_owned(),
            http,
            own,
            scopes,
            pr_base: pr_base.trim_end_matches('/').to_owned(),
            browse: api.trim_end_matches('/').to_owned(),
            limited_until: Mutex::new(None),
        }
    }

    /// The address issue links are built on, when it is not the API's.
    pub(crate) fn browse_at(&mut self, base: &str) {
        self.browse = base.trim_end_matches('/').to_owned();
    }

    /// Fails when nothing says which comments are ours. Without it a retried
    /// write-back posts its comment again and our own come back as foreign.
    pub(crate) fn require_own(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.own.is_empty(),
            "Jira did not say who this credential is (GET /myself failed; an OAuth token needs the \
             read:me scope) and jira.own_logins is empty; set own_logins or grant the scope"
        );
        Ok(())
    }

    pub(crate) fn add_own(&mut self, names: Vec<String>) {
        self.own.extend(names);
    }

    /// The names the credential writes as, from Jira itself.
    pub(crate) async fn login(&self) -> Vec<String> {
        let Ok(res) = self.call(Req::get(self.url("/myself", &[]))).await else {
            return Vec::new();
        };
        ["displayName", "accountId"]
            .iter()
            .filter_map(|k| res.body[k].as_str().map(str::to_owned))
            .collect()
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> String {
        let base = format!("{}/rest/api/2{path}", self.api);
        if query.is_empty() {
            return base;
        }
        match reqwest::Url::parse_with_params(&base, query) {
            Ok(u) => u.to_string(),
            Err(_) => base,
        }
    }

    async fn call(&self, req: Req) -> Result<Res, SourceError> {
        if let Some(until) = *self.limited_until.lock().expect("lock") {
            let now = Instant::now();
            if until > now {
                return Err(SourceError::RateLimited {
                    wait: (until - now).min(MAX_WAIT),
                });
            }
        }
        let res = self.http.send(req).await?;
        let waits =
            res.status == 429 || (res.status == 503 && res.headers.contains_key("retry-after"));
        if waits {
            let wait = res
                .headers
                .get("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map_or(DEFAULT_WAIT, Duration::from_secs)
                .min(MAX_WAIT);
            *self.limited_until.lock().expect("lock") = Some(Instant::now() + wait);
            return Err(SourceError::RateLimited { wait });
        }
        if res.status >= 500 {
            return Err(anyhow!("Jira answered {}", res.status).into());
        }
        Ok(res)
    }

    /// The project key of a `site/PROJ` repo.
    fn project<'a>(&self, repo: &'a str) -> Option<&'a str> {
        repo.strip_prefix(self.site.as_str())?.strip_prefix('/')
    }

    /// `PROJ-12` for `site/PROJ#12`, if the id is on this site.
    fn key(&self, id: &IssueRef) -> Result<String, SourceError> {
        let project = self
            .project(&id.repo)
            .ok_or_else(|| anyhow!("{id} is not on the Jira site {}", self.site))?;
        Ok(format!("{project}-{}", id.number))
    }

    fn id_of(&self, key: &str) -> Option<IssueRef> {
        let (project, number) = key.rsplit_once('-')?;
        Some(IssueRef::new(
            &format!("{}/{project}", self.site),
            number.parse().ok()?,
        ))
    }

    /// Every issue a JQL query matches, following the page token. Stops after
    /// `MAX_PAGES`; the listing is ordered oldest update first, so the
    /// cursor then resumes where it ended.
    async fn search(
        &self,
        jql: &str,
        fields: &str,
    ) -> Result<Result<Vec<Value>, u16>, SourceError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        let max = PAGE.to_string();
        for _ in 0..MAX_PAGES {
            let mut q = vec![
                ("jql", jql),
                ("fields", fields),
                ("maxResults", max.as_str()),
            ];
            if let Some(t) = &token {
                q.push(("nextPageToken", t));
            }
            let res = self.call(Req::get(self.url("/search/jql", &q))).await?;
            match res.status {
                200 => {}
                400 | 401 | 403 | 404 => return Ok(Err(res.status)),
                s => return Err(anyhow!("Jira answered {s} to a search").into()),
            }
            out.extend(res.body["issues"].as_array().cloned().unwrap_or_default());
            match res.body["nextPageToken"].as_str() {
                Some(t) if res.body["isLast"] != true => token = Some(t.to_owned()),
                _ => break,
            }
        }
        Ok(Ok(out))
    }

    fn parse(&self, raw: &Value) -> anyhow::Result<Issue> {
        let key = raw["key"].as_str().context("an issue with no key")?;
        let id = self.id_of(key).context("an issue key that is not PROJ-n")?;
        let f = &raw["fields"];
        let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        let names = |v: &Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x["name"].as_str().map(str::to_owned))
                .collect()
        };
        let closed = (f["status"]["statusCategory"]["key"] == "done").then(|| {
            match f["resolution"]["name"].as_str() {
                // Done with no resolution is an issue closed by a workflow
                // that sets none.
                None => Closed::Completed,
                Some(r) if DONE.contains(&r.to_ascii_lowercase().as_str()) => Closed::Completed,
                Some(_) => Closed::Dropped,
            }
        });
        Ok(Issue {
            id,
            title: text(&f["summary"]),
            body: text(&f["description"]),
            closed,
            labels: f["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l.as_str().map(str::to_owned))
                .collect(),
            milestone: names(&f["fixVersions"]).into_iter().next(),
            assignees: f["assignee"]["displayName"]
                .as_str()
                .map(str::to_owned)
                .into_iter()
                .collect(),
            kind: f["issuetype"]["name"].as_str().map(str::to_owned),
            author: text(&f["reporter"]["displayName"]),
            url: format!("{}/browse/{key}", self.browse),
            updated_at: text(&f["updated"]),
            priority: f["priority"]["name"].as_str().map(str::to_owned),
            components: names(&f["components"]),
            scopes: Vec::new(),
        })
    }

    fn parse_comment(&self, id: &IssueRef, raw: &Value) -> Option<Comment> {
        Some(Comment {
            id: raw["id"].as_str()?.parse().ok()?,
            issue: id.clone(),
            author: raw["author"]["displayName"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            body: raw["body"].as_str().unwrap_or_default().to_owned(),
            updated_at: raw["updated"].as_str().unwrap_or_default().to_owned(),
        })
    }
}

/// `2026-11-10T12:00:05.000+0000` as JQL writes a date: `2026/11/10 12:00`.
fn jql_date(updated: &str) -> Option<String> {
    let head = updated.get(..16)?;
    let b = head.as_bytes();
    let shaped = b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':';
    (shaped && head.bytes().filter(u8::is_ascii_digit).count() == 12)
        .then(|| head.replace('-', "/").replace('T', " "))
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl<T: Transport> Source for Jira<T> {
    fn name(&self) -> &'static str {
        "jira"
    }

    fn reads_pull_requests(&self) -> bool {
        false
    }

    fn own_logins(&self) -> Vec<String> {
        self.own.clone()
    }

    async fn changed(
        &self,
        repos: &[String],
        cursors: &BTreeMap<String, Cursor>,
    ) -> Result<Changes, SourceError> {
        let mut changes = Changes::default();
        for repo in repos {
            let Some(project) = self.project(repo) else {
                continue;
            };
            let cursor = cursors.get(repo).cloned().unwrap_or_default();
            // The first poll reads open issues only; later ones read every
            // change, which includes a close.
            let base = match cursor.issues_since.as_deref().and_then(jql_date) {
                Some(d) => format!("project = {} AND updated >= {}", quote(project), quote(&d)),
                None => format!("project = {} AND statusCategory != Done", quote(project)),
            };
            let listed = match self
                .search(&format!("{base} ORDER BY updated ASC"), FIELDS)
                .await?
            {
                Ok(l) => l,
                Err(status) => {
                    changes.denied.insert(repo.clone(), status);
                    continue;
                }
            };
            // Which rules' filters pick each issue is asked of Jira, which
            // alone can evaluate JQL.
            let mut picked: Vec<(&Scope, BTreeSet<String>)> = Vec::new();
            let mut refused = None;
            for s in self.scopes.iter().filter(|s| &s.repo == repo) {
                let jql = format!("{base} AND ({}) ORDER BY updated ASC", s.jql);
                let keys = match self.search(&jql, "summary").await? {
                    Ok(l) => l
                        .iter()
                        .filter_map(|v| v["key"].as_str().map(str::to_owned))
                        .collect(),
                    // A failed search says nothing about scope, so the project
                    // stalls with its scopes and cursor as they were.
                    Err(status) => {
                        eprintln!(
                            "kbl-ingest: Jira answered {status} to the filter of rule {}",
                            s.name
                        );
                        refused = Some(status);
                        break;
                    }
                };
                picked.push((s, keys));
            }
            if let Some(status) = refused {
                changes.denied.insert(repo.clone(), status);
                continue;
            }
            let mut newest = cursor.issues_since.clone();
            for raw in &listed {
                let Ok(mut issue) = self.parse(raw) else {
                    eprintln!("kbl-ingest: skipping a Jira issue that does not parse");
                    continue;
                };
                let key = raw["key"].as_str().unwrap_or_default();
                issue.scopes = picked
                    .iter()
                    .filter(|(_, keys)| keys.contains(key))
                    .map(|(s, _)| s.name.clone())
                    .collect();
                if newest.as_ref().is_none_or(|n| issue.updated_at > *n) {
                    newest = Some(issue.updated_at.clone());
                }
                if !issue.scopes.is_empty() {
                    changes
                        .comments
                        .extend(self.issue_comments(&issue.id).await?);
                }
                changes.issues.push(issue);
            }
            changes.cursors.insert(
                repo.clone(),
                Cursor {
                    issues_since: newest,
                    ..Cursor::default()
                },
            );
        }
        Ok(changes)
    }

    /// Jira holds no pull requests.
    async fn pull_request(&self, _id: &IssueRef) -> Result<Option<PullRequest>, SourceError> {
        Ok(None)
    }

    async fn fetch(&self, id: &IssueRef) -> Result<Fetched, SourceError> {
        let key = self.key(id)?;
        let res = self
            .call(Req::get(
                self.url(&format!("/issue/{key}"), &[("fields", FIELDS)]),
            ))
            .await?;
        match res.status {
            200 => {}
            404 | 410 => return Ok(Fetched::Gone),
            s => return Err(anyhow!("Jira answered {s} for {id}").into()),
        }
        let mut issue = self.parse(&res.body)?;
        // Jira answers an old key with the issue under its new one.
        if issue.id != *id {
            return Ok(Fetched::Moved(issue.id));
        }
        for s in self.scopes.iter().filter(|s| s.repo == id.repo) {
            let jql = format!("key = {} AND ({})", quote(&key), s.jql);
            if matches!(self.search(&jql, "summary").await?, Ok(l) if !l.is_empty()) {
                issue.scopes.push(s.name.clone());
            }
        }
        Ok(Fetched::Issue(Box::new(issue)))
    }

    async fn issue_comments(&self, id: &IssueRef) -> Result<Vec<Comment>, SourceError> {
        let key = self.key(id)?;
        let mut out = Vec::new();
        for page in 0..MAX_PAGES {
            let start = (page * PAGE).to_string();
            let max = PAGE.to_string();
            let url = self.url(
                &format!("/issue/{key}/comment"),
                &[
                    ("startAt", &start),
                    ("maxResults", &max),
                    ("orderBy", "created"),
                ],
            );
            let res = self.call(Req::get(url)).await?;
            match res.status {
                200 => {}
                404 | 410 => return Ok(out),
                s => return Err(anyhow!("Jira answered {s} for the comments of {id}").into()),
            }
            let items = res.body["comments"].as_array().cloned().unwrap_or_default();
            out.extend(items.iter().filter_map(|c| self.parse_comment(id, c)));
            if items.len() < PAGE {
                break;
            }
        }
        Ok(out)
    }

    async fn links(&self, id: &IssueRef) -> Result<Links, SourceError> {
        let key = self.key(id)?;
        let res = self
            .call(Req::get(self.url(
                &format!("/issue/{key}"),
                &[("fields", "issuelinks,subtasks")],
            )))
            .await?;
        if res.status != 200 {
            // A missing or unreadable issue says nothing about its links.
            return Ok(Links::default());
        }
        let f = &res.body["fields"];
        let keys = |it: &Value, side: &str| it[side]["key"].as_str().and_then(|k| self.id_of(k));
        let blocked_by = f["issuelinks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| {
                l["type"]["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case("blocks"))
            })
            // The inward issue of a Blocks link is the one that blocks this.
            .filter_map(|l| keys(l, "inwardIssue"))
            .collect();
        let sub_issues = f["subtasks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["key"].as_str().and_then(|k| self.id_of(k)))
            .collect();
        Ok(Links {
            blocked_by,
            sub_issues,
            complete: true,
        })
    }

    async fn write_back(&self, what: &WriteBack) -> Result<(), SourceError> {
        match what {
            WriteBack::DraftPullRequest { .. } => {
                Err(anyhow!("Jira holds no pull requests to draft").into())
            }
            WriteBack::Comment { issue, body } => {
                let key = self.key(issue)?;
                // A retry after a later step failed must not say it twice.
                if self
                    .issue_comments(issue)
                    .await?
                    .iter()
                    .any(|c| c.body == *body && self.own.contains(&c.author))
                {
                    return Ok(());
                }
                self.post(&format!("/issue/{key}/comment"), json!({ "body": body }))
                    .await
            }
            WriteBack::LinkPullRequest { issue, pr } => {
                let key = self.key(issue)?;
                // The global id makes Jira update this link rather than add another.
                let body = json!({
                    "globalId": format!("kbl-pr:{pr}"),
                    "object": {
                        "url": format!("{}/{}/pull/{}", self.pr_base, pr.repo, pr.number),
                        "title": format!("Pull request {pr}"),
                    },
                });
                self.post(&format!("/issue/{key}/remotelink"), body).await
            }
            WriteBack::Transition { issue, name } => {
                let key = self.key(issue)?;
                let path = format!("/issue/{key}/transitions");
                let res = self.call(Req::get(self.url(&path, &[]))).await?;
                if res.status != 200 {
                    return Err(anyhow!(
                        "Jira answered {} for the transitions of {issue}",
                        res.status
                    )
                    .into());
                }
                let found = res.body["transitions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|t| {
                        [&t["name"], &t["to"]["name"]]
                            .iter()
                            .any(|n| n.as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))
                    });
                // The workflow does not offer it from here: nothing to do.
                let Some(id) = found.and_then(|t| t["id"].as_str()) else {
                    eprintln!("kbl-ingest: {issue} has no transition {name:?} to take");
                    return Ok(());
                };
                self.post(&path, json!({ "transition": { "id": id } }))
                    .await
            }
        }
    }

    /// Jira has no releases of ours to read; an empty, whole listing says so.
    async fn releases(&self, _repo: &str, _since_ms: u64) -> Result<Releases, SourceError> {
        Ok(Releases {
            whole: true,
            ..Releases::default()
        })
    }

    async fn contains(&self, _repo: &str, _tag: &str, _sha: &str) -> Result<bool, SourceError> {
        Ok(false)
    }

    async fn open_issue(
        &self,
        _repo: &str,
        _title: &str,
        _body: &str,
    ) -> Result<IssueRef, SourceError> {
        Err(anyhow!("the Jira source does not open issues").into())
    }
}

impl<T: Transport> Jira<T> {
    async fn post(&self, path: &str, body: Value) -> Result<(), SourceError> {
        let res = self.call(Req::post(self.url(path, &[]), body)).await?;
        if (200..300).contains(&res.status) {
            Ok(())
        } else {
            Err(anyhow!("Jira answered {} to a write", res.status).into())
        }
    }
}

#[cfg(test)]
mod tests;
