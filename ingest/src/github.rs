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

use kbl_core::work::{
    FailedCheck, MAX_CHECK_NAME, MAX_CHECK_URL, PrChecks, PrPhase, PrReview, PrSnapshot,
};

use crate::source::{
    Changes, Closed, Comment, Cursor, Fetched, Issue, IssueRef, Links, PullRequest, Source,
    SourceError, WriteBack,
};

pub(crate) mod transport;

pub(crate) use transport::{Req, Res, Transport};

/// Pages followed in one listing before the cursor stops where they ended.
const MAX_PAGES: usize = 20;
const REVIEWS_PER_PAGE: usize = 100;
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
                // This repo is not ours to read (gone, renamed, no access): the
                // caller skips it so the other repos keep advancing.
                s @ (403 | 404) => {
                    return Ok(Listing {
                        denied: Some(s),
                        ..Listing::default()
                    });
                }
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
                        ..Listing::default()
                    });
                }
            }
        }
        // The page limit stopped it; the cursor resumes where the pages ended.
        Ok(Listing {
            items: out,
            ..Listing::default()
        })
    }
}

impl<T: Transport> Github<T> {
    /// Every commit status on `sha`, paged. `incomplete` is set when fewer
    /// came back than GitHub counts. `None` when the call failed.
    async fn commit_statuses(&self, repo: &str, sha: &str) -> Result<Option<Value>, SourceError> {
        let mut all: Vec<Value> = Vec::new();
        let mut total = 0;
        for page in 1..=MAX_PAGES {
            let url = format!(
                "{}/repos/{repo}/commits/{sha}/status?per_page=100&page={page}",
                self.api
            );
            let res = self.call(Req::get(url)).await?;
            if res.status != 200 {
                return Ok(None);
            }
            total = res.body["total_count"].as_u64().unwrap_or(0);
            let got = res.body["statuses"].as_array().cloned().unwrap_or_default();
            let empty = got.is_empty();
            all.extend(got);
            if empty || all.len() as u64 >= total {
                break;
            }
        }
        Ok(Some(json!({
            "statuses": all,
            "incomplete": (all.len() as u64) < total,
        })))
    }

    /// Every review on a pull request, oldest first, paged. `None` when a call
    /// failed or the list was cut short: a later review can undo an approval,
    /// so part of the list is not read as the whole.
    async fn reviews(&self, pull: &str) -> Result<Option<Value>, SourceError> {
        let mut all: Vec<Value> = Vec::new();
        for page in 1..=MAX_PAGES {
            let url = format!("{pull}/reviews?per_page={REVIEWS_PER_PAGE}&page={page}");
            let res = self.call(Req::get(url)).await?;
            if res.status != 200 {
                return Ok(None);
            }
            let got = res.body.as_array().cloned().unwrap_or_default();
            let last = got.len() < REVIEWS_PER_PAGE;
            all.extend(got);
            if last {
                return Ok(Some(Value::Array(all)));
            }
        }
        Ok(None)
    }

    /// The checks the base branch requires, when the credential may read its
    /// protection. `None` means unknown, and then every check counts.
    async fn required_checks(
        &self,
        repo: &str,
        base: Option<&str>,
    ) -> Result<Option<Vec<String>>, SourceError> {
        let Some(base) = base else { return Ok(None) };
        let url = format!(
            "{}/repos/{repo}/branches/{base}/protection/required_status_checks",
            self.api
        );
        let res = self.call(Req::get(url)).await?;
        if res.status != 200 {
            return Ok(None);
        }
        let mut names: Vec<String> = res.body["contexts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str().map(str::to_owned))
            .collect();
        names.extend(
            res.body["checks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["context"].as_str().map(str::to_owned)),
        );
        Ok(Some(names))
    }
}

#[derive(Default)]
struct Listing {
    items: Vec<Value>,
    /// The status that said this repo cannot be listed.
    denied: Option<u16>,
}

impl<T: Transport> Source for Github<T> {
    fn name(&self) -> &'static str {
        "github"
    }

    fn own_logins(&self) -> Vec<String> {
        self.own.clone()
    }

    async fn changed(
        &self,
        repos: &[String],
        cursors: &BTreeMap<String, Cursor>,
    ) -> Result<Changes, SourceError> {
        let mut out = Changes::default();
        for repo in repos {
            let old = cursors.get(repo).cloned().unwrap_or_default();
            // Only this poll's validators are kept, so old `since` values do not pile up.
            let mut etags = BTreeMap::new();
            let state = if old.issues_since.is_some() {
                "all"
            } else {
                "open"
            };
            let mut url = format!(
                "{}/repos/{repo}/issues?state={state}&sort=updated&direction=asc&per_page=100",
                self.api
            );
            if let Some(since) = &old.issues_since {
                url.push_str(&format!("&since={since}"));
            }
            let listing = self.pages(url, &old.etags, &mut etags).await?;
            if let Some(code) = listing.denied {
                eprintln!("kbl-ingest: {repo} answered {code}; skipping it this poll");
                out.denied.insert(repo.clone(), code);
                continue;
            }
            let mut last = None;
            for raw in &listing.items {
                // Pull requests count toward the cursor too, or a listing of
                // nothing else would never move it.
                if let Some(at) = raw["updated_at"].as_str() {
                    last = Some(at.to_owned());
                }
                // The issues endpoint lists pull requests too.
                if raw.get("pull_request").is_some() {
                    continue;
                }
                match parse_issue(repo, raw) {
                    Ok(issue) => out.issues.push(issue),
                    Err(e) => eprintln!("kbl-ingest: skipped an item of {repo}: {e:#}"),
                }
            }
            let issues_since = last.or_else(|| old.issues_since.clone());

            let mut comments_last = None;
            if let Some(since) = &old.comments_since {
                let url = format!(
                    "{}/repos/{repo}/issues/comments?sort=updated&direction=asc&per_page=100&since={since}",
                    self.api
                );
                let listing = self.pages(url, &old.etags, &mut etags).await?;
                if let Some(code) = listing.denied {
                    eprintln!("kbl-ingest: comments of {repo} answered {code}; skipping them");
                    out.denied.insert(repo.clone(), code);
                    continue;
                }
                for raw in &listing.items {
                    if let Some(at) = raw["updated_at"].as_str() {
                        comments_last = Some(at.to_owned());
                    }
                    match parse_comment(repo, raw) {
                        Ok(c) => out.comments.push(c),
                        Err(e) => eprintln!("kbl-ingest: skipped a comment of {repo}: {e:#}"),
                    }
                }
            }
            // The first poll sees no comments; later ones start where the issues
            // were, since a comment bumps its issue's update time.
            let comments_since = match &old.comments_since {
                Some(prev) => comments_last.or_else(|| Some(prev.clone())),
                None => issues_since.clone(),
            };
            out.cursors.insert(
                repo.clone(),
                Cursor {
                    issues_since,
                    comments_since,
                    etags,
                },
            );
        }
        out.issues.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        out.comments.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        Ok(out)
    }

    async fn pull_request(&self, id: &IssueRef) -> Result<Option<PullRequest>, SourceError> {
        let base = format!("{}/repos/{}/pulls/{}", self.api, id.repo, id.number);
        let res = self.call(Req::get(base.clone())).await?;
        match res.status {
            200 => {}
            404 | 410 => return Ok(None),
            s => return Err(anyhow!("GitHub answered {s} for {id}").into()),
        }
        let pull = &res.body;
        let author = pull["user"]["login"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let head_ref = pull["head"]["ref"].as_str().unwrap_or_default().to_owned();
        let body = pull["body"].as_str().unwrap_or_default().to_owned();
        let created_ms = pull["created_at"].as_str().and_then(parse_utc_ms);
        // Only a request our credential opened is read further: the name
        // comes from an agent, and its checks and reviews steer a person.
        if !self.own.iter().any(|o| o.eq_ignore_ascii_case(&author)) {
            return Ok(Some(PullRequest {
                author,
                head_ref,
                body,
                created_ms,
                status: None,
            }));
        }
        // A merge counts only into the repo's default branch and by someone
        // other than us: agents never merge their own work.
        let into_default = pull["base"]["repo"]["default_branch"]
            .as_str()
            .is_some_and(|d| pull["base"]["ref"].as_str() == Some(d));
        let by_us = pull["merged_by"]["login"]
            .as_str()
            .is_some_and(|m| self.own.iter().any(|o| o.eq_ignore_ascii_case(m)));
        let phase = match (pull["state"].as_str(), pull["merged"].as_bool()) {
            (_, Some(true)) if into_default && !by_us => PrPhase::Merged,
            (Some("open"), _) => PrPhase::Open,
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
            return Ok(Some(PullRequest {
                author,
                head_ref,
                body,
                created_ms,
                status: Some(status),
            }));
        }
        let asked = pull["requested_reviewers"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
            || pull["requested_teams"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
        status.review = if let Some(reviews) = self.reviews(&base).await? {
            review_state(&reviews, &author, asked)
        } else if asked {
            PrReview::Requested
        } else {
            PrReview::Unreviewed
        };
        if let Some(sha) = pull["head"]["sha"].as_str() {
            let required = self
                .required_checks(&id.repo, pull["base"]["ref"].as_str())
                .await?;
            let runs = self
                .call(Req::get(format!(
                    "{}/repos/{}/commits/{sha}/check-runs?per_page=100",
                    self.api, id.repo
                )))
                .await?;
            let statuses = self.commit_statuses(&id.repo, sha).await?;
            // A call that failed is unknown, never "no checks".
            let runs = (runs.status == 200).then_some(&runs.body);
            (status.checks, status.failing) =
                check_state(runs, statuses.as_ref(), &self.own, required.as_ref());
        } else {
            status.checks = PrChecks::Pending;
        }
        Ok(Some(PullRequest {
            author,
            head_ref,
            body,
            created_ms,
            status: Some(status),
        }))
    }

    async fn issue_comments(&self, id: &IssueRef) -> Result<Vec<Comment>, SourceError> {
        let url = format!(
            "{}/repos/{}/issues/{}/comments?per_page=100",
            self.api, id.repo, id.number
        );
        let listing = self
            .pages(url, &BTreeMap::new(), &mut BTreeMap::new())
            .await?;
        if let Some(code) = listing.denied {
            eprintln!("kbl-ingest: the comments of {id} answered {code}; skipping them");
        }
        Ok(listing
            .items
            .iter()
            .filter_map(|raw| match parse_comment(&id.repo, raw) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("kbl-ingest: skipped a comment of {id}: {e:#}");
                    None
                }
            })
            .collect())
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
        // The request is named by an agent, so only one our credential's
        // identities opened may be touched.
        let author = res.body["user"]["login"].as_str().unwrap_or_default();
        if !self.own.iter().any(|o| o.eq_ignore_ascii_case(author)) {
            eprintln!("kbl-ingest: {pr} was not opened by us; leaving it alone");
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

/// The review state from a pull request's reviews: each reviewer's latest
/// verdict counts, and only from people with write access to the repo.
fn review_state(reviews: &Value, author: &str, asked: bool) -> PrReview {
    let mut latest: BTreeMap<String, &str> = BTreeMap::new();
    for r in reviews.as_array().into_iter().flatten() {
        let who = r["user"]["login"].as_str().unwrap_or_default();
        let may = matches!(
            r["author_association"].as_str(),
            Some("OWNER" | "MEMBER" | "COLLABORATOR")
        );
        if !may || who.eq_ignore_ascii_case(author) {
            continue;
        }
        match r["state"].as_str() {
            Some(v @ ("APPROVED" | "CHANGES_REQUESTED")) => {
                latest.insert(who.to_ascii_lowercase(), v);
            }
            // A dismissed review no longer counts; a comment changes nothing.
            Some("DISMISSED") => {
                latest.remove(&who.to_ascii_lowercase());
            }
            _ => {}
        }
    }
    if latest.values().any(|v| *v == "CHANGES_REQUESTED") {
        PrReview::ChangesRequested
    } else if latest.values().any(|v| *v == "APPROVED") {
        PrReview::Approved
    } else if asked {
        PrReview::Requested
    } else {
        PrReview::Unreviewed
    }
}

enum Outcome {
    Pass,
    Fail,
    Pending,
}

/// Green, pending or red from the check runs and commit statuses on the head
/// commit. Only the `required` names count when they are known, and one with
/// no result yet keeps the request pending. A listing longer than what was
/// read is pending too, since green cannot be shown.
fn check_state(
    runs: Option<&Value>,
    statuses: Option<&Value>,
    own: &[String],
    required: Option<&Vec<String>>,
) -> (PrChecks, Option<FailedCheck>) {
    // An empty list means the protection could not tell us, not "nothing needed".
    let required = required.filter(|r| !r.is_empty());
    let list = runs
        .and_then(|r| r["check_runs"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut results: Vec<(String, Outcome, String)> = Vec::new();
    for r in &list {
        // Only an App writes check runs. One run by our own App (`<slug>[bot]` in
        // `own_logins`) is skipped like our own status below.
        let app = r["app"]["slug"].as_str().unwrap_or_default();
        if !app.is_empty()
            && own
                .iter()
                .any(|o| o.eq_ignore_ascii_case(&format!("{app}[bot]")))
        {
            continue;
        }
        let outcome = match r["conclusion"].as_str() {
            Some("failure" | "timed_out" | "action_required" | "startup_failure") => Outcome::Fail,
            Some("success" | "neutral" | "skipped") => Outcome::Pass,
            // Queued, running or cancelled: not green yet.
            _ => Outcome::Pending,
        };
        results.push((
            r["name"].as_str().unwrap_or_default().to_owned(),
            outcome,
            r["html_url"].as_str().unwrap_or_default().to_owned(),
        ));
    }
    for st in statuses
        .and_then(|s| s["statuses"].as_array())
        .into_iter()
        .flatten()
    {
        // A status our own credential wrote says nothing about the code: an
        // agent that can reach it could forge a green one.
        let by = st["creator"]["login"].as_str().unwrap_or_default();
        if own.iter().any(|o| o.eq_ignore_ascii_case(by)) {
            continue;
        }
        let outcome = match st["state"].as_str() {
            Some("success") => Outcome::Pass,
            Some("failure" | "error") => Outcome::Fail,
            _ => Outcome::Pending,
        };
        results.push((
            st["context"].as_str().unwrap_or_default().to_owned(),
            outcome,
            st["target_url"].as_str().unwrap_or_default().to_owned(),
        ));
    }
    let counted: Vec<&(String, Outcome, String)> = results
        .iter()
        .filter(|(name, _, _)| required.is_none_or(|req| req.iter().any(|n| n == name)))
        .collect();
    // A listing that failed, or is longer than what was read, cannot show green.
    let mut pending = runs
        .is_none_or(|r| r["total_count"].as_u64().unwrap_or(0) > list.len() as u64)
        || statuses.is_none_or(|s| s["incomplete"].as_bool().unwrap_or(false));
    if let Some(req) = required {
        pending |= req
            .iter()
            .any(|n| !counted.iter().any(|(name, _, _)| name == n));
    }
    let mut failing: Option<FailedCheck> = None;
    for (name, outcome, url) in &counted {
        match outcome {
            Outcome::Fail => {
                let name = cut_bytes(name, MAX_CHECK_NAME);
                // The first by name, so the same failure reads the same each poll.
                if failing.as_ref().is_none_or(|f| name < f.name) {
                    failing = Some(FailedCheck {
                        name,
                        url: cut_bytes(url, MAX_CHECK_URL),
                    });
                }
            }
            Outcome::Pending => pending = true,
            Outcome::Pass => {}
        }
    }
    if failing.is_some() {
        (PrChecks::Red, failing)
    } else if counted.is_empty() && !pending {
        (PrChecks::NoChecks, None)
    } else if pending {
        (PrChecks::Pending, None)
    } else {
        (PrChecks::Green, None)
    }
}

/// Milliseconds since the epoch from `YYYY-MM-DDTHH:MM:SSZ`, as GitHub writes times.
fn parse_utc_ms(t: &str) -> Option<u64> {
    let b = t.as_bytes();
    if b.len() != 20 || b[19] != b'Z' || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, s) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days from the civil date (proleptic Gregorian).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 24 + h) * 60 + mi) * 60 + s)
        .ok()
        .map(|x| x * 1000)
}

fn cut_bytes(s: &str, max: usize) -> String {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
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
        // No reason is how GitHub records issues closed before it kept one,
        // and a plain close is a completion.
        Some("completed") | None => Closed::Completed,
        // Not planned, a duplicate, or a reason this version does not know:
        // dropped is the reading that stops dependents instead of freeing them.
        Some(_) => Closed::Dropped,
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
