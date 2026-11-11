//! The GitHub adapter, over the REST API. Writes happen only in
//! [`Source::write_back`], and only to pull requests the tracker named.
//!
//! Rate limits: a response that says the budget is spent (or a 403/429 with
//! `retry-after`) makes every later call fail fast with the wait, instead of
//! spending requests on answers that will be refused. Listings are
//! conditional (`If-None-Match`), and GitHub does not charge a 304.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use serde_json::{Value, json};

use crate::source::{
    Changes, Closed, Comment, Cursor, Fetched, Issue, IssueRef, Links, Source, SourceError,
    WriteBack,
};

pub(crate) mod transport;

pub(crate) use transport::{Req, Res, Transport};

/// Pages followed in one listing before the cursor stops where they ended.
const MAX_PAGES: usize = 20;
/// The longest a rate-limit wait is trusted to be; a bogus header must not park the worker for days.
const MAX_WAIT: Duration = Duration::from_secs(3600);

pub(crate) struct Github<T> {
    api: String,
    http: T,
    own: Vec<String>,
    /// Until when GitHub told us to hold off, as seconds since the epoch.
    limited_until: Mutex<Option<u64>>,
}

impl<T: Transport> Github<T> {
    pub(crate) fn new(api: &str, http: T, own: Vec<String>) -> Self {
        Self {
            api: api.trim_end_matches('/').to_owned(),
            http,
            own,
            limited_until: Mutex::new(None),
        }
    }

    pub(crate) fn add_own(&mut self, login: String) {
        self.own.push(login);
    }

    /// The login the credential acts as, if it is a user token. An App
    /// installation token has none, so the config lists its bot login.
    pub(crate) async fn login(&self) -> Option<String> {
        let res = self
            .call(Req::get(format!("{}/user", self.api)))
            .await
            .ok()?;
        res.body["login"].as_str().map(str::to_owned)
    }

    async fn call(&self, req: Req) -> Result<Res, SourceError> {
        if let Some(until) = *self.limited_until.lock().expect("lock") {
            let now = epoch();
            if until > now {
                return Err(SourceError::RateLimited {
                    wait: Duration::from_secs(until - now).min(MAX_WAIT),
                });
            }
        }
        let res = self.http.send(req).await?;
        if let Some(wait) = rate_limit_wait(&res) {
            *self.limited_until.lock().expect("lock") = Some(epoch() + wait.as_secs());
            // A spent budget on a success still answers; only refusals fail.
            if matches!(res.status, 403 | 429) {
                return Err(SourceError::RateLimited { wait });
            }
        }
        if res.status >= 500 {
            return Err(anyhow!("GitHub answered {}", res.status).into());
        }
        Ok(res)
    }

    /// Every page of a listing, oldest first. `etag_key` is set when a
    /// single-page answer may be cached; a 304 returns nothing.
    async fn pages(
        &self,
        first: String,
        known: &BTreeMap<String, String>,
        etags: &mut BTreeMap<String, String>,
    ) -> Result<Listing, SourceError> {
        let mut out = Vec::new();
        let mut url = first.clone();
        for page in 0..MAX_PAGES {
            let mut req = Req::get(url.clone());
            if page == 0 {
                req.if_none_match = known.get(&first).cloned();
            }
            let res = self.call(req).await?;
            match res.status {
                304 => {
                    if let Some(tag) = known.get(&first) {
                        etags.insert(first, tag.clone());
                    }
                    return Ok(Listing::default());
                }
                200 => {}
                s => return Err(anyhow!("GitHub answered {s} to {url}").into()),
            }
            let next = res.headers.get("link").and_then(|l| next_link(l));
            let items = res.body.as_array().cloned().unwrap_or_default();
            if page == 0 {
                // Only a listing that fits one page can be validated by the
                // first page alone: later pages change without it changing.
                if let (None, Some(tag)) = (&next, res.headers.get("etag")) {
                    etags.insert(first.clone(), tag.clone());
                }
            }
            out.extend(items);
            match next {
                Some(n) => url = n,
                None => {
                    return Ok(Listing {
                        items: out,
                        truncated: false,
                    });
                }
            }
        }
        Ok(Listing {
            items: out,
            truncated: true,
        })
    }
}

#[derive(Default)]
struct Listing {
    items: Vec<Value>,
    /// The page limit stopped it before the end.
    truncated: bool,
}

impl<T: Transport> Source for Github<T> {
    fn name(&self) -> &'static str {
        "github"
    }

    fn own_logins(&self) -> Vec<String> {
        self.own.clone()
    }

    async fn changed(&self, repos: &[String], cursor: &Cursor) -> Result<Changes, SourceError> {
        // Only this poll's validators are kept, so old `since` values do not pile up.
        let mut etags = BTreeMap::new();
        let mut issues = Vec::new();
        let mut comments = Vec::new();
        // Where each repo's listing got to, to resume the shared cursor.
        let mut reached: Vec<(String, bool)> = Vec::new();
        let mut comment_reached: Vec<(String, bool)> = Vec::new();
        for repo in repos {
            let state = if cursor.issues_since.is_some() {
                "all"
            } else {
                "open"
            };
            let mut url = format!(
                "{}/repos/{repo}/issues?state={state}&sort=updated&direction=asc&per_page=100",
                self.api
            );
            if let Some(since) = &cursor.issues_since {
                url.push_str(&format!("&since={since}"));
            }
            let listing = self.pages(url, &cursor.etags, &mut etags).await?;
            let mut last = None;
            for raw in &listing.items {
                // The issues endpoint lists pull requests too.
                if raw.get("pull_request").is_some() {
                    continue;
                }
                let issue = parse_issue(repo, raw)?;
                last = Some(issue.updated_at.clone());
                issues.push(issue);
            }
            if let Some(l) = last {
                reached.push((l, listing.truncated));
            }

            if let Some(since) = &cursor.comments_since {
                let url = format!(
                    "{}/repos/{repo}/issues/comments?sort=updated&direction=asc&per_page=100&since={since}",
                    self.api
                );
                let listing = self.pages(url, &cursor.etags, &mut etags).await?;
                let mut last = None;
                for raw in &listing.items {
                    let c = parse_comment(repo, raw)?;
                    last = Some(c.updated_at.clone());
                    comments.push(c);
                }
                if let Some(l) = last {
                    comment_reached.push((l, listing.truncated));
                }
            }
        }
        issues.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        comments.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        let issues_since = resume(&reached).or_else(|| cursor.issues_since.clone());
        // The first poll sees no comments; later ones start where the issues
        // were, since a comment bumps its issue's update time.
        let comments_since = match &cursor.comments_since {
            Some(old) => resume(&comment_reached).or_else(|| Some(old.clone())),
            None => issues_since.clone(),
        };
        Ok(Changes {
            issues,
            comments,
            cursor: Cursor {
                issues_since,
                comments_since,
                etags,
            },
        })
    }

    async fn fetch(&self, id: &IssueRef) -> Result<Fetched, SourceError> {
        let url = format!("{}/repos/{}/issues/{}", self.api, id.repo, id.number);
        let res = self.call(Req::get(url)).await?;
        match res.status {
            200 if res.body.get("pull_request").is_some() => Ok(Fetched::Gone),
            200 => Ok(Fetched::Issue(Box::new(parse_issue(&id.repo, &res.body)?))),
            301 | 302 | 307 | 308 => {
                let to = res
                    .headers
                    .get("location")
                    .and_then(|l| issue_path(l))
                    .context("a redirect for an issue that names no issue")?;
                Ok(Fetched::Moved(to))
            }
            404 | 410 => Ok(Fetched::Gone),
            s => Err(anyhow!("GitHub answered {s} for {id}").into()),
        }
    }

    async fn links(&self, id: &IssueRef) -> Result<Links, SourceError> {
        let base = format!("{}/repos/{}/issues/{}", self.api, id.repo, id.number);
        let mut links = Links::default();
        for (suffix, into) in [
            ("dependencies/blocked_by", &mut links.blocked_by),
            ("sub_issues", &mut links.sub_issues),
        ] {
            let res = self
                .call(Req::get(format!("{base}/{suffix}?per_page=100")))
                .await?;
            match res.status {
                200 => {
                    for raw in res.body.as_array().into_iter().flatten() {
                        let repo = raw["repository_url"]
                            .as_str()
                            .and_then(|u| u.split_once("/repos/"))
                            .map_or(id.repo.as_str(), |(_, r)| r);
                        if let Some(n) = raw["number"].as_u64() {
                            into.push(IssueRef::new(repo, n));
                        }
                    }
                }
                // Not offered for this repository, none, or the issue moved.
                301 | 302 | 307 | 308 | 404 | 410 => {}
                s => return Err(anyhow!("GitHub answered {s} for the links of {id}").into()),
            }
        }
        Ok(links)
    }

    async fn write_back(&self, what: &WriteBack) -> Result<(), SourceError> {
        let WriteBack::DraftPullRequest { pr, note } = what;
        let pull = format!("{}/repos/{}/pulls/{}", self.api, pr.repo, pr.number);
        let res = self.call(Req::get(pull)).await?;
        match res.status {
            200 => {}
            404 | 410 => return Ok(()),
            s => return Err(anyhow!("GitHub answered {s} for {pr}").into()),
        }
        // A merged or closed pull request needs nothing from us.
        if res.body["state"] != "open" {
            return Ok(());
        }
        if res.body["draft"] != true {
            let node = res.body["node_id"]
                .as_str()
                .context("the pull request has no node id")?;
            let mutation = json!({
                "query": "mutation($id: ID!) { convertPullRequestToDraft(input: {pullRequestId: $id}) { pullRequest { isDraft } } }",
                "variables": { "id": node },
            });
            let out = self
                .call(Req::post(format!("{}/graphql", self.api), mutation))
                .await?;
            if out.status != 200 || !out.body["errors"].is_null() {
                return Err(anyhow!("could not make {pr} a draft: {}", out.body).into());
            }
        }
        let comment = format!(
            "{}/repos/{}/issues/{}/comments",
            self.api, pr.repo, pr.number
        );
        let out = self
            .call(Req::post(comment, json!({ "body": note })))
            .await?;
        if out.status != 201 {
            return Err(anyhow!("could not comment on {pr}: {}", out.status).into());
        }
        Ok(())
    }
}

/// Where to resume after listings that reached these points: the latest point
/// reached, unless a listing was cut short, which sets the earliest of those.
fn resume(reached: &[(String, bool)]) -> Option<String> {
    let cut = reached.iter().filter(|(_, t)| *t).map(|(s, _)| s).min();
    cut.or_else(|| reached.iter().map(|(s, _)| s).max())
        .cloned()
}

fn rate_limit_wait(res: &Res) -> Option<Duration> {
    let num = |k: &str| {
        res.headers
            .get(k)
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    if let Some(s) = num("retry-after") {
        return Some(Duration::from_secs(s.max(1)).min(MAX_WAIT));
    }
    if num("x-ratelimit-remaining") == Some(0) {
        let reset = num("x-ratelimit-reset")?;
        return Some(Duration::from_secs(reset.saturating_sub(epoch()) + 1).min(MAX_WAIT));
    }
    None
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The `rel="next"` target of a `Link` header.
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    })
}

/// `owner/repo#n` from a path like `/repos/owner/repo/issues/n`.
fn issue_path(location: &str) -> Option<IssueRef> {
    let (_, rest) = location.split_once("/repos/")?;
    let mut parts = rest.split('/');
    let (owner, repo, kind, n) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (kind == "issues").then_some(())?;
    Some(IssueRef::new(&format!("{owner}/{repo}"), n.parse().ok()?))
}

fn parse_issue(repo: &str, raw: &Value) -> anyhow::Result<Issue> {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let number = raw["number"].as_u64().context("an issue has no number")?;
    let closed = (raw["state"] == "closed").then(|| match raw["state_reason"].as_str() {
        Some("completed") => Closed::Completed,
        // Not planned, a duplicate, or a reason or none this version knows:
        // dropped is the reading that stops dependents instead of freeing them.
        _ => Closed::Dropped,
    });
    let names = |v: &Value, field: &str| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().or_else(|| x[field].as_str()).map(str::to_owned))
            .collect()
    };
    Ok(Issue {
        id: IssueRef::new(repo, number),
        title: text(&raw["title"]),
        body: text(&raw["body"]),
        closed,
        labels: names(&raw["labels"], "name"),
        milestone: raw["milestone"]["title"].as_str().map(str::to_owned),
        assignees: names(&raw["assignees"], "login"),
        kind: raw["type"]["name"].as_str().map(str::to_owned),
        author: text(&raw["user"]["login"]),
        url: text(&raw["html_url"]),
        updated_at: text(&raw["updated_at"]),
    })
}

fn parse_comment(repo: &str, raw: &Value) -> anyhow::Result<Comment> {
    let number = raw["issue_url"]
        .as_str()
        .and_then(|u| u.rsplit('/').next())
        .and_then(|n| n.parse().ok())
        .context("a comment names no issue")?;
    Ok(Comment {
        id: raw["id"].as_u64().context("a comment has no id")?,
        issue: IssueRef::new(repo, number),
        author: raw["user"]["login"].as_str().unwrap_or_default().to_owned(),
        body: raw["body"].as_str().unwrap_or_default().to_owned(),
        updated_at: raw["updated_at"].as_str().unwrap_or_default().to_owned(),
    })
}

#[cfg(test)]
mod tests;
