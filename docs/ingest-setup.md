# Setting up ingest

Ingest is the worker that turns upstream issues (GitHub or Jira) into work for
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
| `--mode` | `admit` | `admit` makes a match a root at once; `propose` puts it in Triage until a person accepts it. |
| `--cap` | `5` | The most open roots the rule may have. Matches over it wait upstream. |
| `--tracker-url` | `http://127.0.0.1:8790` | Where the tracker is. |
| `--token-env` | `GITHUB_TOKEN`, `JIRA_API_TOKEN` | The variable that will hold the source token. |
| `--jql` | `labels = "<label>"` | Jira only: the filter, combined with the project. |

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
| GitHub | Read access to issues and pull requests on each rule's `repo` | `open_prerequisites`: issues write on that repo. Drafting an agent's pull request after its issue is dropped: pull requests write |
| Jira | Browse projects on each rule's project | `comment`: add comments. `link_pr`: link issues. `transition_on_pr` or `transition_on_merge`: transition issues |

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
saved state. (An App credential still mints its installation token, which is
how it is tested.) Run it as often as you like, and again after every config
change.

It does four things:

1. **Validates the config** with the same parser the worker uses, so an unknown
   key, a rule with no `repo`, two rules with one name, a Jira rule with no
   `jql`, or conflicting sign-in settings is an error with the worker's own
   message.
2. **Tests the credentials.** It names each environment variable and file, says
   whether it is set and readable, and then asks the source who the credential
   is. Secret values are never printed.
3. **Reports permissions per rule.** For each repository or project it says
   whether the credential can read it and whether it holds the write access the
   rule's options need. A missing read, or a missing write that an option
   needs, is an error. Pull request write on GitHub is only a warning, since it
   is used only to draft a pull request after its issue is dropped, and a
   credential whose rights GitHub does not report is a warning too.
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
- It does not show the issues ingest itself opened for prerequisites, or
  comments, links and pull requests.
- If ingest has run before, issues it already imported are counted as
  already imported, not listed again.

## 4. Run ingest

```bash
kbl-ingest --config ingest.json --once   # one poll, then exit
kbl-ingest --config ingest.json          # keep polling
```

`--once` is a good first run: the roots appear in the dashboard's Triage or
Ready columns. If the result is not what `check` showed, run `check` again with
`--json` and compare.

## Quick start

The development stack's quick start does not run ingest yet. To see items
arrive there, run steps 1 to 4 against a repository you own, with the quick
start's tracker URL and ingest token.

## When something fails

| You see | Likely cause |
|---|---|
| `GITHUB_TOKEN is not set or is empty` | The variable is not exported in the shell that runs `check`. |
| `GitHub rejected the credential (401)` | The token is wrong, expired or revoked. |
| `not found, or the credential cannot see it` | GitHub answers 404 for a repository that does not exist and one the token cannot see. Grant the token that repository. |
| `issues: write ... which the credential lacks` | A rule sets `open_prerequisites` and the token has read access only. |
| `own_logins` is empty | An App installation token has no user. List the App's bot login, such as `kbl[bot]`, so ingest skips its own comments. |
| `Jira refused the credential` | Wrong email or token, or an OAuth token without `read:me`. |
| `the source answered 400 listing its issues` | For Jira, the project's JQL is one Jira rejects. Run it in Jira's issue search. |
