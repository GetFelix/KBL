//! The GitLab adapter, over the REST API (v4). Issues of the configured
//! projects are picked by the rules' own filters; merge requests stand in for
//! pull requests. Writes happen only in [`Source::write_back`] and
//! [`Source::open_issue`], and only where a rule asks for one.
//!
//! The address of the instance comes from the config alone. No address, path
//! or query is ever built from issue text, and a page of a listing is asked
//! for by number, never by following a link the server sends.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail, ensure};
use serde_json::{Value, json};

use kbl_core::work::{
    FailedCheck, MAX_CHECK_NAME, MAX_CHECK_URL, PrChecks, PrPhase, PrReview, PrSnapshot,
};

use crate::config::Rule;
use crate::github::{cut_bytes, parse_utc_ms, plain_ref};
use crate::source::{
    Changes, Closed, Comment, Cursor, Fetched, Issue, IssueRef, Links, PullRequest, Release,
    Releases, Source, SourceError, WriteBack,
};

pub(crate) mod transport;

pub(crate) use crate::github::{Req, Res, Transport};

/// Pages followed in one listing before the cursor stops where they ended.
const MAX_PAGES: usize = 20;
const COMMENT_PAGES: usize = 20;
const RELEASE_PAGES: usize = 5;
const REF_PAGES: usize = 10;
const PER_PAGE: &str = "100";
const MAX_WAIT: Duration = Duration::from_secs(3600);
const DEFAULT_WAIT: Duration = Duration::from_secs(30);
/// GitLab keeps no reason on a closed issue, so these labels say the work was
/// dropped. Any other close is a completion.
const DROPPED_LABELS: &[&str] = &[
    "wontfix",
    "won't fix",
    "duplicate",
    "invalid",
    "not planned",
    "resolution::wontfix",
    "resolution::duplicate",
    "resolution::not-planned",
];

/// The address of the instance, checked: https, or http on loopback only, no
/// credentials in it, and nothing after the path. The token goes out with
/// every request, so a plain remote address would send it in the clear.
pub(crate) fn base_url(api: &str) -> anyhow::Result<reqwest::Url> {
    // Not echoed in any message: it may carry a password.
    let url = reqwest::Url::parse(api.trim()).map_err(|_| anyhow!("is not a web address"))?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "carries a user name or password; the token goes in token_env or token_file"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "has a query or a fragment"
    );
    let host = url.host_str().context("names no host")?;
    let host = host.trim_matches(['[', ']']);
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|a| a.is_loopback());
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => bail!(
            "is plain http, which would send the token in the clear; use https \
             (http is allowed on loopback only)"
        ),
        _ => bail!("is not http or https"),
    }
    Ok(url)
}

/// The full path of the project a rule repo names: its entry in `projects`,
/// or the repo itself. At least two plain names.
pub(crate) fn project_path(projects: &BTreeMap<String, String>, repo: &str) -> Option<String> {
    let real = projects
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(repo))
        .map_or(repo, |(_, v)| v.as_str());
    let plain = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    let mut parts = 0;
    for p in real.split('/') {
        if !plain(p) {
            return None;
        }
        parts += 1;
    }
    (parts >= 2).then(|| real.to_owned())
}

pub(crate) struct Gitlab<T> {
    api: String,
    http: T,
    own: Vec<String>,
    rules: Vec<Rule>,
    /// Rule repo to the project's full path.
    projects: Vec<(String, String)>,
    /// Whether any rule writes. Without one the adapter refuses every write,
    /// so a `read_api` token is all that is ever needed.
    writes: bool,
    limited_until: Mutex<Option<Instant>>,
}

impl<T: Transport> Gitlab<T> {
    pub(crate) fn new(
        api: &str,
        http: T,
        own: Vec<String>,
        rules: Vec<Rule>,
        names: &BTreeMap<String, String>,
    ) -> anyhow::Result<Self> {
        let url = base_url(api).context("the GitLab address")?;
        let mut projects: Vec<(String, String)> = Vec::new();
        for r in &rules {
            let real = project_path(names, &r.repo)
                .with_context(|| format!("rule {:?} names no GitLab project path", r.name))?;
            if !projects
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case(&r.repo))
            {
                projects.push((r.repo.clone(), real));
            }
        }
        let writes = rules.iter().any(|r| {
            r.comment
                || r.transition_on_pr.is_some()
                || r.transition_on_merge.is_some()
                || r.open_prerequisites
        });
        Ok(Self {
            api: url.as_str().trim_end_matches('/').to_owned(),
            http,
            own,
            rules,
            projects,
            writes,
            limited_until: Mutex::new(None),
        })
    }

    /// Fails when nothing says which account the token is. Without it a
    /// retried write-back posts its comment again and no merge request is ours.
    pub(crate) fn require_own(&self) -> anyhow::Result<()> {
        ensure!(
            !self.own.is_empty(),
            "GitLab did not say who this token is (GET /user failed) and gitlab.own_logins \
             is empty; set own_logins"
        );
        Ok(())
    }

    pub(crate) fn add_own(&mut self, names: Vec<String>) {
        self.own.extend(names);
    }

    /// The username the token acts as: the bot user of an access token.
    pub(crate) async fn login(&self) -> Vec<String> {
        let Ok(url) = self.url(&["user"], &[]) else {
            return Vec::new();
        };
        let Ok(res) = self.call(Req::get(url)).await else {
            return Vec::new();
        };
        res.body["username"]
            .as_str()
            .map(str::to_owned)
            .into_iter()
            .collect()
    }

    fn is_own(&self, name: &str) -> bool {
        self.own.iter().any(|o| o.eq_ignore_ascii_case(name))
    }

    /// `{api}/api/v4/{parts}`, each part one path segment, so a name can never
    /// add a segment (a project path is `group%2Fproject`) or a query.
    fn url(&self, parts: &[&str], query: &[(&str, &str)]) -> anyhow::Result<String> {
        let mut url = reqwest::Url::parse(&self.api).context("the GitLab address")?;
        {
            let mut seg = url
                .path_segments_mut()
                .map_err(|()| anyhow!("the GitLab address cannot take a path"))?;
            seg.pop_if_empty().push("api").push("v4");
            for part in parts {
                seg.push(part);
            }
        }
        if !query.is_empty() {
            let mut q = url.query_pairs_mut();
            for (k, v) in query {
                q.append_pair(k, v);
            }
        }
        Ok(url.to_string())
    }

    fn in_project(
        &self,
        project: &str,
        rest: &[&str],
        query: &[(&str, &str)],
    ) -> anyhow::Result<String> {
        let mut parts = vec!["projects", project];
        parts.extend_from_slice(rest);
        self.url(&parts, query)
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
                .map_or(DEFAULT_WAIT, |s| Duration::from_secs(s.max(1)))
                .min(MAX_WAIT);
            *self.limited_until.lock().expect("lock") = Some(Instant::now() + wait);
            return Err(SourceError::RateLimited { wait });
        }
        // Redirects are not followed: one is a sign-in page, a moved
        // instance or a renamed project, and none is an answer to trust.
        if (300..400).contains(&res.status) {
            res.status = 401;
        }
        if res.status >= 500 {
            return Err(anyhow!("GitLab answered {}", res.status).into());
        }
        Ok(res)
    }

    /// A GET of one API path, for `kbl-ingest check`'s credential and permission probes.
    pub(crate) async fn probe(
        &self,
        parts: &[&str],
        query: &[(&str, &str)],
    ) -> Result<Res, SourceError> {
        self.call(Req::get(self.url(parts, query)?)).await
    }

    /// The full path of a rule repo, or an error for one outside the
    /// configured projects: nothing is read or written there.
    pub(crate) fn project_of(&self, repo: &str) -> Result<&str, SourceError> {
        self.projects
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(repo))
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| anyhow!("{repo} is not a project this worker is configured for").into())
    }

    /// The rule repo of a project path, in the spelling the rules use.
    fn repo_of(&self, path: &str) -> Option<&str> {
        self.projects
            .iter()
            .find(|(_, v)| v.eq_ignore_ascii_case(path))
            .map(|(k, _)| k.as_str())
    }

    /// Every page of a listing, a page at a time, up to `max` pages. The
    /// status that says this project cannot be read is returned instead.
    async fn listing(
        &self,
        project: &str,
        rest: &[&str],
        query: &[(&str, &str)],
        max: usize,
    ) -> Result<Result<Vec<Value>, u16>, SourceError> {
        let mut out = Vec::new();
        for page in 1..=max {
            let n = page.to_string();
            let mut q = query.to_vec();
            q.push(("per_page", PER_PAGE));
            q.push(("page", &n));
            let res = self
                .call(Req::get(self.in_project(project, rest, &q)?))
                .await?;
            match res.status {
                200 => {}
                401 | 403 | 404 => return Ok(Err(res.status)),
                s => return Err(anyhow!("GitLab answered {s} to a listing").into()),
            }
            let items = res.body.as_array().cloned().unwrap_or_default();
            let full = items.len().to_string() == PER_PAGE;
            out.extend(items);
            let more = match res.headers.get("x-next-page") {
                Some(n) => !n.trim().is_empty(),
                None => full,
            };
            if !more {
                break;
            }
        }
        Ok(Ok(out))
    }

    fn parse_issue(&self, repo: &str, raw: &Value) -> Option<Issue> {
        let number = raw["iid"].as_u64()?;
        let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        let labels: Vec<String> = raw["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(str::to_owned))
            .collect();
        let closed = (raw["state"] == "closed").then(|| {
            let dropped = !raw["moved_to_id"].is_null()
                || labels
                    .iter()
                    .any(|l| DROPPED_LABELS.iter().any(|d| l.eq_ignore_ascii_case(d)));
            if dropped {
                Closed::Dropped
            } else {
                Closed::Completed
            }
        });
        Some(Issue {
            id: IssueRef::new(repo, number),
            title: text(&raw["title"]),
            body: text(&raw["description"]),
            closed,
            labels,
            milestone: raw["milestone"]["title"].as_str().map(str::to_owned),
            assignees: raw["assignees"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["username"].as_str().map(str::to_owned))
                .collect(),
            kind: raw["type"].as_str().map(str::to_owned),
            author: text(&raw["author"]["username"]),
            url: text(&raw["web_url"]),
            updated_at: text(&raw["updated_at"]),
            priority: None,
            components: Vec::new(),
            scopes: Vec::new(),
        })
    }

    /// A person's note. A system note (a label or milestone change) and an
    /// internal one are not the discussion, so they are left out.
    fn parse_note(id: &IssueRef, raw: &Value) -> Option<Comment> {
        if raw["system"] == true || raw["internal"] == true {
            return None;
        }
        Some(Comment {
            id: raw["id"].as_u64()?,
            issue: id.clone(),
            author: raw["author"]["username"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            body: raw["body"].as_str().unwrap_or_default().to_owned(),
            updated_at: raw["updated_at"].as_str().unwrap_or_default().to_owned(),
        })
    }

    /// Whether a note our credential wrote under `path` carries `mark`.
    async fn own_note(
        &self,
        project: &str,
        path: &[&str],
        mark: &str,
    ) -> Result<bool, SourceError> {
        let listed = self
            .listing(project, path, &[("sort", "asc")], COMMENT_PAGES)
            .await?;
        match listed {
            Ok(items) => Ok(items.iter().any(|raw| {
                raw["body"].as_str().is_some_and(|b| b.contains(mark))
                    && raw["author"]["username"]
                        .as_str()
                        .is_some_and(|a| self.is_own(a))
            })),
            Err(status) => Err(anyhow!("GitLab answered {status} for the notes").into()),
        }
    }

    async fn project_issue(&self, id: &IssueRef) -> Result<Option<Value>, SourceError> {
        let project = self.project_of(&id.repo)?;
        let n = id.number.to_string();
        let url = self.in_project(project, &["issues", &n], &[])?;
        let res = self.call(Req::get(url)).await?;
        match res.status {
            200 => Ok(Some(res.body)),
            404 | 410 => Ok(None),
            s => Err(anyhow!("GitLab answered {s} for {id}").into()),
        }
    }

    async fn post_write(&self, req: Req) -> Result<(), SourceError> {
        let res = self.call(req).await?;
        if (200..300).contains(&res.status) {
            Ok(())
        } else {
            Err(anyhow!("GitLab answered {} to a write", res.status).into())
        }
    }

    /// The pipeline's state as checks: red with the first failed job that
    /// counts, pending until it ends, green when it passed or was skipped.
    async fn checks(
        &self,
        project: &str,
        pipeline: &Value,
    ) -> Result<(PrChecks, Option<FailedCheck>), SourceError> {
        let page = pipeline["web_url"].as_str().unwrap_or_default();
        match pipeline["status"].as_str() {
            Some("success" | "skipped") => Ok((PrChecks::Green, None)),
            Some("failed") => {
                let mut failing: Option<FailedCheck> = None;
                if let Some(pid) = pipeline["id"].as_u64() {
                    let pid = pid.to_string();
                    let url = self.in_project(
                        project,
                        &["pipelines", &pid, "jobs"],
                        &[("scope[]", "failed"), ("per_page", PER_PAGE)],
                    )?;
                    let res = self.call(Req::get(url)).await?;
                    for job in res.body.as_array().into_iter().flatten() {
                        // A job allowed to fail does not fail the pipeline.
                        if res.status != 200 || job["allow_failure"] == true {
                            continue;
                        }
                        let name =
                            cut_bytes(job["name"].as_str().unwrap_or_default(), MAX_CHECK_NAME);
                        if failing.as_ref().is_none_or(|f| name < f.name) {
                            failing = Some(FailedCheck {
                                name,
                                url: cut_bytes(
                                    job["web_url"].as_str().unwrap_or(page),
                                    MAX_CHECK_URL,
                                ),
                            });
                        }
                    }
                }
                let failing = failing.unwrap_or_else(|| FailedCheck {
                    name: "pipeline".into(),
                    url: cut_bytes(page, MAX_CHECK_URL),
                });
                Ok((PrChecks::Red, Some(failing)))
            }
            // Created, running, waiting, manual, cancelled or unknown: not green yet.
            _ => Ok((PrChecks::Pending, None)),
        }
    }

    /// Approvals and requested changes on an open merge request, from people
    /// other than its author. GitLab lets only eligible users approve.
    async fn review(
        &self,
        project: &str,
        n: &str,
        author: &str,
        asked: bool,
    ) -> Result<PrReview, SourceError> {
        let mut approved = false;
        let mut changes = false;
        let res = self
            .call(Req::get(self.in_project(
                project,
                &["merge_requests", n, "approvals"],
                &[],
            )?))
            .await?;
        if res.status == 200 {
            approved |= res.body["approved_by"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|a| {
                    a["user"]["username"]
                        .as_str()
                        .is_some_and(|u| !u.eq_ignore_ascii_case(author))
                });
        }
        let res = self
            .call(Req::get(self.in_project(
                project,
                &["merge_requests", n, "reviewers"],
                &[],
            )?))
            .await?;
        if res.status == 200 {
            for r in res.body.as_array().into_iter().flatten() {
                let who = r["user"]["username"].as_str().unwrap_or_default();
                if who.eq_ignore_ascii_case(author) {
                    continue;
                }
                match r["state"].as_str() {
                    Some("requested_changes") => changes = true,
                    Some("approved") => approved = true,
                    _ => {}
                }
            }
        }
        Ok(if changes {
            PrReview::ChangesRequested
        } else if approved {
            PrReview::Approved
        } else if asked {
            PrReview::Requested
        } else {
            PrReview::Unreviewed
        })
    }
}

/// Milliseconds since the epoch from a GitLab time, `2026-11-10T12:00:05.123Z`.
/// Only UTC is read: a time with an offset is unknown, never guessed.
fn utc_ms(t: &str) -> Option<u64> {
    let head = t.get(..19)?;
    let rest = t.get(19..)?;
    let fraction = rest.strip_suffix('Z')?;
    let ok = fraction.is_empty()
        || fraction
            .strip_prefix('.')
            .is_some_and(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
    if !ok {
        return None;
    }
    let ms: u64 = fraction
        .strip_prefix('.')
        .map(|f| format!("{:0<3}", &f[..f.len().min(3)]))
        .map_or(0, |f| f.parse().unwrap_or(0));
    parse_utc_ms(&format!("{head}Z")).map(|s| s + ms)
}

impl<T: Transport> Source for Gitlab<T> {
    fn name(&self) -> &'static str {
        "gitlab"
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
            let Ok(project) = self.project_of(repo) else {
                continue;
            };
            let cursor = cursors.get(repo).cloned().unwrap_or_default();
            // A cursor is saved state; one that is not a time is not sent.
            let since = cursor
                .issues_since
                .as_deref()
                .filter(|s| utc_ms(s).is_some());
            // The first poll reads open issues only; later ones read every
            // change, which includes a close.
            let state = if since.is_some() { "all" } else { "opened" };
            let mut query = vec![
                ("scope", "all"),
                ("state", state),
                ("order_by", "updated_at"),
                ("sort", "asc"),
            ];
            if let Some(s) = since {
                query.push(("updated_after", s));
            }
            let listed = match self
                .listing(project, &["issues"], &query, MAX_PAGES)
                .await?
            {
                Ok(l) => l,
                Err(status) => {
                    eprintln!("kbl-ingest: {repo} answered {status}; skipping it this poll");
                    changes.denied.insert(repo.clone(), status);
                    continue;
                }
            };
            let mut newest = cursor.issues_since.clone();
            let mut issues = Vec::new();
            for raw in &listed {
                let Some(issue) = self.parse_issue(repo, raw) else {
                    eprintln!("kbl-ingest: skipped a GitLab issue of {repo} that does not parse");
                    continue;
                };
                if !issue.updated_at.is_empty()
                    && newest.as_ref().is_none_or(|n| issue.updated_at > *n)
                {
                    newest = Some(issue.updated_at.clone());
                }
                // There is no listing of a project's notes, so they are read
                // per issue, and only for one some rule may take.
                let quiet = raw["user_notes_count"].as_u64() == Some(0);
                let wanted = self
                    .rules
                    .iter()
                    .any(|r| r.repo.eq_ignore_ascii_case(repo) && r.filter(&issue));
                if wanted && !quiet {
                    changes
                        .comments
                        .extend(self.issue_comments(&issue.id).await?);
                }
                issues.push(issue);
            }
            changes.issues.extend(issues);
            changes.cursors.insert(
                repo.clone(),
                Cursor {
                    issues_since: newest,
                    ..Cursor::default()
                },
            );
        }
        changes
            .issues
            .sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        changes
            .comments
            .sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        Ok(changes)
    }

    async fn pull_request(&self, id: &IssueRef) -> Result<Option<PullRequest>, SourceError> {
        let project = self.project_of(&id.repo)?.to_owned();
        let n = id.number.to_string();
        let url = self.in_project(&project, &["merge_requests", &n], &[])?;
        let res = self.call(Req::get(url)).await?;
        match res.status {
            200 => {}
            404 | 410 => return Ok(None),
            s => return Err(anyhow!("GitLab answered {s} for {id}").into()),
        }
        let mr = &res.body;
        let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        let author = text(&mr["author"]["username"]);
        let head_ref = text(&mr["source_branch"]);
        let body = text(&mr["description"]);
        let created_ms = mr["created_at"].as_str().and_then(utc_ms);
        // Only a request our credential opened is read further: the name
        // comes from an agent, and its checks and reviews steer a person.
        if !self.is_own(&author) {
            return Ok(Some(PullRequest {
                author,
                head_ref,
                body,
                created_ms,
                status: None,
                merge: None,
            }));
        }
        let state = mr["state"].as_str();
        // A merge counts only into the project's default branch and by
        // someone other than us: agents never merge their own work. A merge
        // whose merger or default branch cannot be read does not count.
        let counted = state == Some("merged") && {
            // Unreadable is not "not default": a transient failure must not
            // read a real merge as closed.
            let project = self
                .call(Req::get(self.in_project(&project, &[], &[])?))
                .await?;
            if project.status != 200 {
                return Err(
                    anyhow!("GitLab answered {} for the project of {id}", project.status).into(),
                );
            }
            let into_default = project.body["default_branch"]
                .as_str()
                .is_some_and(|d| mr["target_branch"].as_str() == Some(d));
            let merger = mr["merge_user"]["username"]
                .as_str()
                .or_else(|| mr["merged_by"]["username"].as_str());
            into_default && merger.is_some_and(|m| !self.is_own(m))
        };
        let phase = match state {
            Some("merged") if counted => PrPhase::Merged,
            Some("opened" | "locked") => PrPhase::Open,
            _ => PrPhase::Closed,
        };
        let mut status = PrSnapshot {
            pr: id.to_string(),
            phase,
            review: PrReview::Unreviewed,
            checks: PrChecks::NoChecks,
            failing: None,
        };
        if phase != PrPhase::Open {
            // Without the merge time no release can be placed after it.
            let merge = (phase == PrPhase::Merged)
                .then(|| {
                    let sha = ["merge_commit_sha", "squash_commit_sha", "sha"]
                        .iter()
                        .find_map(|k| mr[k].as_str().filter(|s| !s.is_empty()))?;
                    let at = mr["merged_at"].as_str().and_then(utc_ms)?;
                    Some((sha.to_owned(), at))
                })
                .flatten();
            return Ok(Some(PullRequest {
                author,
                head_ref,
                body,
                created_ms,
                status: Some(status),
                merge,
            }));
        }
        let asked = mr["reviewers"].as_array().is_some_and(|a| !a.is_empty());
        status.review = self.review(&project, &n, &author, asked).await?;
        if mr["head_pipeline"].is_object() {
            (status.checks, status.failing) = self.checks(&project, &mr["head_pipeline"]).await?;
        }
        Ok(Some(PullRequest {
            author,
            head_ref,
            body,
            created_ms,
            status: Some(status),
            merge: None,
        }))
    }

    async fn fetch(&self, id: &IssueRef) -> Result<Fetched, SourceError> {
        let Some(raw) = self.project_issue(id).await? else {
            return Ok(Fetched::Gone);
        };
        Ok(match self.parse_issue(&id.repo, &raw) {
            Some(issue) => Fetched::Issue(Box::new(issue)),
            None => Fetched::Gone,
        })
    }

    async fn issue_comments(&self, id: &IssueRef) -> Result<Vec<Comment>, SourceError> {
        let project = self.project_of(&id.repo)?;
        let n = id.number.to_string();
        let listed = self
            .listing(
                project,
                &["issues", &n, "notes"],
                &[("order_by", "created_at"), ("sort", "asc")],
                COMMENT_PAGES,
            )
            .await?;
        match listed {
            Ok(items) => Ok(items
                .iter()
                .filter_map(|raw| Self::parse_note(id, raw))
                .collect()),
            Err(status) => {
                eprintln!("kbl-ingest: the comments of {id} answered {status}; skipping them");
                Ok(Vec::new())
            }
        }
    }

    async fn links(&self, id: &IssueRef) -> Result<Links, SourceError> {
        let project = self.project_of(&id.repo)?;
        let n = id.number.to_string();
        let url = self.in_project(project, &["issues", &n, "links"], &[])?;
        let res = self.call(Req::get(url)).await?;
        match res.status {
            200 => {}
            // Unreadable says nothing about the links.
            401 | 403 | 404 | 410 => return Ok(Links::default()),
            s => return Err(anyhow!("GitLab answered {s} for the links of {id}").into()),
        }
        let Some(list) = res.body.as_array() else {
            return Ok(Links::default());
        };
        let mut links = Links {
            complete: true,
            ..Links::default()
        };
        for raw in list {
            // Only the issues this one waits on are edges. `blocks` is the
            // other issue's wait, which its own listing shows, and `relates_to`
            // waits on nothing.
            if raw["link_type"] != "is_blocked_by" {
                continue;
            }
            let other = raw["references"]["full"]
                .as_str()
                .and_then(|r| r.rsplit_once('#'))
                .and_then(|(path, n)| Some((self.repo_of(path)?, n.parse::<u64>().ok()?)));
            match other {
                Some((repo, n)) => links.blocked_by.push(IssueRef::new(repo, n)),
                // A blocker in a project no rule reads cannot be an edge, but
                // saying the listing is whole would let its issue look unblocked.
                None => links.complete = false,
            }
        }
        Ok(links)
    }

    async fn write_back(&self, what: &WriteBack) -> Result<(), SourceError> {
        if matches!(what, WriteBack::LinkPullRequest { .. }) {
            return Err(anyhow!("GitLab takes no pull request link; the comment names it").into());
        }
        if !self.writes {
            return Err(anyhow!("no rule asks for a write to GitLab").into());
        }
        match what {
            WriteBack::LinkPullRequest { .. } => unreachable!("returned above"),
            WriteBack::DraftPullRequest { pr, note, mark } => {
                let project = self.project_of(&pr.repo)?.to_owned();
                let n = pr.number.to_string();
                let url = self.in_project(&project, &["merge_requests", &n], &[])?;
                let res = self.call(Req::get(url.clone())).await?;
                match res.status {
                    200 => {}
                    404 | 410 => return Ok(()),
                    s => return Err(anyhow!("GitLab answered {s} for {pr}").into()),
                }
                // A merged or closed request needs nothing from us.
                if res.body["state"] != "opened" {
                    return Ok(());
                }
                // The request is named by an agent, so only one our own
                // credential opened may be touched.
                let author = res.body["author"]["username"].as_str().unwrap_or_default();
                if !self.is_own(author) {
                    eprintln!("kbl-ingest: {pr} was not opened by us; leaving it alone");
                    return Ok(());
                }
                let draft = res.body["draft"] == true;
                if !draft {
                    let title = res.body["title"].as_str().unwrap_or_default();
                    self.post_write(put_req(url, json!({ "title": format!("Draft: {title}") })))
                        .await?;
                }
                // A post the fence cut short may have landed.
                if self
                    .own_note(&project, &["merge_requests", &n, "notes"], mark)
                    .await?
                {
                    return Ok(());
                }
                let notes = self.in_project(&project, &["merge_requests", &n, "notes"], &[])?;
                let body = format!("{note}\n\n{mark}");
                self.post_write(Req::post(notes, json!({ "body": body })))
                    .await
            }
            WriteBack::Comment { issue, body, mark } => {
                let project = self.project_of(&issue.repo)?.to_owned();
                // The engine's text names a merge request as `repo#n`, which
                // GitLab would read as an issue. A merge request is `path!n`.
                let body = self
                    .projects
                    .iter()
                    .fold(body.clone(), |b, (alias, real)| mr_refs(&b, alias, real));
                // A retry, after a later step failed or a post was cut short,
                // must not say it twice. Older notes carry no mark.
                if self.issue_comments(issue).await?.iter().any(|c| {
                    (c.body.contains(mark.as_str()) || c.body.trim() == body.trim())
                        && self.is_own(&c.author)
                }) {
                    return Ok(());
                }
                let n = issue.number.to_string();
                let url = self.in_project(&project, &["issues", &n, "notes"], &[])?;
                let body = format!("{body}\n\n{mark}");
                self.post_write(Req::post(url, json!({ "body": body })))
                    .await
            }
            WriteBack::Transition { issue, name } => {
                let event = match name.as_str() {
                    "close" => "close",
                    "reopen" => "reopen",
                    _ => return Err(anyhow!("a GitLab transition is close or reopen").into()),
                };
                let project = self.project_of(&issue.repo)?.to_owned();
                let Some(raw) = self.project_issue(issue).await? else {
                    return Ok(());
                };
                let state = raw["state"].as_str().unwrap_or_default();
                // Already there, or moved: nothing to do.
                if (event == "close" && state == "closed")
                    || (event == "reopen" && state == "opened")
                    || !raw["moved_to_id"].is_null()
                {
                    return Ok(());
                }
                let n = issue.number.to_string();
                let url = self.in_project(&project, &["issues", &n], &[])?;
                self.post_write(put_req(url, json!({ "state_event": event })))
                    .await
            }
        }
    }

    async fn releases(&self, repo: &str, since_ms: u64) -> Result<Releases, SourceError> {
        let project = self.project_of(repo)?.to_owned();
        // Newest first, which is publish order. One more page past the first
        // release before `since_ms` catches one that was published late.
        let mut out = Releases::default();
        let mut past = false;
        for page in 1..=RELEASE_PAGES {
            let n = page.to_string();
            let url = self.in_project(
                &project,
                &["releases"],
                &[
                    ("order_by", "released_at"),
                    ("sort", "desc"),
                    ("per_page", PER_PAGE),
                    ("page", &n),
                ],
            )?;
            let res = self.call(Req::get(url)).await?;
            match res.status {
                200 => {}
                403 | 404 => {
                    out.whole = true;
                    break;
                }
                s => return Err(anyhow!("GitLab answered {s} for the releases of {repo}").into()),
            }
            let listed = res.body.as_array().cloned().unwrap_or_default();
            let mut older = false;
            for r in &listed {
                let Some(at) = r["released_at"].as_str().and_then(utc_ms) else {
                    continue;
                };
                out.oldest_ms = Some(out.oldest_ms.map_or(at, |o| o.min(at)));
                if at < since_ms {
                    older = true;
                    continue;
                }
                // A release dated ahead is not out yet.
                if r["upcoming_release"] == true {
                    continue;
                }
                if let Some(tag) = r["tag_name"].as_str().filter(|t| plain_ref(t)) {
                    out.list.push(Release {
                        tag: tag.to_owned(),
                        published_ms: at,
                    });
                }
            }
            if listed.len() < 100 {
                out.whole = true;
                break;
            }
            if past {
                break;
            }
            past = older;
        }
        out.list
            .sort_by(|a, b| (a.published_ms, &a.tag).cmp(&(b.published_ms, &b.tag)));
        out.list.dedup();
        Ok(out)
    }

    async fn contains(&self, repo: &str, tag: &str, sha: &str) -> Result<bool, SourceError> {
        if !plain_ref(tag) || !plain_ref(sha) {
            return Ok(false);
        }
        let project = self.project_of(repo)?.to_owned();
        // The tags that hold the commit, exactly.
        for page in 1..=REF_PAGES {
            let n = page.to_string();
            let url = self.in_project(
                &project,
                &["repository", "commits", sha, "refs"],
                &[("type", "tag"), ("per_page", PER_PAGE), ("page", &n)],
            )?;
            let res = self.call(Req::get(url)).await?;
            match res.status {
                200 => {}
                404 => return Ok(false),
                s => {
                    return Err(anyhow!(
                        "GitLab answered {s} for the tags holding {sha} in {repo}"
                    )
                    .into());
                }
            }
            if res
                .body
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["name"].as_str() == Some(tag))
            {
                return Ok(true);
            }
            if res
                .headers
                .get("x-next-page")
                .is_none_or(|n| n.trim().is_empty())
            {
                break;
            }
        }
        Ok(false)
    }

    async fn open_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
    ) -> Result<IssueRef, SourceError> {
        if !self.writes {
            return Err(anyhow!("no rule asks for a write to GitLab").into());
        }
        // The repo comes from an agent's prerequisite, so it must be a project a rule reads.
        let project = self.project_of(repo)?;
        let url = self.in_project(project, &["issues"], &[])?;
        let out = self
            .call(Req::post(
                url,
                json!({ "title": title, "description": body }),
            ))
            .await?;
        match (out.status, out.body["iid"].as_u64()) {
            (201, Some(n)) => Ok(IssueRef::new(repo, n)),
            (s, _) => Err(anyhow!("could not open an issue in {repo}: {s}").into()),
        }
    }
}

/// `alias#n` as `real!n`, ignoring case, where the alias starts a word: a
/// longer path that merely ends in it is left alone.
fn mr_refs(text: &str, alias: &str, real: &str) -> String {
    let (lower, key) = (
        text.to_ascii_lowercase(),
        format!("{}#", alias.to_ascii_lowercase()),
    );
    let mut out = String::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find(&key) {
        let i = at + i;
        let word = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/')));
        out.push_str(&text[at..i]);
        if word {
            out.push_str(real);
            out.push('!');
        } else {
            out.push_str(&text[i..i + key.len()]);
        }
        at = i + key.len();
    }
    out.push_str(&text[at..]);
    out
}

fn put_req(url: String, body: Value) -> Req {
    Req {
        method: "PUT",
        url,
        if_none_match: None,
        body: Some(body),
    }
}

#[cfg(test)]
mod tests;
