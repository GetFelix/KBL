# Supervise: one agent runs a pool of worker agents

An engineer asks one agent to "supervise 10 agents on Felix issues". That agent peeks at the queue, starts worker
agents of any vendor, and tops the pool up as workers finish. KBL stays non-agentic: `kbl-mcp` and the tracker never
call a model. They launch configured commands and apply configured tables; judgment stays with the agents.

Issue [#420](https://github.com/GetFelix/KBL/issues/420). This page is the design; what is built so far is marked.

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

## Launch profiles (planned)

`kbl-mcp` config, one entry per profile:

```toml
[profiles.claude]
kind = "command"
vendor = "anthropic"
# Example only: check the flags against the vendor's current docs.
command = ["claude", "-p", "--model", "{model}", "--mcp-config", "{mcp_config_path}"]
mcp_config = "json"      # or "toml"
workdir = "/work/{worker}"

[profiles.cloud]
kind = "hook"
url = "https://hooks.example/start"
```

- Templates fill only `{model}`, `{worker}`, `{mcp_config_path}` and `{workdir}`, each as a whole argv element. No
  shell. Values come from config and the tracker's checked names, never from item text.
- The per-worker MCP config holds `KBL_AGENT={worker}` and that worker's token only, written mode 600 and removed when
  the worker stops. The supervisor's token is never passed to a worker.
- `kind = "hook"` posts the worker name and a one-time code; the hosted agent exchanges the code for its token once.
- A fake profile (a scripted MCP client from `dev/`) runs the tests and CI without any vendor.

## Fit, tools and top-up (planned)

- A table from recorded fields (tier, kind, type, area, size, needs) to `{profile, model}`, first match wins, with a
  per-launch override. Each worker records vendor, profile and model.
- MCP tools: `supervise_start {count, filter, profile?, model?}`, `supervise_stop {worker | all}`, `supervise_status`.
- Top-up: on a timer, `kbl-mcp` keeps N workers while `work_peek` shows ready work matching the filter.
  `supervise_status` shows workers, holds, finished and failed since the last look, and the queue shape.
