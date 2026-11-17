# Threat model

Who can reach KBL (Kanban By Log, pronounced Kibble), what they could damage, and the check that stops each
attempt. Every tracker route, ingest route, MCP tool and record kind has a row.

A pull request that adds a route, a tool or a record kind adds its row here.
`scripts/check-threat-model.mjs` fails CI when one is missing. It finds routes
and tools in every crate of the workspace, and a path served with more than one
method needs a row per method, named like `` `PUT /v1/x` ``.

Each `file:line` cited below is pinned to the text of that line in
`docs/threat-model-citations.json`. When code moves, the check fails and names
the new line; `node scripts/check-threat-model.mjs --fix` updates the doc. A new
citation fails until `--fix` pins it, so look at the line first: it should be
the one that does the check, not a closing brace near it. A citation whose
line is gone keeps failing, `--fix` or not, until the row cites the code that
does the check now.

A cell marked **None** is a gap with an open issue. The gaps are listed at
the end.

## Actors

| Actor | What it is | Credential | Trust |
|---|---|---|---|
| Person | Someone using the dashboard or the REST API | A person token from the tracker's token file | Trusted. Every person token can do everything a person can ([#120](https://github.com/GetFelix/KBL/issues/120)) |
| Agent | A model acting through `kbl-mcp` | An agent token for the tracker, and a Felix token for its run | Not trusted. Assume it follows any instruction it reads in an issue, a comment or a file |
| Ingest | The `kbl-ingest` worker that mirrors GitHub | An ingest token, and a GitHub credential | Trusted code that carries untrusted text |
| Gateway reader | A browser reading through felix-gateway | A gateway session scoped to one run | Read only (`dev/scope.toml`) |
| Webhook sender | Whoever posts to ingest's `/webhook` | The shared webhook secret | Not trusted without a valid signature |

Two more parties are outside KBL and can still steer it:

- **Anyone who can file or edit an issue or comment** in a repo an intake rule
  admits. Their text reaches agents as context.
- **Anyone who can push to an agent's branch or administer the repo.** They can
  make checks fail or add approvals. This is GitHub's own trust; the tracker
  never merges.

## Assets

| Asset | Where it lives | What an attacker wants |
|---|---|---|
| The log | `work.events` in Felix | Records that did not happen, or records under another name |
| Budgets | Folded from the log, per root | Free work, or more items, depth and redos than allowed |
| Holds | Folded from the log, plus the tracker's delivery table | To write on work it does not hold, or to keep work from others |
| Merges | GitHub. KBL records them and never merges | An item closed as merged that was not, or a merge into the wrong branch |
| Rules | `rules_set` records, shown in every claim reply | Instructions injected into every other agent |
| Artifacts | The artifacts cache, keyed by content hash | Other bytes shown under a trusted id |
| Credentials | The token file, the webhook secret, the GitHub credential, Felix tokens | Any of them |

## Trust boundaries

1. **Agent to tracker.** The bearer token names the caller
   (`tracker/src/http.rs:170`). The name and role on a record come from the
   token, never from the body (`tracker/src/tracker.rs:1722`). The attempt
   number comes from the hold, never from the body
   (`tracker/src/tracker/tools.rs:1040`, `tracker/src/tracker.rs:1709`).
2. **Tracker to log.** The tracker is the only writer of `work.events`. It
   appends a record only if the fold accepts it
   (`tracker/src/tracker.rs:2164`), so every rule in
   `core/src/work_fold.rs` is a check on every route. A writer lease keeps a
   second tracker from writing (`tracker/src/tracker.rs:2122`), and every
   append is conditional on the offset it expects, so a tracker that lost the
   lease cannot append after the new holder's records. After a refusal, or a
   write whose answer was lost, it reads the log again and writes again only
   once Felix grants it the lease after that read. Residual: a stale holder's
   write still lands if the new holder has written nothing since. The
   new holder's next write is then refused and it reads the stale record
   before writing, so the record is not lost, but it takes effect without the
   lease: a revoke or release written that way counts, late.
3. **Ingest to tracker.** A separate token and a short list of record kinds,
   only on roots ingest imported (`tracker/src/tracker/ingest.rs:20`,
   `tracker/src/tracker/ingest.rs:105`).
4. **GitHub to ingest.** Issue text is data. The pull request an item reports
   is named by an agent, so ingest ties it to the item before it reads it
   (see [What ingest believes](#what-ingest-believes)).
5. **Agent to Felix.** `kbl-mcp` runs beside the model with the agent's
   Felix token. Nothing in `kbl-mcp` is a security check: an agent with its
   token and a raw client skips it. Felix grants are the check.
6. **Browser to gateway.** Read only, one run.

The token file must be mode 600 or 400 (`tracker/src/config.rs:93`), a token
may be listed once (`tracker/src/config.rs:114`), and the lookup takes the
same time for a wrong token as for a right one (`tracker/src/config.rs:149`).
The tracker and the webhook serve plain HTTP, so each refuses to listen on
an address other than loopback unless a flag says a TLS proxy is in front
(`tracker/src/main.rs:132`, `ingest/src/main.rs:69`). The flag is a promise
the process cannot check: set it without a proxy and tokens cross the network
in the clear. Callers are not checked either: `kbl-mcp` and ingest send their
token to whatever tracker URL they are given, `http://` included. Tokens are long-lived ([#121](https://github.com/GetFelix/KBL/issues/121)).
There is no request rate limit; a refused call writes nothing, and the first
cap an attempt hits is logged once (`tracker/src/tracker/tools.rs:994`).

## Tracker routes

Every route but `/healthz` needs a bearer token. A wrong kind of token gets
403. Request bodies are limited to 2 MB by the HTTP layer.

### Agent routes

All of them need an agent token (`tracker/src/http.rs:230`).

| Route | Fields the agent controls | What they could steer | Check |
|---|---|---|---|
| `/v1/work/next` | `wait_seconds`, `review_tiers` | Hold a connection open; claim work it should not | Wait capped at 30 s (`tracker/src/tracker.rs:717`). A claim needs a Ready item, tries left and cost left (`core/src/work_fold.rs:1741`). Any agent may claim any Ready item ([#120](https://github.com/GetFelix/KBL/issues/120)). `review_tiers`: a review is given only to an agent that names its tier; at most 4 names, an unconfigured one is refused (`tracker/src/tracker.rs:1302`). An agent that worked a tree is skipped for its reviews (`tracker/src/tracker.rs:1337`, `tracker/src/tracker.rs:1377`) and the fold refuses the claim too (`core/src/work_fold.rs:1746`) |
| `/v1/work/extend` | `item` | Keep another agent's claim alive, or its own after losing it | Must own the delivery and the hold (`tracker/src/tracker.rs:2453`, `tracker/src/tracker.rs:2460`) |
| `/v1/work/complete` | `item`, `claim`, `attempt`, `usage` | Finish work it does not hold; finish over open children; report no usage, or a model with no price; claim a replay of an outcome it did not write | `claim` (or `attempt`) only replays a record this agent wrote, of the same kind, ending that claim (by attempt, its own latest claim of it); a lapse the tracker wrote stays `lost`, and a `claim` or `attempt` that is not the one held ends nothing (`tracker/src/tracker.rs:538`, `tracker/src/tracker.rs:1709`). Holder only (`tracker/src/tracker.rs:1709`, `core/src/work_fold.rs:1833`). Open children refuse it (`core/src/work_fold.rs:1838`). A fix is not closed by `done`; a merge or a person closes it. Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs:1386`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs:1456`, `tracker/src/http.rs:344`); the item total saturates (`core/src/work_fold.rs:1389`) |
| `/v1/work/release` | `item`, `claim`, `attempt`, `reason`, `usage` | Release another's work; write a huge record | `claim` and `attempt` replay only as for `complete`. Holder only (`core/src/work_fold.rs:1871`). Two claims per attempt (`core/src/work_fold.rs:1728`). `reason` is held to 4 KiB (`tracker/src/tracker/tools.rs:1273`). Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs:1386`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs:1456`, `tracker/src/http.rs:344`) |
| `/v1/work/give_up` | `item`, `claim`, `attempt`, `reason`, `usage` | Same as release | `claim` and `attempt` replay only as for `complete`. Holder only (`core/src/work_fold.rs:1881`). `reason` is held to 4 KiB (`tracker/src/tracker/tools.rs:1273`). Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs:1386`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs:1456`, `tracker/src/http.rs:344`) |
| `/v1/work/post` (child or proposal) | `item`, `key`, `work`, `title` | Create work outside its tree, flood Triage with proposals, or create a kind it may not | Must hold `item` (`tracker/src/tracker/tools.rs:131`). Only split, augment, question, test or a proposal (`tracker/src/tracker/tools.rs:45`). Children draw from the root's items and depth (`core/src/work_fold.rs:2773`). 24 posts per attempt (`core/src/work_fold.rs:2769`). A proposal waits for a person; an agent may have 16 waiting at once and 8 from one root, over all its attempts, and a decision frees a slot. The tracker checks this before it appends (`proposal_room`, `tracker/src/tracker/tools.rs`), so the fold of an old log does not change. Proposals draw no budget and a key can be reused across claims ([#109](https://github.com/GetFelix/KBL/issues/109)) |
| `/v1/work/post` (`work: "prerequisite"`) | `target_source`, `target_repo`, `title`, `why`, `evidence`, `existing`, `done_when`, `claim`, `attempt`, `usage` | File work in another repo's Triage, without limit; link to another tree | `claim` and `attempt` replay only as for `complete` (`tracker/src/tracker/tools.rs:261`). Only a source and repo in the agent's grants or those of its own tree; a grant names both (`tracker/src/config.rs`), and a missing item answers the same as a hidden one (`tracker/src/tracker/tools.rs:284`). Holder, no cycle (`core/src/work_fold.rs:2897`), not closed (`core/src/work_fold.rs:2891`). From format 5, `done_when: released` may name an item closed as merged or completed with no release yet (`core/src/work_fold.rs:2886`). A new one counts against the same pending-proposal caps as `proposal`. Sizes (`tracker/src/tracker/tools.rs:185`). Evidence target: a `work.events` record of the same tree, or the record its context entry says made the item; another stream is refused (`tracker/src/tracker.rs:321`). Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs:1386`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs:1456`, `tracker/src/http.rs:344`). `title` and `why` are read by a person before accepting. From a held triage item the same checks apply, except that nothing is charged and `usage` sent is not recorded (triage reports it when it completes), and the item under triage waits instead (`tracker/src/tracker/tools.rs:214`): the agent can hold up the item it triages, as it can with `unclear`, until a person removes the link |
| `/v1/work/link` | `item`, `to` | Link across trees; block work it does not hold | Same tree only (`core/src/work_fold.rs:3159`). Must hold `item`, a child of it or its parent (`core/src/work_fold.rs:3162`). No cycle (`core/src/work_fold.rs:1672`). 100 links per attempt (`core/src/work_fold.rs:1677`). A repeat is refused (`tracker/src/tracker/tools.rs:394`) |
| `/v1/work/context` | `item` | Read context of work it does not hold | Must hold the item or its parent (`tracker/src/tracker/tools.rs:466`) |
| `/v1/work/context/add` | `item`, `entry`, `text` | Write on work it does not hold; replace what ingest or a person wrote | Must hold the item or a child of it (`core/src/work_fold.rs:1697`). Text inline up to 4 KiB (`tracker/src/tracker/tools.rs:1209`). 100 writes per attempt (`core/src/work_fold.rs:1699`). An agent replaces only an entry it wrote itself; ingest's, a person's and the fold's stay, refused as `not_your_entry` before the record is logged (`tracker/src/tracker/tools.rs:430`). From format 6 the `prerequisite` entry is refused to agents even where none exists. The fold takes such a record from an old log as it always did. A `files` entry puts an overlap warning, and a suggestion that one item wait on another, on the other items' cards in the same repo (`core/src/overlap.rs`). It is only a suggestion: nothing links or blocks until a person records a `linked`. The record names the agent in `by`, and the fold keeps it per entry in `context_agents` |
| `/v1/work/progress` | `note`, `branch`, `pr` | Point the item at someone else's pull request so its merge or checks count | Holder only (`core/src/work_fold.rs:1802`). One request, one open item (`core/src/work_fold.rs:1810`). Sizes (`tracker/src/tracker/tools.rs:1236`). 100 per attempt (`core/src/work_fold.rs:1814`). `pr` is only a name: see [What ingest believes](#what-ingest-believes) |
| `/v1/work/decide` | `phase`, `so_far`, `question`, `options`, `chosen`, `why`, `commit`, `evidence`, `revert_check` | Fill the log; cite evidence that is not there; claim a test guards a fix | Holder only (`core/src/work_fold.rs:1961`). Sizes and counts (`tracker/src/tracker/tools.rs:608`, `tracker/src/tracker/tools.rs:613`). 200 per attempt (`core/src/work_fold.rs:1975`). Evidence is before the record (`core/src/work_fold.rs:1965`). Test name 400 bytes (`core/src/work_fold.rs:1970`). A revert check is the agent's word; only one that failed without and passed with the fix is listed as proven. Evidence target: a `work.events` record of the same tree, or the record its context entry says made the item; another stream is refused (`tracker/src/tracker.rs:321`) |
| `/v1/work/finding` | `item`, `severity`, `file`, `line`, `title`, `scenario`, `verdict` | Fill the log; make fix work without limit; write on a review it does not hold or for an older request | Holder of a review item only (`core/src/work_fold.rs:2583`, `core/src/work_fold.rs:2544`). Sizes: file 400, title 200, scenario 4096 bytes, line from 1 (`tracker/src/tracker/tools.rs:1291`, `core/src/work_fold.rs:2588`). 40 per attempt (`core/src/work_fold.rs:2596`). The request must still be the root's (`core/src/work_fold.rs:2594`). A `fix` draws one item from the root's budget; a `defer` is a proposal a person accepts and counts toward the same pending-proposal caps as `proposal` |
| `/v1/work/verdict` | `item`, `verdict`, `why` | Recommend a merge with fixes open; give a verdict from a first pass; skip the verdict | Holder of a final review only (`core/src/work_fold.rs:2001`, `core/src/work_fold.rs:2002`). `why` at most 2048 bytes (`tracker/src/tracker/tools.rs:1295`). `merge` is refused while a fix a review asked for is open (`core/src/work_fold.rs:2010`). A final review cannot finish without one (`core/src/work_fold.rs:1847`). It is advice: nothing here merges or closes |
| `/v1/work/triage` | `item`, `tier`, `why`, `duplicate_of`, `unclear`, `depends_on`, `split` | Triage an item it holds no triage for; raise its own work's tier; park an item in Needs you; point a duplicate at nothing; fill the log | Holder of the triage item only, and the record names that item's parent, never an item the agent picks (`tracker/src/tracker/tools.rs:733`, `core/src/work_fold.rs:3119`). It is a proposal: a tier a person set stands (`core/src/work_fold.rs:1574`). `why` at most 4096 bytes (`tracker/src/tracker/tools.rs:720`). `duplicate_of` must be another item the agent can see; a missing item and one outside its grants get the same refusal (`tracker/src/tracker/tools.rs:737`). `depends_on` follows the same rule, and may not name the item itself or repeat; `depends_on` and `split` hold at most 8 entries and a split title at most 256 bytes (`triage_lists`, `tracker/src/tracker/tools.rs:1123`). Neither links or creates anything: a person acts on them. A tier never changes the tier an admitted root has. `unclear` only sends the item to a person as a question. The same answer again from the same holder is not logged; an attempt logs at most 4 different answers, then the call is refused; a new holder of the same attempt shares the count (`triage_full`, `core/src/work_fold.rs`) |
| `/v1/work/step` | `command`, `exit_code`, `output` | Claim a command passed | Holder only (`core/src/work_fold.rs:2021`). Sizes (`tracker/src/tracker/tools.rs:1253`). 500 per attempt (`core/src/work_fold.rs:2023`). A step is the agent's word. Check results come from ingest, never from steps |
| `/v1/work/edit` | `file`, `hunk`, `decision` | Tie an edit to another attempt's decision | Holder only (`core/src/work_fold.rs:2034`). The decision is one of this attempt's (`core/src/work_fold.rs:2036`). Sizes (`tracker/src/tracker/tools.rs:1254`). 500 per attempt (`core/src/work_fold.rs:2039`) |
| `/v1/work/ask` | `text`, `options`, `default`, `usage` | Flood Needs you; report no cost | Holder only (`core/src/work_fold.rs:1898`). At most 8 options of 200 bytes (`tracker/src/tracker/tools.rs:1063`). 20 per attempt (`core/src/work_fold.rs:1901`). Usage: what each model used is the agent's word; the fold prices it and charges at least the floor the tracker writes on the record, `KBL_COST_FLOOR_MICROS` (`core/src/work_fold.rs:1386`). At most 16 entries, one per model, plain model names; a `cost_micros` is refused (`core/src/work_fold.rs:1456`, `tracker/src/http.rs:344`); the item total saturates (`core/src/work_fold.rs:1389`) |
| `/v1/work/answer` | `item`, `question`, `text`, `chosen`, `evidence`, `moves_to` | Answer a question nobody asked it; move work to another repo | `moves_to` is refused (`tracker/src/http.rs:871`) and the fold refuses it from an agent (`core/src/work_fold.rs:3028`). Must hold a question item under `item` (`core/src/work_fold.rs:3176`). Only the open question (`core/src/work_fold.rs:1926`). `chosen` is one of the options (`core/src/work_fold.rs:1934`). 10 per attempt (`core/src/work_fold.rs:1942`). The resume item obeys the depth limit (`core/src/work_fold.rs:3019`). Evidence target: a `work.events` record of the same tree, or the record its context entry says made the item; another stream is refused (`tracker/src/tracker.rs:321`) |
| `/v1/work/rules/read` | `item` | Read rules of work it does not hold | Holder only (`tracker/src/tracker/tools.rs:534`) |

### Person routes

All of them need a person token (`tracker/src/http.rs:239`). An agent or
ingest token gets 403. The fold then checks the record, and sizes are checked
in `check_sizes` (`tracker/src/tracker/tools.rs:1175`).

| Route | Record | Check beyond the token |
|---|---|---|
| `/v1/work/records` | Any kind a person may write. A kind the tracker does not know is refused | The fold refuses the kinds a person may not write: see [Records on work.events](#records-on-workevents). The strings are held to 4 KiB when the record is appended, including progress `branch` and `pr`, `triaged.why`, `imported` fields and a rules target (`tracker/src/tracker.rs:2046`) |
| `/v1/work/proposals/accept` | `accepted` | Tier 0 to 3 (`core/src/work_fold.rs:1601`) |
| `/v1/work/proposals/edit` | `edited` | Only before acceptance (`core/src/work_fold.rs:2359`) |
| `/v1/work/proposals/reject` | `rejected` | Only a proposal never accepted (`core/src/work_fold.rs:2135`) |
| `/v1/work/questions/answer` | `answer` | Only the open question; a `moves_to` target and title are bounded (`core/src/work_fold.rs:3034`). Evidence as for an agent's answer. Known secret shapes redacted from the text, choice and title (`tracker/src/tracker/tools.rs:913`) |
| `/v1/work/tier` | `tier_changed` | Tier 0 to 3, item open (`core/src/work_fold.rs:2341`) |
| `/v1/work/note` | `human_input` | Item exists; an `attempt` must be one of the item's (`core/src/work_fold.rs:2049`). Text inline up to 4 KiB (`tracker/src/tracker/tools.rs:1208`). Known secret shapes redacted (`tracker/src/http.rs:952`) |
| `/v1/work/close` | `closed` | Item exists (`core/src/work_fold.rs:2107`) |
| `/v1/work/unlink` | `unlinked` | Item exists (`core/src/work_fold.rs:1688`) |
| `/v1/work/budget` | `budget_set` | Person only in the fold too (`core/src/work_fold.rs:2267`) |
| `/v1/work/prices` | `price_set` | Person only in the fold too, plain model name, and never effective before it is written, so a price cannot reprice usage already charged (`core/src/work_fold.rs:2209`). `as_of` must be a real `YYYY-MM-DD` or the price is refused as `bad_price`; it only labels the price. Format 8, or 9 with `as_of`, so refused until a person raises the log |
| `/v1/work/rules` | `rules_set` | Person only in the fold too (`core/src/work_fold.rs:2285`). 8 KiB of text, 20 checks of 500 bytes, 500 scopes (`core/src/work_fold.rs:2294`). The tracker never runs a check; the agent does |
| `/v1/work/flag`, `/v1/work/unflag` | `flagged` | Person only in the fold too, item open and accepted, and the flag must change (`core/src/work_fold.rs:2157`). Format 7, so refused until a person raises the log. A held item stops only once its holder finishes or releases (`core/src/work_fold.rs:1030`) |
| `GET /v1/work/items`, `GET /v1/work/items/{item}` | None: reads the fold | A person sees every item of the team, as the dashboard does from `work.events` (`tracker/src/http.rs:1050`) |
| `GET /v1/work/items/{item}/history` | None: reads `work.events` | Records about the item, at most 500 per answer and 20,000 read, page by page from Felix (`tracker/src/tracker/reads.rs:15`). Payloads come back as logged, with no redaction at read time. Secrets and agent text are cut only in records written since the log started redacting, so older records return as written. The same person can already read `work.events` itself, so this shows nothing new |

### The dashboard

The dashboard's writes are the person routes above, called from the page with the
signed-in person's own token: `proposals/accept`, `proposals/reject`, `tier`, `close`
and `questions/answer`, plus `GET /v1/me` to check the token (`dashboard/src/tracker.ts`).
The page has no authority of its own; the tracker's checks are the only checks. A write the tracker does not answer within 30 s is shown as failed with its outcome unknown, because the record may have been appended.

| Attack | Check |
|---|---|
| A crafted link sends the person's token somewhere else | The page calls only its own origin's `/v1/...` routes; no parameter names the tracker. `?team=` only picks the `/v1/teams/{team}/` path, and the tracker checks membership (`dashboard/src/writes.ts`) |
| A token left behind in the browser | Typed into a password field, kept in memory for the tab, never in the address, local storage or the view state, and dropped by Sign out in the sidebar, which also names who is signed in, or by a 401 or 403 (`dashboard/src/main.ts`) |
| Another site makes writes as the person (CSRF) | The token goes in an `Authorization` header the page sets, not a cookie, so a request from another origin carries nothing |
| An upstream title runs as script in the page | Views build elements with `dashboard/src/ui/dom.ts`, which never parses HTML |
| A bulk action does more than the person could | Each item is its own call through the same route and checks, one after another within the action, and only on marked items the view shows (`dashboard/src/list.ts`) |
| An undo overwrites someone else's change | Only a tier change has an undo, a new `tier_changed`, and the page offers it only while its own view of the log shows the tier it set. The tier route itself is unconditional, so a change that lands between that look and the undo is overwritten; the log keeps both |

### Ingest routes and health

The ingest routes need an ingest token (`tracker/src/http/ingest.rs:38`).

| Route | What ingest controls | Check |
|---|---|---|
| `/v1/ingest/records` | A whole record | Only the kinds upstream sync needs (`tracker/src/tracker/ingest.rs:20`). Only on roots it imported (`tracker/src/tracker/ingest.rs:105`). An `imported` naming an existing prerequisite is held to the fold's format 6 checks; ingest opens that issue, and trusts a `kbl-prerequisite:` marker, only in a repo whose own rule has `open_prerequisites`, which needs issues: write there (`ingest/src/sync.rs:1080`). A marker counts only in an issue by one of `own_logins`, so those credentials must not be given to agents with issue write. Closing as merged needs a merged status for the request the item reports (`tracker/src/tracker/ingest.rs:111`, `core/src/work_fold.rs:2109`). So does `shipped`, which is also refused once a release is recorded (`tracker/src/tracker/ingest.rs:121`). An `unlinked` removes only an edge that ingest's own `linked` created, which the tracker derives from the log, so an edge from a prerequisite, an answer, a person or an agent stays; it is refused as `not_linked` or `not_ingests`, and ingest's `linked` of such an edge as `already_linked` (`tracker/src/tracker/ingest.rs:138`); ingest writes one only from a complete listing of the issue's links (`ingest/src/sync.rs:880`). Context text up to 4 KiB (`tracker/src/tracker/ingest.rs:92`); a context write with the text the entry already holds, on an entry no agent wrote, is not logged, so repeated lost saves cannot grow the log (`tracker/src/tracker/ingest.rs:157`). Other strings are held to 4 KiB by the append check (`tracker/src/tracker.rs:2145`); ingest cuts titles itself (`ingest/src/sync.rs:1482`) |
| `/v1/ingest/roots` | Nothing | Read only |
| `GET /v1/ingest/state/{source}`, `PUT /v1/ingest/state/{source}` | The saved cursor for a source | The name is a key (`tracker/src/tracker/ingest.rs:292`). 4 MB at most (`tracker/src/tracker/ingest.rs:313`). Saved only if nobody saved since the version read (`tracker/src/tracker/ingest.rs:316`). Any ingest token reaches any source's state, which is fine with one worker |
| `/healthz` | Nothing | No token. Answers `ok` and nothing else |

### Team routes

Every route above acts in one team, the team of the token, and only reaches
that team's tracker: its fold, its lease and its Felix connections
(`tracker/src/teams.rs`). The team comes from the token's entry, never from the
body (`tracker/src/http.rs:170`). Today there is one team, `KBL_NAMESPACE`,
and the token file's callers belong to it.

| Route | Fields the caller controls | What they could steer | Check |
|---|---|---|---|
| `/v1/teams/{team}/{*rest}` | `team`, and the inner route's own fields | Act in a team the token is not in; reach a route that has no team form | Only `work/` and `ingest/` routes are served under it (`tracker/src/http.rs:132`). The named team must be one the token belongs to; any other, existing or not, answers `404` with `unknown_team`, and the token is checked first (`tracker/src/http.rs:182`). The route's own token and field checks then apply as above. Tested with two teams in `tracker/src/http/tests.rs` |
| `/v1/me` | None | Learn other teams | Person token only. Answers only the teams the token is in (`tracker/src/http.rs:251`), with each team's review limit, a count that is no secret |
| `GET /v1/org/teams`, `POST /v1/org/teams` | `team`, `display_name` on `POST` | Create a team, or adopt a namespace that already holds another team's data; list the org's teams | Org admin only: a person of the token file. An agent or ingest token gets `403` with `org_admin_required` before the body is read, and the body is capped at 4 KiB (`tracker/src/http/org.rs`). The name follows the key rule and is not `kbl`. A name already on `org.events` writes nothing and answers `created: false`. The tracker only serves the namespace: Felix grants on it come from `kbl-tool team create`, run with the org admin's own Felix sign-in. Without `KBL_ORG_NAMESPACE` it answers `404` with `teams_off` |

A person token in more than one team gets `400` with `team_required` on a path
without a team. A team takes at most `KBL_TEAM_INFLIGHT` calls at once and
answers `503` with `team_busy` over that (`tracker/src/teams.rs:109`), so one
team's load does not hold the others' calls; waiting `work_next` calls have
their own pool, so they cannot starve a holder's `extend`. A slot is taken
before the body is read, so a slow upload holds one of its own team's slots
only. A team that loses its lease answers `503` alone. Without teams on, the process
exits when every team has lost its lease. With teams on, a team that lost its
lease, or could not start (its log has a record of a newer format), answers
`503` with `team_unavailable` and is started again in the background with a
fresh fold; the process exits only when the org lease has been lost for 60
seconds. Either way it exits at once if a team's maintenance panics
(`tracker/src/teams.rs`).

Each team's tracker, and the org writer, exchanges its ID token for Felix
tokens narrowed to its own namespace (`tracker/src/felix.rs`), so a record
sent on the wrong team's connection is refused by the broker.

## Ingest

| Route | Who can call it | What it could steer | Check |
|---|---|---|---|
| `/webhook` | Anyone who can reach the port | Make ingest spend GitHub requests; inject data | A webhook carries no data into the tracker, it only wakes the poll. HMAC signature, and an empty secret refuses everything (`ingest/src/webhook.rs:83`). A repeated delivery id is dropped (`ingest/src/webhook.rs:90`), which stops GitHub's own redeliveries and nothing else: the signature covers the body, not the delivery id, so a captured body can be sent again under a new id. Without TLS in front, anyone on the path can capture one; ingest listens only on loopback unless a TLS proxy is declared. What bounds a replay is that wakes are spaced (`ingest/src/webhook.rs:103`) and a wake only starts a poll |

### What ingest believes

An agent names its pull request in `/v1/work/progress`. Ingest reads that
request only when all of these hold, so the name alone steers nothing.

| Attack | Check |
|---|---|
| Name a request in another repo | Only the issue's own repo, and only a repo an intake rule names (`ingest/src/sync.rs:1054`) |
| Name a stranger's request | Only a request one of our own logins opened (`ingest/src/sync.rs:577`, `ingest/src/github.rs:362`) |
| Name an old merged request | Created after the item's first claim, and tied to the item by the branch `kbl/<item>` or a closing reference (`ingest/src/sync.rs:591`) |
| Merge into a side branch, or merge its own work | A merge counts only into the default branch and not by one of our logins (`ingest/src/github.rs:374`) |
| Approve its own work | Reviews count only from owners, members and collaborators, never the author (`ingest/src/github.rs:708`) |
| Post a green status itself | Statuses from our own logins are ignored; a failed or partial read is pending, never green (`ingest/src/github.rs:795`) |
| Post a green check run with our own App's token | A check run by an App whose `<slug>[bot]` is one of our logins is ignored (`ingest/src/github.rs:771`) |
| Name another item's request in other letter case | The tracker records the casing an open item already uses, so the fold refuses it as `pr_in_use` (`tracker/src/tracker.rs:312`) |
| Get a stranger's request turned into a draft | Write-back uses the same repo and author checks (`ingest/src/sync.rs:1009`) |
| Hide instructions in an issue or comment | Not stopped: the text is the work. Intake rules choose whose issues get in, text is cut to fit a record (`ingest/src/sync.rs:1491`), our own comments are skipped (`ingest/src/sync.rs:507`), and an edit after triage shows as "changed upstream" |

## MCP tools

`kbl-mcp` runs on the agent's side. The work tools call the tracker with the
agent's token, so the tracker row is the check. The task tools write to Felix
with the agent's Felix token, so Felix grants are the check.

| Tool | Goes to | Check |
|---|---|---|
| `work_next` | `/v1/work/next` | Tracker row |
| `work_extend` | `/v1/work/extend` | Tracker row |
| `work_complete` | `/v1/work/complete` | Tracker row |
| `work_release` | `/v1/work/release` | Tracker row |
| `work_post` | `/v1/work/post` | Tracker row |
| `work_link` | `/v1/work/link` | Tracker row |
| `work_context` | `/v1/work/context` and `/v1/work/context/add` | Tracker rows. Artifacts are shown only if the bytes hash to the id (`mcp/src/tasks.rs:528`) |
| `work_progress` | `/v1/work/progress` | Tracker row |
| `work_finding` | `/v1/work/finding` | Tracker row. The tool also limits `severity` and `verdict` to their words (`mcp/src/server.rs:811`) |
| `work_triage` | `/v1/work/triage` | Tracker row. The tool limits `unclear` to its three words |
| `work_verdict` | `/v1/work/verdict` | Tracker row. The tool limits `verdict` to merge or block |
| `decide` | `/v1/work/decide` | Tracker row. Known secret patterns are cut before long text is stored as an artifact (`mcp/src/tasks.rs:511`) |
| `record_step` | `/v1/work/step` | Tracker row. Known secret patterns are cut before long text is stored as an artifact (`mcp/src/tasks.rs:511`) |
| `edit` | `/v1/work/edit` | Tracker row. Known secret patterns are cut before long text is stored as an artifact (`mcp/src/tasks.rs:511`) |
| `work_ask` | `/v1/work/ask` | Tracker row |
| `work_answer` | `/v1/work/answer` | Tracker row |
| `work_rules` | `/v1/work/rules/read` | Tracker row |
| `run_info` | Nothing | Reads its own settings |
| `task_post` | The run's tasks stream and run log | **None** beyond access to the run ([#29](https://github.com/GetFelix/KBL/issues/29)). Known secret patterns are cut from the title (`mcp/src/tasks.rs:121`) and the body (`mcp/src/tasks.rs:511`) |
| `task_claim` | The run's `workers` group | Felix hands a task to one member. An artifact body is shown only if its bytes hash to the id (`mcp/src/tasks.rs:528`). The two-attempt limit is in `kbl-mcp` only (`mcp/src/tasks.rs:284`, [#39](https://github.com/GetFelix/KBL/issues/39)). A task of a format newer than the worker reads is handed back, not run (`mcp/src/tasks.rs:215`); a writer in the run can post one, and each hand-back uses a delivery until Felix dead-letters it. Known secret patterns are cut from the error text of every `refused` record, the dead-letter and run log gap notes included (`mcp/src/tasks.rs:596`) |
| `task_complete` | The results cache, artifacts and run log | First result wins by a put-if-absent in Felix. Who may write: **None** (#29). Known secret patterns are cut from the result (`mcp/src/tasks.rs:342`) |
| `task_release` | The `workers` group and run log | `kbl-mcp` only releases the task it holds (`mcp/src/tasks.rs:637`). In Felix: **None** (#29). Known secret patterns are cut from the reason (`mcp/src/tasks.rs:443`) |
| `task_give_up` | The `workers` group and run log | Same, and the reason too (`mcp/src/tasks.rs:475`) |

## Records on work.events

Only the tracker appends. "Agent" below means a record written for an agent's
token, "tracker" one the tracker or ingest wrote, "person" one from a person's
token. A kind an actor may not write is refused with `actor_not_allowed`.

| Kind | Who may write | Check |
|---|---|---|
| `imported` | Tracker, person | Not an agent; one per upstream id (`core/src/work_fold.rs:1512`). From format 6 it may name an existing item only if that item is an open root made by a `prerequisite` record, not a deferred finding, that a person accepted after the raise, the issue is in the repo it targets, and it has no upstream issue yet (`core/src/work_fold.rs:2955`) |
| `proposed` | All | An agent needs to hold the parent, and draws budget (`core/src/work_fold.rs:2761`). A `review` item only from the tracker, under an open root that reports the same request, 8 per root (`core/src/work_fold.rs:2509`, `core/src/work_fold.rs:2535`) |
| `triaged` | Person, or the agent holding the item's triage child | `core/src/work_fold.rs:3117`. An agent writes it through `/v1/work/triage` |
| `accepted` | Tracker, person | `core/src/work_fold.rs:1598` |
| `prerequisite` | The holding agent | `core/src/work_fold.rs:2867`. Not closed, except from format 5 a `released` wait on merged or completed work with no release (`core/src/work_fold.rs:2886`). With `triage`, the agent holding that triage item writes it for the item under triage, and keeps its claim (`core/src/work_fold.rs:1236`). Format 3, so refused until a person raises the log |
| `shipped` | Tracker, person, ingest | Only a done item (`core/src/work_fold.rs:1659`). Ingest only on a root it imported whose reported request has a merged status, and not once a release is recorded (`tracker/src/tracker/ingest.rs:121`). The worker, not the tracker, checks that the release is published and not a draft or prerelease, and that GitHub says its tag contains the merge commit. A person's `shipped` replaces the release recorded before, ingest's included (`core/src/work_fold.rs:1662`), and a reopen clears it (`core/src/work_fold.rs:2188`) |
| `linked` | All | An agent only inside one tree (`core/src/work_fold.rs:3159`) |
| `unlinked` | Tracker, person | `core/src/work_fold.rs:1685` |
| `context` | All | An agent only on work it holds (`core/src/work_fold.rs:1697`). An agent replaces only its own entries (`tracker/src/tracker/tools.rs:430`) |
| `finding` | The agent holding a review item | `core/src/work_fold.rs:2583`. Bounded and capped as the route above |
| `review_verdict` | The agent holding a final review | `core/src/work_fold.rs:2001`, `core/src/work_fold.rs:2002`. Open fixes block a merge (`core/src/work_fold.rs:2010`) |
| `claimed` | Agent, written by the tracker on a delivery | `core/src/work_fold.rs:1717` |
| `progress` | Holder | `core/src/work_fold.rs:1802` |
| `done` | Holder | `core/src/work_fold.rs:1833` |
| `released` | Holder, or the tracker on a lapse | `core/src/work_fold.rs:1871` |
| `gave_up` | Holder | `core/src/work_fold.rs:1881` |
| `question` | The holding agent | `core/src/work_fold.rs:1898` |
| `answer` | Person, or the agent holding a question item | `core/src/work_fold.rs:1925` |
| `decision` | The holding agent | `core/src/work_fold.rs:1961` |
| `step` | The holding agent | `core/src/work_fold.rs:2021` |
| `edit` | The holding agent | `core/src/work_fold.rs:2034` |
| `human_input` | Tracker, person | `core/src/work_fold.rs:2045` |
| `forked_from` | Tracker, person | Redo limit per item (`core/src/work_fold.rs:2092`). While the item's pull request has a failed check, the tracker refuses one that names no note written since the failure (`tracker/src/tracker.rs:377`) |
| `closed` | Tracker, person | The tracker closes as merged only on a merged status (`core/src/work_fold.rs:2109`) |
| `rejected` | Tracker, person | `core/src/work_fold.rs:2104` |
| `updated` | Tracker, person | Open roots only (`core/src/work_fold.rs:2700`) |
| `left_scope` | Tracker, person | Open roots only (`core/src/work_fold.rs:2700`) |
| `returned_to_scope` | Tracker, person | Open roots only (`core/src/work_fold.rs:2703`). Format 2, so refused until a person raises the log (`core/src/work_fold.rs:729`) |
| `flagged` | Person | Open, accepted items only, and only a change (`core/src/work_fold.rs:2168`). Format 7, so refused until a person raises the log (`core/src/work_fold.rs:729`). A reopen clears it |
| `reopened` | Tracker, person | Closed roots only (`core/src/work_fold.rs:2181`). Ingest reopens only what it closed. It keeps release waits and clears the item's release, so a waiter waits for the next release |
| `attempt_closed` | Tracker, person | `core/src/work_fold.rs:2199` |
| `tier_changed` | Person | `core/src/work_fold.rs:2338` |
| `edited` | Person | `core/src/work_fold.rs:2351` |
| `budget_set` | Person | `core/src/work_fold.rs:2267` |
| `price_set` | Person | Not backdated; usage waiting for a price is charged the rest when one comes (`core/src/work_fold.rs:2209`, `core/src/work_fold.rs:1409`). Format 8, so refused until a person raises the log (`core/src/work_fold.rs:729`) |
| `pr_status` | Tracker, person | Only for the request the item reports, bounded and consistent (`core/src/work_fold.rs:2396`) |
| `source_stalled` | Tracker | 200-byte names, 64 repos (`core/src/work_fold.rs:2421`) |
| `rules_set` | Person | `core/src/work_fold.rs:2285` |
| `refused` | Tracker | `core/src/work_fold.rs:2373` |
| `format_raised` | Person | Only upward, only to a format the tracker's build reads, and stamped with that format (`core/src/work_fold.rs:724`). An older reader stops at it ([upgrades.md](upgrades.md)) |

## Records on the run log

A run log (`agents.log.<run>`) is written straight to Felix by `kbl-mcp`,
`kbl-run` and `kbl-tool`, each with its own Felix token. Nothing
validates a record before it lands, and `by` is what the writer says. So a
run log is a set of claims, and the dashboard marks it unverified. Who may
write which kind: **None**, tracked by
[#29](https://github.com/GetFelix/KBL/issues/29) (roles per run),
[#45](https://github.com/GetFelix/KBL/issues/45) (the publisher Felix
recorded) and [#32](https://github.com/GetFelix/KBL/issues/32) (a
grants test with a raw client).

| Kinds | Meant to be written by |
|---|---|
| `run_started` | `kbl-tool` |
| `run_ended`, `agent_up`, `model_started`, `model_call`, `budget_exhausted`, `refund_unconfirmed` | `kbl-run`, with the runtime token the model cannot reach |
| `agent_gone` | The planner's `kbl-mcp` |
| `task_posted`, `task_claimed`, `task_extended`, `task_released`, `task_done`, `task_duplicate`, `task_dead`, `task_claim_lost` | An agent's `kbl-mcp` |
| `board_put`, `board_conflict`, `lease_taken`, `lease_renewed`, `lease_released`, `lease_lost`, `message`, `refused` | An agent's `kbl-mcp` |

## Teams

Partly built ([#185](https://github.com/GetFelix/KBL/issues/185),
[design.md](design.md#teams)): the team table, the team paths and `/v1/me` are
under [Team routes](#team-routes). The rows below are the checks each later
pull request in the plan has to add. They move into the tables above as the
routes ship.

### Actors with teams

| Actor | Credential | Trust |
|---|---|---|
| Org admin | A person token with `org_admin` on `org.events`, and a Felix sign-in with `tenant.manage` for `kbl-tool` | Trusted for the whole org |
| Team member | A person token, and a role in each team they belong to | Trusted to the role's limit, in their own teams only |
| Team agent | An agent token bound to one team | Not trusted, as any agent |
| Team ingest | An ingest token bound to one team | Trusted code carrying untrusted text, for the team's own sources |

The new asset is `org.events`: who is in which team, every role and who
granted it, token hashes, sources and grants. An attacker wants a role,
a team or a token it was not given.

### Org and team routes

| Route | Fields the caller controls | What they could steer | Check |
|---|---|---|---|
| `POST /v1/teams/{team}/members` | `team`, `person`, `role`, `repos`, `subject` | Make itself or a friend admin; reach another team; take over a person by naming their OIDC subject | Admin of that team or `org_admin`. `org_admin` is granted only by an `org_admin`. `repos` must be the team's own sources. A `subject` already on another person is refused. The Felix assignment is made with the caller's own Felix credential, so Felix refuses one outside the caller's scope |
| `DELETE /v1/teams/{team}/members/{person}` | `team`, `person` | Lock out the other admins | Team admin or `org_admin`. The last admin of a team cannot be removed; an `org_admin` can always add one back |
| `POST /v1/teams/{team}/agents` | `team`, `agent` | Mint tokens without limit; take an existing agent's name and its holds | Triager or above in that team. A name in use in the team is refused, and so is a revoked one, so a new token never inherits holds. A cap on agents per team |
| `DELETE /v1/teams/{team}/agents/{agent}` | `team`, `agent` | Revoke another team's agent; free work held by others | Whoever added it, or team admin, in that team only. Releases holds only of that agent |
| `POST /v1/teams/{team}/sources` | `team`, `source`, `repo` | Pull another team's issues into its own queue | Team admin. One team per source: a source another team owns is refused |
| `POST /v1/teams/{team}/ingest` | `team`, `name` | Write to another team's log | Team admin. The token is bound to the team, and ingest records only for roots from the team's sources |
| `POST /v1/teams/{team}/grants` | `team`, `from_team`, `agents`, `repos`, `max_pending` | Give itself the right to file into another team | Only an admin of the receiving team (`{team}`) writes a grant. `repos` must be that team's sources |
| `/v1/teams/{team}/work/...` | `team`, and the route's own fields | Act in a team it is not in | The person must be a member with the route's role, checked before the append. An agent or ingest token with another team's path gets `unknown_team`, the same answer as a team that does not exist |

### Attacks across teams

| Attack | Check |
|---|---|
| A team's agent or viewer reads or writes another team's streams with a raw client | Felix: every grant names the team's namespace, and no team principal has anything outside it. Tested with a raw Felix client in `dev/check-teams.mjs`: a viewer of team `alpha` reads `alpha`'s `work.events` and cannot read or write `beta`'s, and the tracker's token narrowed to `alpha` cannot reach `beta` either |
| A tracker bug routes team A's record to team B's log | Each team's fold publishes only with a Felix token narrowed to its own namespace, so the broker refuses it |
| A revoked agent keeps working | The token table is the fold of `org.events`; the next call after `agent_revoked` is refused, and its holds are released |
| A removed member keeps reading the dashboard | Removing the Felix assignment revokes the person's broker tokens and ends open streams, within the control plane's sync interval |
| A cross-team prerequisite floods another team's Triage | A grant from the receiving team is needed, the filing agent's pending-proposal caps apply, and the grant can carry its own cap |
| A forged "prerequisite done" frees blocked work | `prerequisite_settled` is written only by the tracker, from the other team's fold. No route takes it from a person, agent or ingest |
| `org.events` leaks | It holds SHA-256 hashes of 32-byte random tokens, not tokens. Only the tracker and org admins can read it |
| One team's load or a broken log stops the others | A cap on calls in flight per team, and a team with a lost lease or failing Felix calls answers 503 alone: built, see [Team routes](#team-routes). A team whose log has a newer format, or that lost its lease, answers 503 and is retried in the background while the others serve. One process is still one crash for all (#124) |
| A person in two teams acts in the wrong one | Person routes name the team; a person token in several teams gets `team_required` on a path without one |

## Records on org.events

Built in `kbl-core` ([#185](https://github.com/GetFelix/KBL/issues/185)). The tracker writes `team_created`; no route writes the others yet. The tracker will be the only appender, and
it checks the caller's role before it asks the fold. The fold (`OrgState::check`)
then refuses a record the log cannot take, before the append and again on replay, so a
refused call writes nothing and an old log folds as it did. The log holds the SHA-256 of
each token and never a token. A format above the build's stops the reader.

| Kind | Who may write | Check |
|---|---|---|
| `team_created` | Org admin, through `POST /v1/org/teams`; or the tracker, adopting `KBL_NAMESPACE` when the log is empty | Name follows the key rule and is not `kbl`; repeating a team changes nothing |
| `person_added` | Org admin, team admin (route check, later) | Name and OIDC subject unique, token hash not used by any person, agent or ingest worker |
| `role_granted` | Team admin, org admin (route check, later) | Person and team exist; `org_admin` only with no team, a team role only with one; `repos` are the team's own sources; the last unscoped admin of a team cannot be demoted; an admin role covers the whole team, so a scoped one is refused |
| `role_revoked` | Team admin, org admin (route check, later) | The last admin of a team and the last org admin stay |
| `agent_added` | Triager and above (route check, later) | Team exists; name unique in the team and a revoked name stays taken; token hash unused; 256 per team |
| `agent_revoked` | Whoever added it, team admin (route check, later) | Team and agent exist; revoking twice changes nothing |
| `source_added` | Team admin (route check, later) | One owning team per source, compared exactly (case counts); a label of 1 to 256 bytes with no control characters |
| `ingest_added` | Team admin (route check, later) | Name unique in the team, token hash unused, 32 per team |
| `grant_added` | Admin of the receiving team (route check, later) | Both teams exist and differ; `repos` are the receiver's sources; 256 per receiving team |

## Attack classes found in review

| Class | Example | Where it is stopped |
|---|---|---|
| Steering a field the agent does not own | Set rules, tiers or budgets; name itself on a record; move work to another repo | Role and name come from the token; person-only kinds; `moves_to` refused |
| Cross-tree links | Make another tree wait on its item | `/v1/work/link` is one tree only; a prerequisite needs a grant |
| Writes to work it does not hold | Context, progress or an answer on another agent's item | Holder checks in the fold on every agent record |
| Unbounded writes per attempt | Loop on `decide` or `record_step` to fill the log | A cap per kind per attempt, two claims per attempt, and one logged refusal |
| Forged artifacts and statuses | Bytes under another id; a green check; an approval; a release | Hash check on read; statuses come from ingest only; `shipped` is not an agent's |
| Merges into other branches | Merge into a side branch and report the item merged | A merge counts only into the default branch, by someone else |
| Budget escapes | Free questions, free prerequisites, resume work past the depth limit | Cost is charged on ask and prerequisite; depth applies to resume work. Every claim end and ask is charged at least the floor the tracker writes on it, so an agent that reports no usage still spends the root's cost budget. Naming a model with no price costs the floor only until a person prices it; then the fold draws the rest. At most 32 models wait for a price at once, so made-up names cannot grow the state without bound. Naming a cheaper model than the one used, or under-reporting tokens, is not caught until `kbl-run` measures usage |
| Secrets in what an agent reports | A token from the environment in a step's output or a diff hunk, kept on the log for as long as the item | Known secret shapes are replaced with `[redacted]` by the tracker in the free text of every record an agent writes (`tracker/src/tracker.rs:2150`) and of a person's note (`tracker/src/http.rs:952`) or answer (`tracker/src/tracker/tools.rs:913`), and by `kbl-mcp` before it stores an artifact (`mcp/src/tasks.rs:511`), a task result (`mcp/src/tasks.rs:342`) or a task title (`mcp/src/tasks.rs:121`). Every record kind's fields are sorted into redacted and structured, with no default (`core/src/redact.rs:1057`); a progress record's `pr` and `branch` are keys the fold matches on, so only URL userinfo is cut from them. Each shape is matched on what a real one looks like, not its prefix alone, because a cut word in a record is lost for good. What still goes through: a secret with no known prefix, such as an AWS secret access key; `Bearer` followed by two spaces or a tab; quoted assignments such as `TOKEN="..."` and `"token": "..."`; `password=`; `token=` outside a URL query unless the value has a long run of mixed letters and digits; the body of a key whose lines start with anything but indentation, `>`, `+` or `-`, such as `# ` or `//` comments, or that has a `Name: value` header line over 256 bytes or one that names another key header (its header line is cut); an AWS access key id followed by anything but a second id, such as `AKIA...EXAMPLEs`; a `Bearer` token with no digits outside an `Authorization` header, or inside one when it reads as a call, a path or words joined by `-` (`Bearer correct-horse-battery-staple-token` and `Bearer AbCd.EfGh` look like prose and paths), after `AUTHORIZATION,` with no quote, or in `from_static("Bearer ...")` and `format!("Bearer ...")`, where no header name is in sight; a PEM body indented more than 128 bytes with no `END` line; a key with no `END` whose first body line is under 32 bytes, or under 64 bytes with fewer than three character classes (a 64-character hex hash after a header is cut with one more short line), whose first line is over 71 bytes with fewer than three character classes, or that has more than one blank line after its header; a one-line key with no `END` whose short last run has no `=` padding and is followed by prose; a Slack token whose secret is under 12 bytes or of one character class. The reverse also happens: `feature/xoxb-2-RotateTokensV2x` is cut as a token and `-----BEGIN PRIVATE KEY-----` named in prose is cut as a header, which loses the words and nothing else. Line breaks escaped to any depth, written as `\u000a` or `%0A` (with or without the carriage return, and with `%2B`, `%2F` and `%3D` read as base64), and a `\n\` continuation, are followed through a key body. `PRIVATE KEY` and PGP `PRIVATE KEY BLOCK` headers are keys, also URL-encoded with `+` or `%20` for the spaces (`-----BEGIN%20PRIVATE%20KEY-----`), `Proc-Type:`, `DEK-Info:` and any number of PGP armor lines before a body go with it, a `/` escaped as `\/` and `+`, `/` or `=` written as `\u002b`, `\u002f` or `\u003d` (at any depth) count as base64, a URL-encoded key pasted on one line has its runs split by `%20`, a body with its line breaks stripped goes whole, stuck to the header or after one gap, a URL-encoded byte (`%20ghp_...`, `d%3Dxoxd-...`) starts a secret like a space does, and two AWS ids joined by one byte are cut together |
| Forged webhooks | Wake ingest in a loop | Signature and spacing. A captured body can be replayed under a new delivery id, and it only wakes the poll |

## Gaps

| Issue | Gap |
|---|---|
| [#109](https://github.com/GetFelix/KBL/issues/109) | Proposals draw no budget; post keys can be reused across claims |
| [#185](https://github.com/GetFelix/KBL/issues/185) | `org.events` has no record to remove a person, rotate a person's token or revoke an ingest token yet; counts of teams, people and members are not capped |
| [#120](https://github.com/GetFelix/KBL/issues/120) | No roles: every person can do everything, every agent can claim any repo's work |
| [#121](https://github.com/GetFelix/KBL/issues/121) | Tokens are long-lived |
| [#29](https://github.com/GetFelix/KBL/issues/29), [#32](https://github.com/GetFelix/KBL/issues/32), [#45](https://github.com/GetFelix/KBL/issues/45) | Any writer in a run can write any run log record under any name |
