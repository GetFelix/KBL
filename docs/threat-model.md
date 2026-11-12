# Threat model

Who can reach Kibble, what they could damage, and the check that stops each
attempt. Every tracker route, ingest route, MCP tool and record kind has a row.

A pull request that adds a route, a tool or a record kind adds its row here.
`scripts/check-threat-model.mjs` fails CI when one is missing.

A cell marked **None** is a gap with an open issue. The gaps are listed at
the end.

## Actors

| Actor | What it is | Credential | Trust |
|---|---|---|---|
| Person | Someone using the dashboard or the REST API | A person token from the tracker's token file | Trusted. Every person token can do everything a person can ([#120](https://github.com/GetFelix/felix-agents/issues/120)) |
| Agent | A model acting through `felix-mcp` | An agent token for the tracker, and a Felix token for its run | Not trusted. Assume it follows any instruction it reads in an issue, a comment or a file |
| Ingest | The `agents-ingest` worker that mirrors GitHub | An ingest token, and a GitHub credential | Trusted code that carries untrusted text |
| Gateway reader | A browser reading through felix-gateway | A gateway session scoped to one run | Read only (`dev/scope.toml`) |
| Webhook sender | Whoever posts to ingest's `/webhook` | The shared webhook secret | Not trusted without a valid signature |

Two more parties are outside Kibble and can still steer it:

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
| Merges | GitHub. Kibble records them and never merges | An item closed as merged that was not, or a merge into the wrong branch |
| Rules | `rules_set` records, shown in every claim reply | Instructions injected into every other agent |
| Artifacts | The artifacts cache, keyed by content hash | Other bytes shown under a trusted id |
| Credentials | The token file, the webhook secret, the GitHub credential, Felix tokens | Any of them |

## Trust boundaries

1. **Agent to tracker.** The bearer token names the caller
   (`tracker/src/http.rs:675`). The name and role on a record come from the
   token, never from the body (`tracker/src/tracker.rs:958`). The attempt
   number comes from the hold, never from the body
   (`tracker/src/tracker/tools.rs:647`, `tracker/src/tracker.rs:778`).
2. **Tracker to log.** The tracker is the only writer of `work.events`. It
   appends a record only if the fold accepts it
   (`tracker/src/tracker.rs:979`), so every rule in
   `core/src/work_fold.rs` is a check on every route. A writer lease keeps a
   second tracker from writing (`tracker/src/tracker.rs:965`); the window
   that is left is noted in the code there.
3. **Ingest to tracker.** A separate token and a short list of record kinds,
   only on roots ingest imported (`tracker/src/tracker/ingest.rs:20`,
   `tracker/src/tracker/ingest.rs:88`).
4. **GitHub to ingest.** Issue text is data. The pull request an item reports
   is named by an agent, so ingest ties it to the item before it reads it
   (see [What ingest believes](#what-ingest-believes)).
5. **Agent to Felix.** `felix-mcp` runs beside the model with the agent's
   Felix token. Nothing in `felix-mcp` is a security check: an agent with its
   token and a raw client skips it. Felix grants are the check.
6. **Browser to gateway.** Read only, one run.

The token file must be mode 600 or 400 (`tracker/src/config.rs:90`), a token
may be listed once (`tracker/src/config.rs:113`), and the lookup takes the
same time for a wrong token as for a right one (`tracker/src/config.rs:142`).
The tracker and the webhook serve plain HTTP: **None**
([#158](https://github.com/GetFelix/felix-agents/issues/158)). Tokens are
long-lived ([#121](https://github.com/GetFelix/felix-agents/issues/121)).
There is no request rate limit; a refused call writes nothing, and the first
cap an attempt hits is logged once (`tracker/src/tracker/tools.rs:685`).

## Tracker routes

Every route but `/healthz` needs a bearer token. A wrong kind of token gets
403. Request bodies are limited to 2 MB by the HTTP layer.

### Agent routes

All of them need an agent token (`tracker/src/http.rs:685`).

| Route | Fields the agent controls | What they could steer | Check |
|---|---|---|---|
| `/v1/work/next` | `wait_seconds`, `review_tiers` | Hold a connection open; claim work it should not | Wait capped at 30 s (`tracker/src/tracker.rs:40`). A claim needs a Ready item, tries left and cost left (`core/src/work_fold.rs:1016`). Any agent may claim any Ready item ([#120](https://github.com/GetFelix/felix-agents/issues/120)). `review_tiers`: a review is given only to an agent that names its tier; at most 4 names, an unconfigured one is refused (`tracker/src/tracker.rs:647`). An agent that worked a tree is skipped for its reviews (`tracker/src/tracker.rs:681`, `tracker/src/tracker.rs:743`) and the fold refuses the claim too (`core/src/work_fold.rs:1032`) |
| `/v1/work/extend` | `item` | Keep another agent's claim alive, or its own after losing it | Must own the delivery and the hold (`tracker/src/tracker.rs:289`, `tracker/src/tracker.rs:307`) |
| `/v1/work/complete` | `item`, `cost_micros` | Finish work it does not hold; finish over open children; report no cost | Holder only (`tracker/src/tracker.rs:1171`, `core/src/work_fold.rs:673`). Open children refuse it (`core/src/work_fold.rs:1038`). A fix is not closed by `done`; a merge or a person closes it. Cost: **None** ([#155](https://github.com/GetFelix/felix-agents/issues/155), [#153](https://github.com/GetFelix/felix-agents/issues/153)) |
| `/v1/work/release` | `item`, `reason`, `cost_micros` | Release another's work; write a huge record | Holder only (`core/src/work_fold.rs:1067`). Two claims per attempt (`core/src/work_fold.rs:936`). `reason` size: **None** ([#154](https://github.com/GetFelix/felix-agents/issues/154)). Cost: **None** (#155) |
| `/v1/work/give_up` | `item`, `reason`, `cost_micros` | Same as release | Holder only (`core/src/work_fold.rs:1083`). `reason` size: **None** (#154). Cost: **None** (#155) |
| `/v1/work/post` (child or proposal) | `item`, `key`, `work`, `title` | Create work outside its tree, without limit, or of a kind it may not | Must hold `item` (`tracker/src/tracker/tools.rs:114`). Only split, augment, question, test or a proposal (`tracker/src/tracker/tools.rs:39`). Children draw from the root's items and depth (`core/src/work_fold.rs:1566`). 24 posts per attempt (`core/src/work_fold.rs:1560`). A proposal waits for a person. Proposals draw no budget and a key can be reused across claims ([#109](https://github.com/GetFelix/felix-agents/issues/109)) |
| `/v1/work/post` (`work: "prerequisite"`) | `target_source`, `target_repo`, `title`, `why`, `evidence`, `existing`, `done_when`, `cost_micros` | File work in another repo's Triage; link to another tree | Only a repo in the agent's grants or its own tree's repo, and a missing item answers the same as a hidden one (`tracker/src/tracker/tools.rs:204`). Holder, no cycle, not closed (`core/src/work_fold.rs:1644`). Sizes (`tracker/src/tracker/tools.rs:159`). `target_source`: **None** ([#156](https://github.com/GetFelix/felix-agents/issues/156)). Evidence target: **None** ([#157](https://github.com/GetFelix/felix-agents/issues/157)). Cost: **None** (#155). `title` and `why` are read by a person before accepting |
| `/v1/work/link` | `item`, `to` | Link across trees; block work it does not hold | Same tree only (`core/src/work_fold.rs:1880`). Must hold `item`, a child of it or its parent (`core/src/work_fold.rs:1883`). No cycle (`core/src/work_fold.rs:891`). 100 links per attempt (`core/src/work_fold.rs:896`). A repeat is refused (`tracker/src/tracker/tools.rs:293`) |
| `/v1/work/context` | `item` | Read context of work it does not hold | Must hold the item or its parent (`tracker/src/tracker/tools.rs:349`) |
| `/v1/work/context/add` | `item`, `entry`, `text` | Write on work it does not hold; replace what ingest or a person wrote | Must hold the item or a child of it (`core/src/work_fold.rs:913`). Text inline up to 4 KiB (`tracker/src/tracker/tools.rs:317`). 100 writes per attempt (`core/src/work_fold.rs:918`). Replacing another writer's entry: **None** ([#152](https://github.com/GetFelix/felix-agents/issues/152)) |
| `/v1/work/progress` | `note`, `branch`, `pr` | Point the item at someone else's pull request so its merge or checks count | Holder only (`core/src/work_fold.rs:1000`). One request, one open item (`core/src/work_fold.rs:1002`). Sizes (`tracker/src/tracker/tools.rs:453`). 100 per attempt (`core/src/work_fold.rs:1012`). `pr` is only a name: see [What ingest believes](#what-ingest-believes) |
| `/v1/work/decide` | `phase`, `so_far`, `question`, `options`, `chosen`, `why`, `commit`, `evidence`, `revert_check` | Fill the log; cite evidence that is not there; claim a test guards a fix | Holder only (`core/src/work_fold.rs:1263`). Sizes and counts (`tracker/src/tracker/tools.rs:503`, `tracker/src/tracker/tools.rs:516`). 200 per attempt (`core/src/work_fold.rs:1277`). Evidence is before the record (`core/src/work_fold.rs:1267`). Test name 400 bytes (`core/src/work_fold.rs:1272`). A revert check is the agent's word; only one that failed without and passed with the fix is listed as proven. Evidence target: **None** (#157) |
| `/v1/work/finding` | `item`, `severity`, `file`, `line`, `title`, `scenario`, `verdict` | Fill the log; make fix work without limit; write on a review it does not hold or for an older request | Holder of a review item only (`core/src/work_fold.rs:1781`, `core/src/work_fold.rs:1742`). Sizes: file 400, title 200, scenario 4096 bytes, line from 1 (`tracker/src/tracker/tools.rs:545`, `core/src/work_fold.rs:1786`). 40 per attempt (`core/src/work_fold.rs:1794`). The request must still be the root's (`core/src/work_fold.rs:1762`). A `fix` draws one item from the root's budget; a `defer` is a proposal a person accepts |
| `/v1/work/verdict` | `item`, `verdict`, `why` | Recommend a merge with fixes open; give a verdict from a first pass; skip the verdict | Holder of a final review only (`core/src/work_fold.rs:1303`, `core/src/work_fold.rs:1750`). `why` at most 2048 bytes (`tracker/src/tracker/tools.rs:576`). `merge` is refused while a fix a review asked for is open (`core/src/work_fold.rs:1313`). A final review cannot finish without one (`core/src/work_fold.rs:1134`). It is advice: nothing here merges or closes |
| `/v1/work/step` | `command`, `exit_code`, `output` | Claim a command passed | Holder only (`core/src/work_fold.rs:1176`). Sizes (`tracker/src/tracker/tools.rs:520`). 500 per attempt (`core/src/work_fold.rs:1178`). A step is the agent's word. Check results come from ingest, never from steps |
| `/v1/work/edit` | `file`, `hunk`, `decision` | Tie an edit to another attempt's decision | Holder only (`core/src/work_fold.rs:1189`). The decision is one of this attempt's (`core/src/work_fold.rs:1191`). Sizes (`tracker/src/tracker/tools.rs:541`). 500 per attempt (`core/src/work_fold.rs:1194`) |
| `/v1/work/ask` | `text`, `options`, `default`, `cost_micros` | Flood Needs you; report no cost | Holder only (`core/src/work_fold.rs:1101`). At most 8 options of 200 bytes (`tracker/src/tracker/tools.rs:733`). 20 per attempt (`core/src/work_fold.rs:1104`). Cost: **None** (#155, #153) |
| `/v1/work/answer` | `item`, `question`, `text`, `chosen`, `evidence`, `moves_to` | Answer a question nobody asked it; move work to another repo | `moves_to` is refused (`tracker/src/http.rs:509`) and the fold refuses it from an agent (`core/src/work_fold.rs:1750`). Must hold a question item under `item` (`core/src/work_fold.rs:1888`). Only the open question (`core/src/work_fold.rs:1130`). `chosen` is one of the options (`core/src/work_fold.rs:1133`). 10 per attempt (`core/src/work_fold.rs:1146`). The resume item obeys the depth limit (`core/src/work_fold.rs:1741`) |
| `/v1/work/rules/read` | `item` | Read rules of work it does not hold | Holder only (`tracker/src/tracker/tools.rs:412`) |

### Person routes

All of them need a person token (`tracker/src/http.rs:694`). An agent or
ingest token gets 403. The fold then checks the record, and sizes are checked
in `check_sizes` (`tracker/src/tracker/tools.rs:804`).

| Route | Record | Check beyond the token |
|---|---|---|
| `/v1/work/records` | Any kind a person may write | The fold refuses the kinds a person may not write: see [Records on work.events](#records-on-workevents). Sizes of `triaged.why`, `imported` fields and a rules target: **None** (#154) |
| `/v1/work/proposals/accept` | `accepted` | Tier 0 to 3 (`core/src/work_fold.rs:823`) |
| `/v1/work/proposals/edit` | `edited` | Only before acceptance (`core/src/work_fold.rs:1431`) |
| `/v1/work/proposals/reject` | `rejected` | Only a proposal never accepted (`core/src/work_fold.rs:1286`) |
| `/v1/work/questions/answer` | `answer` | Only the open question; a `moves_to` target and title are bounded (`core/src/work_fold.rs:1753`) |
| `/v1/work/tier` | `tier_changed` | Tier 0 to 3, item open (`core/src/work_fold.rs:1413`) |
| `/v1/work/close` | `closed` | Item exists (`core/src/work_fold.rs:1262`) |
| `/v1/work/unlink` | `unlinked` | Item exists (`core/src/work_fold.rs:907`) |
| `/v1/work/budget` | `budget_set` | Person only in the fold too (`core/src/work_fold.rs:1339`) |
| `/v1/work/rules` | `rules_set` | Person only in the fold too (`core/src/work_fold.rs:1357`). 8 KiB of text, 20 checks of 500 bytes, 500 scopes (`core/src/work_fold.rs:1366`). The tracker never runs a check; the agent does |

### Ingest routes and health

The ingest routes need an ingest token (`tracker/src/http/ingest.rs:38`).

| Route | What ingest controls | Check |
|---|---|---|
| `/v1/ingest/records` | A whole record | Only the kinds upstream sync needs (`tracker/src/tracker/ingest.rs:20`). Only on roots it imported (`tracker/src/tracker/ingest.rs:88`). Closing as merged needs a merged status for the request the item reports (`tracker/src/tracker/ingest.rs:94`, `core/src/work_fold.rs:1264`). Context text up to 4 KiB (`tracker/src/tracker/ingest.rs:75`). Other field sizes: **None** (#154); ingest cuts titles itself (`ingest/src/sync.rs:890`) |
| `/v1/ingest/roots` | Nothing | Read only |
| `/v1/ingest/state/{source}` | The saved cursor for a source | The name is a key (`tracker/src/tracker/ingest.rs:181`). 4 MB at most (`tracker/src/tracker/ingest.rs:156`). Saved only if nobody saved since the version read (`tracker/src/tracker/ingest.rs:159`). Any ingest token reaches any source's state, which is fine with one worker |
| `/healthz` | Nothing | No token. Answers `ok` and nothing else |

## Ingest

| Route | Who can call it | What it could steer | Check |
|---|---|---|---|
| `/webhook` | Anyone who can reach the port | Make ingest spend GitHub requests; inject data | A webhook carries no data into the tracker, it only wakes the poll. HMAC signature, and an empty secret refuses everything (`ingest/src/webhook.rs:83`). A repeated delivery id is dropped (`ingest/src/webhook.rs:90`), which stops GitHub's own redeliveries and nothing else: the signature covers the body, not the delivery id, so a captured body can be sent again under a new id. Over plain HTTP anyone on the path can capture one (#158). What bounds a replay is that wakes are spaced (`ingest/src/webhook.rs:101`) and a wake only starts a poll |

### What ingest believes

An agent names its pull request in `/v1/work/progress`. Ingest reads that
request only when all of these hold, so the name alone steers nothing.

| Attack | Check |
|---|---|
| Name a request in another repo | Only the issue's own repo, and only a repo an intake rule names (`ingest/src/sync.rs:733`) |
| Name a stranger's request | Only a request one of our own logins opened (`ingest/src/sync.rs:474`, `ingest/src/github.rs:335`) |
| Name an old merged request | Created after the item's first claim, and tied to the item by the branch `kibble/<item>` or a closing reference (`ingest/src/sync.rs:488`) |
| Merge into a side branch, or merge its own work | A merge counts only into the default branch and not by one of our logins (`ingest/src/github.rs:346`) |
| Approve its own work | Reviews count only from owners, members and collaborators, never the author (`ingest/src/github.rs:516`) |
| Post a green status itself | Statuses from our own logins are ignored; a failed or partial read is pending, never green (`ingest/src/github.rs:559`) |
| Get a stranger's request turned into a draft | Write-back uses the same repo and author checks (`ingest/src/sync.rs:697`) |
| Hide instructions in an issue or comment | Not stopped: the text is the work. Intake rules choose whose issues get in, text is cut to fit a record (`ingest/src/sync.rs:899`), our own comments are skipped (`ingest/src/sync.rs:404`), and an edit after triage shows as "changed upstream" |

Still open: canonical request names and a filter on who created a check run
([#148](https://github.com/GetFelix/felix-agents/issues/148)).

## MCP tools

`felix-mcp` runs on the agent's side. The work tools call the tracker with the
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
| `work_context` | `/v1/work/context` and `/v1/work/context/add` | Tracker rows. Artifacts are shown only if the bytes hash to the id (`mcp/src/tasks.rs:484`) |
| `work_progress` | `/v1/work/progress` | Tracker row |
| `work_finding` | `/v1/work/finding` | Tracker row. The tool also limits `severity` and `verdict` to their words (`mcp/src/server.rs:768`) |
| `work_verdict` | `/v1/work/verdict` | Tracker row. The tool limits `verdict` to merge or block |
| `decide` | `/v1/work/decide` | Tracker row |
| `record_step` | `/v1/work/step` | Tracker row |
| `edit` | `/v1/work/edit` | Tracker row |
| `work_ask` | `/v1/work/ask` | Tracker row |
| `work_answer` | `/v1/work/answer` | Tracker row |
| `work_rules` | `/v1/work/rules/read` | Tracker row |
| `run_info` | Nothing | Reads its own settings |
| `task_post` | The run's tasks stream and run log | **None** beyond access to the run ([#29](https://github.com/GetFelix/felix-agents/issues/29)) |
| `task_claim` | The run's `workers` group | Felix hands a task to one member. The two-attempt limit is in `felix-mcp` only (`mcp/src/tasks.rs:250`, [#39](https://github.com/GetFelix/felix-agents/issues/39)) |
| `task_complete` | The results cache, artifacts and run log | First result wins by a put-if-absent in Felix. Who may write: **None** (#29). Task bodies are not hash-checked ([#150](https://github.com/GetFelix/felix-agents/issues/150)) |
| `task_release` | The `workers` group and run log | `felix-mcp` only releases the task it holds (`mcp/src/tasks.rs:584`). In Felix: **None** (#29) |
| `task_give_up` | The `workers` group and run log | Same |

## Records on work.events

Only the tracker appends. "Agent" below means a record written for an agent's
token, "tracker" one the tracker or ingest wrote, "person" one from a person's
token. A kind an actor may not write is refused with `actor_not_allowed`.

| Kind | Who may write | Check |
|---|---|---|
| `imported` | Tracker, person | Not an agent; one per upstream id (`core/src/work_fold.rs:763`) |
| `proposed` | All | An agent needs to hold the parent, and draws budget (`core/src/work_fold.rs:1872`). A `review` item only from the tracker, under an open root that reports the same request, 8 per root (`core/src/work_fold.rs:1707`, `core/src/work_fold.rs:1733`) |
| `triaged` | Person, or the agent holding the item's triage child | `core/src/work_fold.rs:1830`. No agent route writes it yet |
| `accepted` | Tracker, person | `core/src/work_fold.rs:820` |
| `prerequisite` | The holding agent | `core/src/work_fold.rs:1644` |
| `shipped` | Tracker, person | Only a done item (`core/src/work_fold.rs:871`). Ingest's list leaves it out |
| `linked` | All | An agent only inside one tree (`core/src/work_fold.rs:1874`) |
| `unlinked` | Tracker, person | `core/src/work_fold.rs:904` |
| `context` | All | An agent only on work it holds (`core/src/work_fold.rs:913`). Entry ownership: **None** (#152) |
| `finding` | The agent holding a review item | `core/src/work_fold.rs:1769`. Bounded and capped as the route above |
| `review_verdict` | The agent holding a final review | `core/src/work_fold.rs:1297`, `core/src/work_fold.rs:1303`. Open fixes block a merge (`core/src/work_fold.rs:1313`) |
| `claimed` | Agent, written by the tracker on a delivery | `core/src/work_fold.rs:1016` |
| `progress` | Holder | `core/src/work_fold.rs:1000` |
| `done` | Holder | `core/src/work_fold.rs:1034` |
| `released` | Holder, or the tracker on a lapse | `core/src/work_fold.rs:1067` |
| `gave_up` | Holder | `core/src/work_fold.rs:1083` |
| `question` | The holding agent | `core/src/work_fold.rs:1101` |
| `answer` | Person, or the agent holding a question item | `core/src/work_fold.rs:1888` |
| `decision` | The holding agent | `core/src/work_fold.rs:1164` |
| `step` | The holding agent | `core/src/work_fold.rs:1176` |
| `edit` | The holding agent | `core/src/work_fold.rs:1189` |
| `human_input` | Tracker, person | `core/src/work_fold.rs:1200` |
| `forked_from` | Tracker, person | Redo limit per item (`core/src/work_fold.rs:1214`) |
| `closed` | Tracker, person | The tracker closes as merged only on a merged status (`core/src/work_fold.rs:1259`) |
| `rejected` | Tracker, person | `core/src/work_fold.rs:1283` |
| `updated` | Tracker, person | Open roots only (`core/src/work_fold.rs:1511`) |
| `left_scope` | Tracker, person | Open roots only (`core/src/work_fold.rs:1511`) |
| `reopened` | Tracker, person | Closed roots only (`core/src/work_fold.rs:1302`). Ingest reopens only what it closed |
| `attempt_closed` | Tracker, person | `core/src/work_fold.rs:1322` |
| `tier_changed` | Person | `core/src/work_fold.rs:1410` |
| `edited` | Person | `core/src/work_fold.rs:1423` |
| `budget_set` | Person | `core/src/work_fold.rs:1339` |
| `pr_status` | Tracker, person | Only for the request the item reports, bounded and consistent (`core/src/work_fold.rs:1467`) |
| `source_stalled` | Tracker | 200-byte names, 64 repos (`core/src/work_fold.rs:1484`) |
| `rules_set` | Person | `core/src/work_fold.rs:1357` |
| `refused` | Tracker | `core/src/work_fold.rs:1445` |
| `format_raised` | Person | Only upward, only to a format the tracker's build reads, and stamped with that format (`core/src/work_fold.rs:446`). An older reader stops at it ([upgrades.md](upgrades.md)) |

## Records on the run log

A run log (`agents.log.<run>`) is written straight to Felix by `felix-mcp`,
`agents-run` and `agents-tool`, each with its own Felix token. Nothing
validates a record before it lands, and `by` is what the writer says. So a
run log is a set of claims, and the dashboard marks it unverified. Who may
write which kind: **None**, tracked by
[#29](https://github.com/GetFelix/felix-agents/issues/29) (roles per run),
[#45](https://github.com/GetFelix/felix-agents/issues/45) (the publisher Felix
recorded) and [#32](https://github.com/GetFelix/felix-agents/issues/32) (a
grants test with a raw client).

| Kinds | Meant to be written by |
|---|---|
| `run_started` | `agents-tool` |
| `run_ended`, `agent_up`, `model_started`, `model_call`, `budget_exhausted`, `refund_unconfirmed` | `agents-run`, with the runtime token the model cannot reach |
| `agent_gone` | The planner's `felix-mcp` |
| `task_posted`, `task_claimed`, `task_extended`, `task_released`, `task_done`, `task_duplicate`, `task_dead`, `task_claim_lost` | An agent's `felix-mcp` |
| `board_put`, `board_conflict`, `lease_taken`, `lease_renewed`, `lease_released`, `lease_lost`, `message`, `refused` | An agent's `felix-mcp` |

## Attack classes found in review

| Class | Example | Where it is stopped |
|---|---|---|
| Steering a field the agent does not own | Set rules, tiers or budgets; name itself on a record; move work to another repo | Role and name come from the token; person-only kinds; `moves_to` refused |
| Cross-tree links | Make another tree wait on its item | `/v1/work/link` is one tree only; a prerequisite needs a grant |
| Writes to work it does not hold | Context, progress or an answer on another agent's item | Holder checks in the fold on every agent record |
| Unbounded writes per attempt | Loop on `decide` or `record_step` to fill the log | A cap per kind per attempt, two claims per attempt, and one logged refusal |
| Forged artifacts and statuses | Bytes under another id; a green check; an approval; a release | Hash check on read; statuses come from ingest only; `shipped` is not an agent's |
| Merges into other branches | Merge into a side branch and report the item merged | A merge counts only into the default branch, by someone else |
| Budget escapes | Free questions, free prerequisites, resume work past the depth limit | Cost is charged on ask and prerequisite; depth applies to resume work. Reported cost is still the agent's word (#155) |
| Forged webhooks | Wake ingest in a loop | Signature and spacing. A captured body can be replayed under a new delivery id, and it only wakes the poll |

## Gaps

| Issue | Gap |
|---|---|
| [#152](https://github.com/GetFelix/felix-agents/issues/152) | An agent can replace a context entry ingest or a person wrote |
| [#153](https://github.com/GetFelix/felix-agents/issues/153) | A reported cost can overflow an item's total |
| [#154](https://github.com/GetFelix/felix-agents/issues/154) | Some strings are bounded only by the 2 MB body limit |
| [#155](https://github.com/GetFelix/felix-agents/issues/155) | The cost budget rests on what the agent reports |
| [#156](https://github.com/GetFelix/felix-agents/issues/156) | A prerequisite's source is not checked against grants |
| [#157](https://github.com/GetFelix/felix-agents/issues/157) | Evidence can cite any record |
| [#158](https://github.com/GetFelix/felix-agents/issues/158) | The tracker and the webhook serve plain HTTP |
| [#109](https://github.com/GetFelix/felix-agents/issues/109) | Proposals draw no budget; post keys can be reused across claims |
| [#120](https://github.com/GetFelix/felix-agents/issues/120) | No roles: every person can do everything, every agent can claim any repo's work |
| [#121](https://github.com/GetFelix/felix-agents/issues/121) | Tokens are long-lived |
| [#148](https://github.com/GetFelix/felix-agents/issues/148) | Request names are not canonical; check runs are not filtered by author |
| [#150](https://github.com/GetFelix/felix-agents/issues/150) | Task bodies are shown without a hash check |
| [#29](https://github.com/GetFelix/felix-agents/issues/29), [#32](https://github.com/GetFelix/felix-agents/issues/32), [#45](https://github.com/GetFelix/felix-agents/issues/45) | Any writer in a run can write any run log record under any name |
