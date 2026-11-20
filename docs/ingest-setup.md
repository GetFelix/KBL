# Setting up ingest

Ingest is the worker that turns upstream issues (GitHub, Jira, Azure DevOps or GitLab) into work for
agents. This page takes you from nothing to a config that has been checked
against your real repository or project, before the worker ever imports
anything. The full config reference is in
[Running the ingest worker](work-tracker.md#running-the-ingest-worker) and
[Sources](work-tracker.md#sources).

You need a running tracker ([Running the tracker](work-tracker.md#running-the-tracker))
and its token file with an `ingest` entry: the ingest token is a key under
`"ingest"`, and its value names the worker.

## 1. Write a starter config

`kbl-tool source init` writes a config for one source. It asks only for what it
cannot guess, either as flags or, on a terminal, as questions.

```bash
# GitHub: issues labelled agent-ok in one repository
kbl-tool source init github --repo GetFelix/felix --out ingest.json

# Jira: issues one project's JQL picks, worked on in a GitHub repository
kbl-tool source init jira --site acme --api https://acme.atlassian.net \
  --project FEL --email kbl@acme.example --target-repo GetFelix/felix --out ingest.json
```

Without `--out` the config is printed. It will not overwrite a file unless you
pass `--force`. Options you may want:

| Flag | Default | What it sets |
|---|---|---|
| `--label` | `agent-ok` | The label that marks an issue for agents. It also names the rule. Jira uses it for the default `--jql`. |
| `--mode` | `admit` | `admit` imports a match as a root and queues its triage and augment work; `propose` puts it in Triage and queues nothing until a person accepts it. See below. |
| `--triage` | `run` | `skip` admits an already specified issue with no triage or augment item, so it is Ready to fix at once. Only with `--mode admit`. |
| `--cap` | `5` | The most open roots the rule may have. Matches over it wait upstream. |
| `--tracker-url` | `http://127.0.0.1:8790` | Where the tracker is. |
| `--token-env` | `GITHUB_TOKEN`, `JIRA_API_TOKEN` | The variable that will hold the source token. |
| `--jql` | `labels = "<label>"` | Jira only: the filter, combined with the project. |

### Mapping labels and milestones

Every imported item keeps its issue's labels and milestone, and reconcile updates them when upstream changes. A rule
can also use them when it admits an item:

```json
{ "name": "agent-ok", "repo": "GetFelix/felix", "labels_all": ["agent-ok"], "mode": "admit",
  "milestone_tiers": { "0.8": 0 },
  "label_types": { "regression": "bug", "perf": "perf" },
  "label_areas": { "disk": "storage" } }
```

`milestone_tiers` sets the tier (the lowest of this and `priority_labels` wins). `label_types` and `label_areas` set the
item's type and area from the first label upstream lists that the rule maps. The value must be in the team's vocabulary
(`vocabulary` in the standing rules), or the tracker refuses it and the item imports without it. Triage and a person
replace a mapped value. The tracker must be at format 19; until it is, the facts are written after the raise
([upgrades](upgrades.md)).

`label_needs` maps a label to something the work needs, such as `"needs-gpu": "gpu"`. An issue needs every need its
labels map to, and an agent is handed it only when it declares all of them
([work tracker](work-tracker.md#what-the-work-needs-and-what-an-agent-can-do)). The needs must be in the vocabulary's
`needs` list, or the tracker refuses the whole set and the item imports without it. Triage and a person replace a
mapped set, and a label that is later removed upstream does not remove a need already set. This needs format 20.

### What admit and propose do

With `admit`, a match is imported as a root and its children come with it: a triage item and an augment item are
queued first, so agents are handed that work before any fix. The root becomes ready to fix once a person accepts the
triage, or a [triage policy](work-tracker.md#accepting-routine-triage-by-policy) rule does. That is why a freshly admitted root shows `has_children: true` and agents seem to be doing something other
than the fix. The dashboard shows the root's stage (Triage, Ready to fix, Held, Needs you) and a chip for its triage
child.

With `propose`, the match waits in Triage for a person before anything is queued.

With `admit` and `"triage": "skip"`, a match is imported already accepted and ready to fix, with no triage or augment
item. Use it for issues that are already well specified, such as a curated milestone or an `agent-ok` label a person
applies after writing the acceptance. The dashboard shows such a root as Ready to fix with "triage skipped by rule
<name>", and `kbl-ingest check` says the rule "admits straight to ready". Skipping also skips augment, so nothing adds
missing context before the fix; an agent that needs it asks a question. `skip` is refused with `propose`, since a
proposed match waits for a person. The default is `"triage": "run"`.

```json
{ "name": "curated", "repo": "GetFelix/felix", "milestone": "0.8", "mode": "admit", "triage": "skip" }
```

The file is plain JSON, which has no comments, so the fields are explained in
[the reference](work-tracker.md#running-the-ingest-worker). The starter holds
no secrets: it names the environment variables that will.

## 2. Create the credentials

Ingest needs one token for the tracker and one for the source. Put them in the
environment variables the config names.

```bash
export KBL_INGEST_TOKEN=...   # the tracker's ingest token
export GITHUB_TOKEN=...       # or JIRA_API_TOKEN
```

What the source credential needs:

| Source | Always | Only when a rule sets it |
|---|---|---|
| GitHub | Read access to issues and pull requests on each rule's `repo` | `open_prerequisites`: issues write on that repo. Drafting an agent's pull request after its issue is dropped: pull requests write. Re-running a failed check on the [flake list](work-tracker.md#known-flaky-checks): Actions write (the `actions: write` App permission). The job log a [failed check](work-tracker.md#failed-checks-become-fix-work) reports: Actions read; without it the check's own output is used |
| Jira | Browse projects on each rule's project | `comment`: add comments. `link_pr`: link issues. `transition_on_pr` or `transition_on_merge`: transition issues |
| Azure DevOps | Work Items (Read) on each rule's project | `comment`, `link_pr`, `transition_on_pr` or `transition_on_merge`: Work Items (Read & write) |
| GitLab | A token with `read_api` and a role in each rule's project | `comment`, `transition_on_pr`, `transition_on_merge`: the `api` scope and the Reporter role or higher. `open_prerequisites`: `api` and Guest or higher |

A fine-grained GitHub token or a GitHub App installation must be granted each
repository the rules name. For an App, see
[the credential options](work-tracker.md#running-the-ingest-worker); for Jira
OAuth, the token needs the `read:me` scope so ingest can tell its own comments
from others.

## 3. Check it

```bash
kbl-ingest check --config ingest.json
```

`check` reads only. It never creates, changes or comments on anything, in the
tracker or upstream: the connection to the source refuses any request that is
not a GET, and the tracker is called only to list its roots and read ingest's
saved state. (An App or Entra credential still mints its token, which is how
it is tested.) The one exception is Azure DevOps, which takes a work item query
as a POST: `check` lets through exactly that query endpoint, with a `SELECT`
query as its only body, and refuses every other request. Run it as often as you
like, and again after every config change.

It does four things:

1. **Validates the config** with the same parser the worker uses, so an unknown
   key, a rule with no `repo`, two rules with one name, a Jira rule with no
   `jql`, or conflicting sign-in settings is an error with the worker's own
   message.
2. **Tests the credentials.** It names each environment variable and file, says
   whether it is set and readable, and then asks the source who the credential
   is. Secret values are never printed. For GitLab it also reports the token's
   scopes and expiry (GitLab 15.5 or later), and an expired or soon-to-expire
   token is flagged.
3. **Reports permissions per rule.** For each repository or project it says
   whether the credential can read it and whether it holds the write access the
   rule's options need. A missing read, or a missing write that an option
   needs, is an error. Pull request write on GitHub is only a warning, since it
   is used only to draft a pull request after its issue is dropped, and a
   credential whose rights GitHub does not report is a warning too.
   For Azure DevOps, a project read and, where a rule writes back, the work item
   edit right from the permission service; a personal access token's own scopes
   are not visible to the API, so when the service will not say, `check` warns.
   For GitLab, the project read, the token's `api` scope where a rule writes
   back, and the token's role in the project (Reporter for comments and
   transitions).
4. **Dry-runs the first poll.** It lists the open issues in the rules'
   repositories and gives each to the first rule that matches, as the worker
   does, then applies each rule's `cap` to the roots the tracker already has. For each rule it shows what would be
   imported, what would wait over the cap, and what is already imported.

```text
ok    config: ingest.json parses: 1 rule(s) for github
ok    tracker: KBL_INGEST_TOKEN is set
ok    github: GITHUB_TOKEN is set
ok    github: the credential signs in as octocat
ok    repo GetFelix/felix: the credential can read it
warn  rule agent-ok: no pull requests: write on GetFelix/felix: when an issue is dropped upstream, ingest cannot make the agent's open pull request a draft
ok    tracker: http://127.0.0.1:8790 answers the ingest token (0 imported root(s))

Dry run (nothing is written): what the first poll would do

rule agent-ok (GetFelix/felix, admit, cap 5, 0 open now): 5 to import, 2 over the cap, 0 already imported
  import GetFelix/felix#812 (tier 2, work in GetFelix/felix) Reads miss a fresh write
  ...
  waits GetFelix/felix#830 Compaction stalls

14 open issue(s) match no rule.

No errors.
```

The exit status is 0 when there are no errors and 1 otherwise, so it can gate a
deploy. `--json` prints the same report as one JSON object (`ok`, `findings`
with `level`, `scope` and `message`, `rules` with every issue listed, and
`unmatched`).

### What the dry run does not show

- It lists as a first poll does: open issues only, and a GitHub repository is
  read in full up to the worker's page limit. A large repository costs the same
  requests as the worker's first poll.
- It leaves out the issues ingest itself opened for prerequisites, as the
  worker does, and shows no comments, links or pull requests.
- If ingest has run before, issues it already imported (from this source only)
  are counted as already imported, not listed again. If the tracker's roots
  cannot be read, `check` warns and treats every issue as new.

## 4. Run ingest

```bash
kbl-ingest --config ingest.json --once   # one poll, then exit
kbl-ingest --config ingest.json          # keep polling
```

`--once` is a good first run: the roots appear in the dashboard's Triage or
Ready columns. If the result is not what `check` showed, run `check` again with
`--json` and compare.

### Request budget

Every vendor has an hourly API limit that all of a credential's users share:
5,000 requests an hour for a GitHub token or App installation, for example.
Ingest spends only part of it, so a worker cannot use up a limit that `gh`, CI
and other people's tools need.

- **`budget_share`** (default `0.5`) is the part of the vendor's reported hourly
  limit this worker may spend. **`budget_per_hour`** sets a number instead.
  GitHub and GitLab report their limit in response headers; for Jira and Azure
  DevOps, which do not, the worker assumes 5,000 an hour unless
  `budget_per_hour` is set. Set it there to what your site allows.
- Token requests count too: a GitHub App's installation-token mint and
  installation lookup, and an Azure DevOps Entra sign-in, are charged to the
  same budget and wait when the vendor has asked for a pause.
- A fifth of the budget is kept back for write-backs and checks. Polling and
  the reconcile pass cannot spend it. When the rest runs low the next poll waits.
  A third of the kept-back part is the wake share: a webhook delivery may start
  a poll ahead of its interval only while that share can pay for one, and it
  never touches what write-backs keep. A delivery that finds the share spent
  waits for the interval.
- A worker that serves a connection takes its budget from the tracker, not from
  these settings: the pool's hourly budget (`PUT /v1/org/budgets/{pool}`) times
  the sources the worker holds leases on over the sources the pool serves, and
  the pool's kept-back and wake numbers in place of the fifth and the third.
  The share moves when leases do. A pool with no budget set falls back to the
  settings above.
- **`poll_seconds`** (default 300) is a minimum, and never below 60: a smaller
  value polls every 60 seconds and logs a warning at start. A webhook still
  wakes a poll sooner. When a poll reads more than
  the budget affords at that rate, the gap grows to fit, and the worker logs
  the gap and the budget left after each poll.
- **`reconcile_hours`** (default 24) is how long one pass over every open
  imported issue takes. Each poll re-reads its share of them instead of all of
  them every few polls.
- When a vendor says to wait (GitHub's `403` secondary limit or `429`,
  `Retry-After` and `X-RateLimit-*` on GitHub, Jira, Azure DevOps and GitLab),
  the worker sends no request to it until the wait is over, capped at an hour.
  A worker that serves a connection also tells the tracker, which holds the
  wait for the whole budget pool: every worker in the pool stops, within about
  ten seconds, even those the vendor has not yet refused.

Guidance by vendor:

| Vendor | Limit ingest sees | What to set |
| --- | --- | --- |
| GitHub | `X-RateLimit-*` headers (5,000 an hour for a token or App installation) | `budget_share` 0.5 on a personal token, since GitHub counts per account and your own `gh` use shares it; up to 0.9 for a bot account or App |
| GitLab | `RateLimit-*` headers | `budget_share` 0.5 on a personal token; up to 0.9 for a bot or group token |
| Jira Cloud | No hourly count; it answers 429 with `Retry-After` when it throttles | `budget_per_hour` if your site is throttled below 5,000 an hour |
| Azure DevOps | No hourly count; it delays or answers 429 when a user's usage is high | `budget_per_hour` if your organization is throttled below 5,000 an hour |

**A config with `reconcile_every` is refused.** Replace it with
`reconcile_hours`; the error prints a starting value from your `poll_seconds`.

**Use a separate token for ingest.** A limit belongs to the credential, so a
personal token shared with ingest also pays for your own `gh` and CI calls. A
worker run locally with a personal token that polled every minute over many
imported roots has used a whole account's hour three times. Give ingest its own
token, or a GitHub App, and keep the budget at the default.

`kbl-ingest check` prints the vendor's remaining requests, what this worker may
spend, and the requests an hour the config is projected to need (an estimate
from the repos, the open imported roots and the intervals). It warns when the
projection is over budget.

This is each worker's own budget. Splitting one limit between several workers
is not done yet: set `budget_per_hour` on each so they sum to your share.

### Running more than one worker

Set `"group"` under `"tracker"` to the worker group an org admin named in a
`connection_set`. The worker then holds the config route open on its own task
and takes the connection's sources by lease: it polls and writes back only to
the sources it holds. Each lease runs for 90 s and the worker renews all of
them every 30 s. If no renew has succeeded for 60 s, it makes no upstream call
until one does, and cancels any request still running, so a worker cut off from
the tracker stops before another can take its sources. Time the machine spends
asleep counts. A lease is free to take 30 s after it expires, so the trackers'
clocks must agree within 30 s. When the worker stops, it releases its leases.

A repo in the worker's rules that the connection does not name is not polled:
the worker prints a warning naming it when the config arrives. Names match the
way the vendor compares them, so `Owner/Repo` and `owner/repo` are one GitHub,
GitLab or Azure DevOps repo; Jira project keys must match exactly.

Leave `"group"` unset, or name a group no connection uses, and the worker polls
every configured repo as before.

Each worker still keeps one state per adapter in this release, so run one
worker per connection until per-source state ships.

## Quick start

The development stack's [quick start](../README.md#quick-start) has the tracker
and its ingest token ready. From the checkout:

```bash
cargo build --release -p kbl-ingest
node dev/ingest.mjs --repo OWNER/NAME --token-file ~/.github-token --label agent-ok
```

`dev/ingest.mjs` writes `dev/state/ingest.json` for that repository and label,
runs `kbl-ingest check` against the tracker, and then runs the worker on your
machine until you press Ctrl-C. Without `--token-file` it reads `GITHUB_TOKEN`.
`--check-only` stops after the check and `--once` polls once.

## When something fails

| You see | Likely cause |
|---|---|
| `GITHUB_TOKEN is not set or is empty` | The variable is not exported in the shell that runs `check`. |
| `GitHub rejected the credential (401)` | The token is wrong, expired or revoked. |
| `not found, or the credential cannot see it` | GitHub answers 404 for a repository that does not exist and one the token cannot see. Grant the token that repository. |
| `issues: write ... which the credential lacks` | A rule sets `open_prerequisites` and the token has read access only. |
| `own_logins` is empty | An App installation token has no user. List the App's bot login, such as `kbl[bot]`, so ingest skips its own comments. |
| `Jira refused the credential` | Wrong email or token, or an OAuth token without `read:me`. |
| `GitLab rejected the token (401)` | The token is wrong, expired or revoked. |
| `the token lacks the api scope` | A GitLab rule comments, transitions or opens prerequisites. Give the token `api`, not only `read_api`. |
| `the credential may not edit work items` | An Azure DevOps rule writes back. Grant the credential Work Items (Read & write) on the project. |
| `the source answered 400 listing its issues` | For Jira, the project's JQL is one Jira rejects. Run it in Jira's issue search. |

## Changing a rule

Edit the rules and restart ingest. A rule that is new, or whose selector changed (repo or project, labels, milestone, assignee, type, JQL or saved query), makes its repo or project read in full on the next poll, so issues that already match the new rule and are older than the saved cursor are imported (up to the rule's cap). Rules you did not touch keep their cursor. Rules edited in the same restart that first runs this version are recorded as they are and not re-read; restart once on the old rules first, or clear the saved state. Changing only options such as `cap`, `mode` or `comment` re-reads nothing. A `label_needs` that is new or changed for a rule also reads its repo in full, and each issue the rule already imported is mapped again on that poll, so the needs reach existing issues and not only ones whose labels change next. Writing the same set again is a no-op, so a restart or a repeated poll adds nothing. Removing a mapping removes no need already set.
