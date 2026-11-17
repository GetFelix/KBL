//! The Azure DevOps adapter: work items of the configured projects, picked by
//! tag or by a saved query, mapped to the same shape as any source's, and
//! written back as comments, hyperlinks and state changes. It reads and
//! writes through the REST API at version 7.1.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use serde_json::{Value, json};

use crate::config::Rule;
use crate::source::{
    Changes, Closed, Comment, Cursor, Fetched, Issue, IssueRef, Links, PullRequest, Releases,
    Source, SourceError, WriteBack,
};

pub(crate) mod transport;

pub(crate) use crate::github::{Req, Res, Transport};

/// Work items a listing reads in one poll. The listing is oldest change
/// first, so the cursor resumes where it ended.
const LIST_CAP: usize = 2000;
/// Work items one batch read asks for; the service allows 200.
const BATCH: usize = 200;
const COMMENT_PAGES: usize = 20;
const MAX_WAIT: Duration = Duration::from_secs(3600);
const DEFAULT_WAIT: Duration = Duration::from_secs(30);
const API: &str = "7.1";
/// The comments API is still a preview.
const COMMENTS_API: &str = "7.1-preview.4";
/// States that mean the work was done, and states that mean it was dropped.
/// Any other state is open.
const DONE: &[&str] = &["done", "closed", "completed"];
const DROPPED: &[&str] = &["removed", "cut"];

/// What the adapter needs of a rule.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub(crate) rule: Rule,
}

pub(crate) struct Ado<T> {
    org: String,
    api: String,
    http: T,
    own: Vec<String>,
    scopes: Vec<Scope>,
    /// Rule-repo project name to the project's real name.
    projects: BTreeMap<String, String>,
    pr_base: String,
    /// Whether any rule writes back. Without one the adapter refuses every
    /// write, so a read-only credential is all that is ever needed.
    writes: bool,
    limited_until: Mutex<Option<Instant>>,
}

impl<T: Transport> Ado<T> {
    pub(crate) fn new(
        org: &str,
        api: &str,
        http: T,
        own: Vec<String>,
        scopes: Vec<Scope>,
        names: &BTreeMap<String, String>,
        pr_base: &str,
    ) -> Self {
        let mut projects = BTreeMap::new();
        for s in &scopes {
            if let Some(slug) = s.rule.repo.strip_prefix(&format!("{org}/")) {
                let real = names.get(slug).map_or(slug, String::as_str);
                projects.insert(slug.to_owned(), real.to_owned());
            }
        }
        let writes = scopes.iter().any(|s| {
            let r = &s.rule;
            r.comment
                || r.link_pr
                || r.transition_on_pr.is_some()
                || r.transition_on_merge.is_some()
        });
        Self {
            org: org.to_owned(),
            api: api.trim_end_matches('/').to_owned(),
            http,
            own,
            scopes,
            projects,
            pr_base: pr_base.trim_end_matches('/').to_owned(),
            writes,
            limited_until: Mutex::new(None),
        }
    }

    /// Fails when nothing says which comments are ours. Without it a retried
    /// write-back posts its comment again and our own come back as foreign.
    pub(crate) fn require_own(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.own.is_empty(),
            "Azure DevOps did not say who this credential is (GET connectionData failed) and \
             ado.own_logins is empty; set own_logins"
        );
        Ok(())
    }

    pub(crate) fn add_own(&mut self, names: Vec<String>) {
        self.own.extend(names);
    }

    /// The names the credential writes as, from the service itself.
    pub(crate) async fn login(&self) -> Vec<String> {
        let Ok(url) = self.url(None, "/connectionData", &[], "7.1-preview") else {
            return Vec::new();
        };
        let Ok(res) = self.call(Req::get(url)).await else {
            return Vec::new();
        };
        let user = &res.body["authenticatedUser"];
        ["providerDisplayName", "customDisplayName"]
            .iter()
            .filter_map(|k| user[k].as_str().map(str::to_owned))
            .collect()
    }

    /// `{api}/{org}[/{project}]/_apis{path}`, each part encoded as a path
    /// segment so a name can never add a segment or a query.
    fn url(
        &self,
        project: Option<&str>,
        path: &str,
        query: &[(&str, &str)],
        version: &str,
    ) -> anyhow::Result<String> {
        let mut url = reqwest::Url::parse(&self.api).context("the Azure DevOps api address")?;
        {
            let mut seg = url
                .path_segments_mut()
                .map_err(|()| anyhow!("the Azure DevOps api address cannot take a path"))?;
            seg.pop_if_empty().push(&self.org);
            if let Some(p) = project {
                seg.push(p);
            }
            seg.push("_apis");
            for part in path.split('/').filter(|p| !p.is_empty()) {
                seg.push(part);
            }
        }
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("api-version", version);
            for (k, v) in query {
                q.append_pair(k, v);
            }
        }
        Ok(url.to_string())
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
        let mut res = self.http.send(req).await?;
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
        // A bad personal access token is not a 401: the service answers 203
        // with its sign-in page, or redirects to it.
        if res.status == 203 || (300..400).contains(&res.status) {
            res.status = 401;
        }
        if res.status >= 500 {
            return Err(anyhow!("Azure DevOps answered {}", res.status).into());
        }
        Ok(res)
    }

    /// The real name of a rule-repo project.
    fn project_name(&self, repo: &str) -> Option<&str> {
        let slug = repo.strip_prefix(self.org.as_str())?.strip_prefix('/')?;
        self.projects.get(slug).map(String::as_str)
    }

    /// The rule-repo name of a project, found by its real name.
    fn slug_of(&self, project: &str) -> Option<&str> {
        self.projects
            .iter()
            .find(|(_, real)| real.eq_ignore_ascii_case(project))
            .map(|(slug, _)| slug.as_str())
    }

    fn id_of(&self, slug: &str, number: u64) -> IssueRef {
        IssueRef::new(&format!("{}/{slug}", self.org), number)
    }

    /// The real project name of an id on this organization, or an error for
    /// an id outside the configured projects: nothing is read or written there.
    fn project_of(&self, id: &IssueRef) -> Result<&str, SourceError> {
        self.project_name(&id.repo)
            .ok_or_else(|| anyhow!("{id} is not in a project this worker is configured for").into())
    }

    /// The ids a WIQL query returns, oldest change first.
    async fn wiql(&self, project: &str, query: &str) -> Result<Result<Vec<u64>, u16>, SourceError> {
        let top = LIST_CAP.to_string();
        let url = self.url(
            Some(project),
            "/wit/wiql",
            &[("timePrecision", "true"), ("$top", &top)],
            API,
        )?;
        let res = self.call(Req::post(url, json!({ "query": query }))).await?;
        match res.status {
            200 => Ok(Ok(work_item_ids(&res.body))),
            400 | 401 | 403 | 404 => Ok(Err(res.status)),
            s => Err(anyhow!("Azure DevOps answered {s} to a query").into()),
        }
    }

    /// The ids a saved query returns. A path is resolved to its id first.
    async fn saved(
        &self,
        project: &str,
        query: &str,
    ) -> Result<Result<Vec<u64>, u16>, SourceError> {
        let id = if is_guid(query) {
            query.to_owned()
        } else {
            let url = self.url(Some(project), &format!("/wit/queries/{query}"), &[], API)?;
            let res = self.call(Req::get(url)).await?;
            match (res.status, res.body["id"].as_str()) {
                (200, Some(id)) if is_guid(id) => id.to_owned(),
                (200, _) => return Ok(Err(404)),
                (s @ (400 | 401 | 403 | 404), _) => return Ok(Err(s)),
                (s, _) => return Err(anyhow!("Azure DevOps answered {s} for a query").into()),
            }
        };
        let url = self.url(
            Some(project),
            &format!("/wit/wiql/{id}"),
            &[("timePrecision", "true")],
            API,
        )?;
        let res = self.call(Req::get(url)).await?;
        match res.status {
            200 => Ok(Ok(work_item_ids(&res.body))),
            400 | 401 | 403 | 404 => Ok(Err(res.status)),
            s => Err(anyhow!("Azure DevOps answered {s} to a saved query").into()),
        }
    }

    /// The work items with these ids, with their relations. An id the
    /// credential cannot read is left out.
    async fn items(&self, ids: &[u64]) -> Result<Result<Vec<Value>, u16>, SourceError> {
        let mut out = Vec::new();
        for chunk in ids.chunks(BATCH) {
            let list = chunk
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let url = self.url(
                None,
                "/wit/workitems",
                &[
                    ("ids", &list),
                    ("$expand", "relations"),
                    ("errorPolicy", "omit"),
                ],
                API,
            )?;
            let res = self.call(Req::get(url)).await?;
            match res.status {
                200 => out.extend(
                    res.body["value"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|v| v.is_object())
                        .cloned(),
                ),
                400 | 401 | 403 | 404 => return Ok(Err(res.status)),
                s => return Err(anyhow!("Azure DevOps answered {s} to a read").into()),
            }
        }
        Ok(Ok(out))
    }

    fn parse(&self, raw: &Value) -> Option<Issue> {
        let number = raw["id"].as_u64()?;
        let f = &raw["fields"];
        let project = f["System.TeamProject"].as_str()?;
        let slug = self.slug_of(project)?;
        let text = |k: &str| f[k].as_str().unwrap_or_default().to_owned();
        let state = f["System.State"].as_str().unwrap_or_default();
        let lower = state.to_ascii_lowercase();
        let closed = if DONE.contains(&lower.as_str()) {
            Some(Closed::Completed)
        } else if DROPPED.contains(&lower.as_str()) {
            Some(Closed::Dropped)
        } else {
            None
        };
        let priority = f["Microsoft.VSTS.Common.Priority"]
            .as_u64()
            .map(|p| p.to_string());
        let area = f["System.AreaPath"].as_str().map(str::to_owned);
        Some(Issue {
            id: self.id_of(slug, number),
            title: text("System.Title"),
            body: html_text(&text("System.Description")),
            closed,
            labels: f["System.Tags"]
                .as_str()
                .unwrap_or_default()
                .split(';')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect(),
            milestone: f["System.IterationPath"].as_str().map(str::to_owned),
            assignees: identity(&f["System.AssignedTo"]).into_iter().collect(),
            kind: f["System.WorkItemType"].as_str().map(str::to_owned),
            author: identity(&f["System.CreatedBy"]).unwrap_or_default(),
            url: format!(
                "{}/{}/{}/_workitems/edit/{number}",
                self.api,
                enc(&self.org),
                enc(project)
            ),
            updated_at: text("System.ChangedDate"),
            priority,
            components: area.into_iter().collect(),
            scopes: Vec::new(),
        })
    }

    fn parse_comment(&self, id: &IssueRef, raw: &Value) -> Option<Comment> {
        Some(Comment {
            id: raw["id"].as_u64()?,
            issue: id.clone(),
            author: identity(&raw["createdBy"]).unwrap_or_default(),
            body: html_text(raw["text"].as_str().unwrap_or_default()),
            updated_at: raw["modifiedDate"]
                .as_str()
                .or_else(|| raw["createdDate"].as_str())
                .unwrap_or_default()
                .to_owned(),
        })
    }

    /// The ids each of the repo's saved-query rules picks, by rule name. A
    /// refused query is the status it answered.
    async fn picked(
        &self,
        repo: &str,
        project: &str,
    ) -> Result<Result<Vec<(String, BTreeSet<u64>)>, u16>, SourceError> {
        let mut out = Vec::new();
        for s in self.scopes.iter().filter(|s| s.rule.repo == repo) {
            let Some(q) = &s.rule.ado_query else {
                continue;
            };
            match self.saved(project, q).await? {
                Ok(ids) => out.push((s.rule.name.clone(), ids.into_iter().collect())),
                Err(status) => {
                    eprintln!(
                        "kbl-ingest: Azure DevOps answered {status} to the query of rule {}",
                        s.rule.name
                    );
                    return Ok(Err(status));
                }
            }
        }
        Ok(Ok(out))
    }

    async fn post_write(&self, req: Req) -> Result<(), SourceError> {
        let res = self.call(req).await?;
        if (200..300).contains(&res.status) {
            Ok(())
        } else {
            Err(anyhow!("Azure DevOps answered {} to a write", res.status).into())
        }
    }

    async fn item_url(&self, id: &IssueRef, project: &str) -> Result<String, SourceError> {
        Ok(self.url(
            Some(project),
            &format!("/wit/workitems/{}", id.number),
            &[],
            API,
        )?)
    }

    async fn read_item(&self, id: &IssueRef) -> Result<Option<Value>, SourceError> {
        let url = self.url(
            None,
            &format!("/wit/workitems/{}", id.number),
            &[("$expand", "relations")],
            API,
        )?;
        let res = self.call(Req::get(url)).await?;
        match res.status {
            200 => Ok(Some(res.body)),
            404 | 410 => Ok(None),
            s => Err(anyhow!("Azure DevOps answered {s} for {id}").into()),
        }
    }
}

/// Ids from a query answer: a flat query lists `workItems`, a link query
/// `workItemRelations` whose targets are the items.
fn work_item_ids(body: &Value) -> Vec<u64> {
    let flat = body["workItems"].as_array().into_iter().flatten();
    let linked = body["workItemRelations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| &r["target"]);
    flat.chain(linked)
        .filter_map(|w| w["id"].as_u64())
        .collect()
}

/// A person or app named by an identity field: an object with a display
/// name, or the older `Name <login>` string.
fn identity(v: &Value) -> Option<String> {
    match v {
        Value::Object(_) => v["displayName"].as_str().map(str::to_owned),
        Value::String(s) => Some(s.split(" <").next().unwrap_or(s).trim().to_owned()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

fn is_guid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// `2026-11-10T12:00:05.123Z` as a WIQL date literal, once its shape is
/// checked: a cursor is saved state, and WIQL has no way to bind a value.
fn wiql_date(updated: &str) -> Option<String> {
    let head = updated.get(..19)?;
    let b = head.as_bytes();
    let shaped = b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && b[16] == b':';
    (shaped && head.bytes().filter(u8::is_ascii_digit).count() == 14).then(|| format!("{head}Z"))
}

/// A string literal in WIQL: single quotes, doubled inside.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn enc(s: &str) -> String {
    reqwest::Url::parse("http://h/")
        .map(|mut u| {
            if let Ok(mut seg) = u.path_segments_mut() {
                seg.clear().push(s);
            }
            u.path().trim_start_matches('/').to_owned()
        })
        .unwrap_or_default()
}

/// Plain text from the HTML Azure DevOps keeps in descriptions and comments.
fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut tag = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match (in_tag, c) {
            (false, '<') => {
                in_tag = true;
                tag.clear();
            }
            (true, '>') => {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if matches!(
                    name.as_str(),
                    "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3"
                ) && !out.ends_with('\n')
                {
                    out.push('\n');
                }
            }
            (true, c) => tag.push(c),
            (false, c) => out.push(c),
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl<T: Transport> Source for Ado<T> {
    fn name(&self) -> &'static str {
        "ado"
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
            let Some(project) = self.project_name(repo) else {
                continue;
            };
            let cursor = cursors.get(repo).cloned().unwrap_or_default();
            // The first poll reads open items only; later ones read every
            // change, which includes a close.
            let when = match cursor.issues_since.as_deref().and_then(wiql_date) {
                Some(d) => format!("[System.ChangedDate] >= {}", quote(&d)),
                None => {
                    let closed: Vec<String> = DONE
                        .iter()
                        .chain(DROPPED)
                        .map(|s| quote(&capital(s)))
                        .collect();
                    format!("[System.State] NOT IN ({})", closed.join(", "))
                }
            };
            let wiql = format!(
                "SELECT [System.Id] FROM WorkItems WHERE [System.TeamProject] = {} AND {when} \
                 ORDER BY [System.ChangedDate] ASC",
                quote(project)
            );
            let ids = match self.wiql(project, &wiql).await? {
                Ok(l) => l,
                Err(status) => {
                    changes.denied.insert(repo.clone(), status);
                    continue;
                }
            };
            let listed = match self.items(&ids).await? {
                Ok(l) => l,
                Err(status) => {
                    changes.denied.insert(repo.clone(), status);
                    continue;
                }
            };
            // A failed query says nothing about scope, so the project stalls
            // with its scopes and cursor as they were.
            let picked = match self.picked(repo, project).await? {
                Ok(p) => p,
                Err(status) => {
                    changes.denied.insert(repo.clone(), status);
                    continue;
                }
            };
            let mut issues: Vec<Issue> = listed
                .iter()
                .filter_map(|raw| {
                    let issue = self.parse(raw);
                    if issue.is_none() {
                        eprintln!("kbl-ingest: skipping an Azure DevOps item that does not parse");
                    }
                    issue.filter(|i| i.id.repo == *repo)
                })
                .collect();
            issues.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
            let mut newest = cursor.issues_since.clone();
            for mut issue in issues {
                issue.scopes = picked
                    .iter()
                    .filter(|(_, ids)| ids.contains(&issue.id.number))
                    .map(|(name, _)| name.clone())
                    .collect();
                if newest.as_ref().is_none_or(|n| issue.updated_at > *n) {
                    newest = Some(issue.updated_at.clone());
                }
                // Comments are read only for an item some rule may take, since
                // there is no listing of a project's comments.
                let wanted = !issue.scopes.is_empty()
                    || self.scopes.iter().any(|s| {
                        s.rule.repo == *repo && s.rule.ado_query.is_none() && s.rule.filter(&issue)
                    });
                if wanted {
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

    /// Azure DevOps holds no pull requests of ours.
    async fn pull_request(&self, _id: &IssueRef) -> Result<Option<PullRequest>, SourceError> {
        Ok(None)
    }

    async fn fetch(&self, id: &IssueRef) -> Result<Fetched, SourceError> {
        let project = self.project_of(id)?.to_owned();
        let Some(raw) = self.read_item(id).await? else {
            return Ok(Fetched::Gone);
        };
        // An item moved to a project we do not serve is out of reach.
        let Some(mut issue) = self.parse(&raw) else {
            return Ok(Fetched::Gone);
        };
        if issue.id != *id {
            return Ok(Fetched::Moved(issue.id));
        }
        for s in self.scopes.iter().filter(|s| s.rule.repo == id.repo) {
            let Some(q) = &s.rule.ado_query else {
                continue;
            };
            if matches!(self.saved(&project, q).await?, Ok(ids) if ids.contains(&id.number)) {
                issue.scopes.push(s.rule.name.clone());
            }
        }
        Ok(Fetched::Issue(Box::new(issue)))
    }

    async fn issue_comments(&self, id: &IssueRef) -> Result<Vec<Comment>, SourceError> {
        let project = self.project_of(id)?;
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..COMMENT_PAGES {
            let mut q = vec![("$top", "200"), ("order", "asc")];
            if let Some(t) = &token {
                q.push(("continuationToken", t));
            }
            let url = self.url(
                Some(project),
                &format!("/wit/workItems/{}/comments", id.number),
                &q,
                COMMENTS_API,
            )?;
            let res = self.call(Req::get(url)).await?;
            match res.status {
                200 => {}
                404 | 410 => return Ok(out),
                s => {
                    return Err(
                        anyhow!("Azure DevOps answered {s} for the comments of {id}").into(),
                    );
                }
            }
            out.extend(
                res.body["comments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| self.parse_comment(id, c)),
            );
            match res.body["continuationToken"].as_str() {
                Some(t) if !t.is_empty() => token = Some(t.to_owned()),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn links(&self, id: &IssueRef) -> Result<Links, SourceError> {
        self.project_of(id)?;
        let Some(raw) = self.read_item(id).await? else {
            // A missing or unreadable item says nothing about its links.
            return Ok(Links::default());
        };
        let mut predecessors = Vec::new();
        let mut children = Vec::new();
        for r in raw["relations"].as_array().into_iter().flatten() {
            let Some(n) = r["url"]
                .as_str()
                .and_then(|u| u.rsplit('/').next())
                .and_then(|n| n.parse::<u64>().ok())
            else {
                continue;
            };
            match r["rel"].as_str() {
                Some("System.LinkTypes.Dependency-Reverse") => predecessors.push(n),
                Some("System.LinkTypes.Hierarchy-Forward") => children.push(n),
                _ => {}
            }
        }
        let wanted: Vec<u64> = predecessors.iter().chain(&children).copied().collect();
        if wanted.is_empty() {
            return Ok(Links {
                complete: true,
                ..Links::default()
            });
        }
        // A relation names no project, so the other end is read to learn it.
        let list = wanted
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let url = self.url(
            None,
            "/wit/workitems",
            &[
                ("ids", &list),
                ("fields", "System.TeamProject"),
                ("errorPolicy", "omit"),
            ],
            API,
        )?;
        let res = self.call(Req::get(url)).await?;
        if res.status != 200 {
            return Ok(Links::default());
        }
        let known: BTreeMap<u64, IssueRef> = res.body["value"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                let n = w["id"].as_u64()?;
                let slug = self.slug_of(w["fields"]["System.TeamProject"].as_str()?)?;
                Some((n, self.id_of(slug, n)))
            })
            .collect();
        let refs = |ns: &[u64]| -> Vec<IssueRef> {
            ns.iter().filter_map(|n| known.get(n).cloned()).collect()
        };
        Ok(Links {
            blocked_by: refs(&predecessors),
            sub_issues: refs(&children),
            complete: true,
        })
    }

    async fn write_back(&self, what: &WriteBack) -> Result<(), SourceError> {
        let issue = match what {
            WriteBack::DraftPullRequest { .. } => {
                return Err(anyhow!("Azure DevOps holds no pull requests to draft").into());
            }
            WriteBack::Comment { issue, .. }
            | WriteBack::LinkPullRequest { issue, .. }
            | WriteBack::Transition { issue, .. } => issue,
        };
        if !self.writes {
            return Err(anyhow!("no rule asks for a write to Azure DevOps").into());
        }
        let project = self.project_of(issue)?.to_owned();
        match what {
            WriteBack::DraftPullRequest { .. } => unreachable!("returned above"),
            WriteBack::Comment { issue, body } => {
                // A retry after a later step failed must not say it twice.
                if self
                    .issue_comments(issue)
                    .await?
                    .iter()
                    .any(|c| c.body == *body && self.own.contains(&c.author))
                {
                    return Ok(());
                }
                let url = self.url(
                    Some(&project),
                    &format!("/wit/workItems/{}/comments", issue.number),
                    &[],
                    COMMENTS_API,
                )?;
                self.post_write(Req::post(url, json!({ "text": html_escape(body) })))
                    .await
            }
            WriteBack::LinkPullRequest { issue, pr } => {
                let link = format!("{}/{}/pull/{}", self.pr_base, pr.repo, pr.number);
                let Some(raw) = self.read_item(issue).await? else {
                    return Ok(());
                };
                if raw["relations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|r| r["rel"] == "Hyperlink" && r["url"] == link.as_str())
                {
                    return Ok(());
                }
                let patch = json!([{
                    "op": "add",
                    "path": "/relations/-",
                    "value": {
                        "rel": "Hyperlink",
                        "url": link,
                        "attributes": { "comment": format!("Pull request {pr}") },
                    },
                }]);
                let url = self.item_url(issue, &project).await?;
                self.post_write(patch_req(url, patch)).await
            }
            WriteBack::Transition { issue, name } => {
                let Some(raw) = self.read_item(issue).await? else {
                    return Ok(());
                };
                let state = raw["fields"]["System.State"].as_str().unwrap_or_default();
                if state.eq_ignore_ascii_case(name) {
                    return Ok(());
                }
                let patch = json!([{
                    "op": "add",
                    "path": "/fields/System.State",
                    "value": name,
                }]);
                let url = self.item_url(issue, &project).await?;
                let res = self.call(patch_req(url, patch)).await?;
                match res.status {
                    200..=299 => Ok(()),
                    // The process does not allow the state from here: nothing to do.
                    400 => {
                        eprintln!("kbl-ingest: {issue} cannot take the state {name:?} now");
                        Ok(())
                    }
                    s => Err(anyhow!("Azure DevOps answered {s} to a state change").into()),
                }
            }
        }
    }

    /// Azure DevOps has no releases of ours to read; an empty, whole listing says so.
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
        Err(anyhow!("the Azure DevOps source does not open work items").into())
    }
}

fn patch_req(url: String, body: Value) -> Req {
    Req {
        method: "PATCH",
        url,
        if_none_match: None,
        body: Some(body),
    }
}

/// `done` as `Done`, for the state names of a query.
fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
