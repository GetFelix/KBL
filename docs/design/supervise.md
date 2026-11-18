# Supervise: one agent runs a pool of worker agents

An engineer asks one agent to "supervise 10 agents on Felix issues". That agent peeks at the queue, starts worker
agents of any vendor, and tops the pool up as workers finish. KBL stays non-agentic: `kbl-mcp` and the tracker never
call a model. They launch configured commands and apply configured tables; judgment stays with the agents.

Issue [#420](https://github.com/GetFelix/KBL/issues/420). This page is the design; what is built is marked.

## Records (format 21, built)

| Record | Written by | What the fold checks |
|---|---|---|
| `worker_started {worker, token_sha256, supervisor_token_sha256, vendor?, profile?, model?}` | The supervisor's agent token | The writer is an agent and not itself a worker (one level). `worker` is `<supervisor>-<suffix>`, never used before. Both hashes are 64 hex digits; the worker's is unique. At most 64 live workers per supervisor |
| `worker_stopped {worker}` | The worker's supervisor, a person, or the tracker when the supervisor is gone | The worker is live. Its token stops working and its name is never reused |
| `released` with reason `supervisor_gone` | The tracker | The hold goes back and the try it used is given back |
| `sup` on any record | The tracker, from the token | Set exactly when `by` is a live worker, and then it must be that worker's supervisor (`bad_worker` otherwise). Any record with `sup` is format 21 |

Every hold, step and decision a worker writes therefore names the worker (`by`) and its supervisor (`sup`). The
tracker keeps one hold per agent name, so a worker holds at most one item.

## Names and identity (built)

A worker's name is its supervisor's name, `-`, and a suffix of letters, digits and `_`, such as `w1-a`. Agent names
never contain `-`, so:

- No agent, existing or added later, can have a worker's name, and no worker can have an agent's. From format 21 the
  fold refuses an agent record under a name with a `-` unless it is that live worker writing for its supervisor.
- The part before the first `-` is the one supervisor a name can belong to: `w1_x-a` is `w1_x`'s, never `w1`'s.
- A stopped worker's name is never used again, by a worker or an agent.

A hold belongs to the name that took it, and only that name acts on it: not the supervisor, and not a sibling worker.

A worker is bound to its supervisor's identity, not just its name: `worker_started` logs the hash of the token the
supervisor called with. The worker's token works only while that name still has that token, in the token file or on
the org log. A supervisor that is removed and added again under the same name with a new token does not get its old
workers back.

## Token flow (built)

1. The supervisor's `kbl-mcp` calls `POST /v1/work/workers {worker, vendor?, profile?, model?}` with its own agent
   token.
2. The tracker checks its caps (`KBL_MAX_WORKERS` per supervisor, default 8; `KBL_MAX_TEAM_WORKERS` per team, default
   32), draws a 256-bit token, logs `worker_started` with its SHA-256 only, and returns the token once.
3. The token works once the record is on the log, and only while the worker is live and its supervisor still has the
   token it started the worker with. A restart rebuilds the token map from the log.
4. `POST /v1/work/workers/stop {worker}` logs `worker_stopped`; the token fails from then on. The worker's hold goes
   back when its claim runs out, as a crashed agent's does.
5. When the supervisor goes (revoked, removed from the token file, or given another token), the tracker stops its
   workers for good: it logs `worker_stopped` for each, then releases each hold with `supervisor_gone`, which costs
   the item no try. Revoking an agent does this at once; otherwise each team checks every 5 seconds. While the org
   log cannot answer, nothing is stopped, since silence is not evidence that the supervisor is gone.

A supervisor that sees `replayed: true` on a start reply got a token it was already given once; it should stop that
worker and start one under a new name.

## Caps (built)

- A worker acts with its supervisor's grants (the token file's `grants` are looked up by the supervisor's name), so
  fanning out cannot reach a repo the supervisor cannot.
- Budgets in KBL are per root tree, so workers already draw from the same tree budgets as any agent. The supervisor's
  own cap is its worker count; cost per supervisor is derived from `sup` on the reports.
- Hard caps: per supervisor and per team in the tracker, and 64 per supervisor in the fold whatever the config says.

## Launch profiles (built)

The supervisor's `kbl-mcp` reads a TOML file named by `KBL_SUPERVISE_CONFIG`. Without it the `supervise_*` tools
answer that they are not set up. One entry per profile:

```toml
state_dir = "/var/lib/kbl/supervise"   # per-worker MCP configs and logs; default ~/.local/state/kbl/supervise
default_profile = "claude"
top_up_seconds = 30

[profiles.claude]
kind = "command"
vendor = "anthropic"
model = "sonnet"
command = ["claude", "-p", "Take work with work_next until there is none.", "--model", "{model}",
           "--mcp-config", "{mcp_config_path}", "--strict-mcp-config"]
mcp_config = "json"
workdir = "/work/{worker}"
pass_env = ["PATH", "HOME"]

[profiles.cloud]
kind = "hook"
vendor = "example"
url = "https://hooks.example/start"
hook_token_env = "CLOUD_HOOK_TOKEN"
```

- `command` is an argv array, run without a shell. Its elements may hold `{model}`, `{worker}`, `{mcp_config_path}`
  and `{workdir}`; any other `{...}` is refused when the file is read, and the program (the first element) takes no
  placeholder. Every value is checked before it is filled in: the worker name by the tracker's name rules, the model
  against `A-Z a-z 0-9 . _ : / @ -` (never a leading `-`), the paths built by `kbl-mcp`. Item text never reaches a
  template.
- The worker process starts with an empty environment plus the names in `pass_env`, copied from the supervisor's.
  `KBL_TRACKER_TOKEN`, `KBL_PERSON_TOKEN` and `KBL_FELIX_TOKEN` may not be listed, so the supervisor's tokens never
  reach a worker; no `KBL_` name may be listed at all. `set_env` adds fixed values, which may use `{worker}` and
  `{workdir}`. `mcp_config_file` writes the MCP config to a fixed place the vendor reads (with `{worker}` and
  `{workdir}`) instead of the state directory.
- The per-worker MCP config (`json`: `mcpServers.kbl`; `toml`: `mcp_servers.kbl`) starts `kbl-mcp` with
  `KBL_AGENT={worker}`, that worker's token, the run, the tracker and broker addresses, and the pool's filter as
  `KBL_WORK_FILTER`. It is written mode 600 in a mode 700 directory and removed when the worker stops. The worker's
  `kbl-mcp` applies `KBL_WORK_FILTER` to every `work_next` and `work_peek`, over whatever the agent asks for.
- Felix: an ID token source (`KBL_FELIX_ID_TOKEN_FILE` or `_COMMAND` with `KBL_FELIX_CONTROL_PLANE`) is passed by
  name. A fixed `KBL_FELIX_TOKEN` is copied into the worker's config only with `share_felix_token = true`.
- Output goes to `<state_dir>/<worker>/out.log`, never to the supervisor's stdout, which belongs to MCP.
- Each worker runs in a process group of its own, and a stop kills the group, so what the vendor's CLI started
  (its `kbl-mcp` among them) ends with it. A stop is logged on the tracker first, so the token is dead before the
  kill; a stop the tracker refuses for any reason but `bad_worker` (already stopped) is sent again each round.
- `kind = "hook"` asks the tracker for a one-time code instead of a token and posts `{worker, code, tracker_url,
  vendor, profile, model, filter}` to the URL, with a bearer from `hook_token_env` if set. The hosted agent calls
  `POST /v1/work/workers/exchange {code}` once and gets the worker's token. A code works once and for 10 minutes,
  and lives only in the tracker's memory: after a restart it is gone, so that worker never connects and holds
  nothing until it is stopped. A hook that does not answer 2xx stops the worker at once. A hook worker counts as
  live until it is stopped, and never as idle.
- A fake profile (`dev/fake-worker.mjs`, a scripted MCP client) runs the tests and CI without any vendor.

Example profiles for Claude Code, Codex CLI and GitHub Copilot CLI are in [supervise-profiles.md](../supervise-profiles.md).

## Fit (built)

`[[fit]]` rows map recorded fields to a profile and model, first match wins:

```toml
[[fit]]
tiers = [0, 1]
profile = "claude"
model = "opus"

[[fit]]
types = ["docs"]
profile = "codex"
```

A row may list `tiers`, `kinds`, `types`, `areas`, `sizes` and `needs`. Every field it lists must match (any value
of the list); `needs` matches when the item needs any of them. A row without `model` uses the profile's. No match
falls back to `default_profile`. For each launch, the fit reads the next ready item in `work_peek` order that no
launch in the same round has taken, so `n` launches fit the first `n` ready items. The worker still claims with
`work_next`, so it may get another item if the queue changes first. `profile` and `model` on `supervise_start`
override the table for every launch of that pool. Each worker records vendor, profile and model.

## Tools and top-up (built)

- `supervise_start {count, filter, profile?, model?}` sets the pool to `count` workers on `filter` (the narrowing
  fields of `work_next`), starts up to `count` minus the live ones now, at most one per ready item, and turns the
  top-up on.
- `supervise_stop {worker}` stops one worker and lowers the pool by one; `{all: true}` stops every worker and turns
  the top-up off. A stop logs `worker_stopped` first, so the token dies before the process is killed.
- `supervise_status` shows the workers (with holds), the workers that finished (exit 0) or failed since the last
  call, the queue shape (ready counts by kind and tier from `work_peek`), and the top-up state.
- Top-up: every `top_up_seconds` (5 to 3600), `kbl-mcp` reaps exited workers (stopping them on the tracker), then
  starts workers until the pool is full or there is no more ready work for them. It stops for the round on
  `worker_cap`, and pauses after 3 workers in a row fail or exit within `min_run_seconds` (default 30) of starting,
  or 3 launches in a row fail; `supervise_start` resumes it. A worker that holds nothing yet counts against the
  ready items, so a slow starter does not make the top-up start a second worker for the same item.
- Every round, a supervisor's `kbl-mcp` stops the live workers the tracker lists for it that its pool does not
  track: ones an earlier process left, or a start whose answer was lost. At most 8 workers start per round, so a
  round, which holds the pool, stays short even when hooks are slow.
