# Example launch profiles

These are examples for the supervisor's `KBL_SUPERVISE_CONFIG` (see [design/supervise.md](design/supervise.md)).
Vendors change their flags; check each one against the vendor's current docs before you use it. KBL assumes no
vendor: a profile is an argv array and an MCP config format, nothing more.

Every profile below runs without a shell. `{model}`, `{worker}`, `{mcp_config_path}` and `{workdir}` are the only
placeholders. The worker gets an empty environment plus `pass_env`, so list what the vendor's CLI needs to log in
(an API key variable, or `HOME` for a saved login).

## Claude Code

```toml
[profiles.claude]
kind = "command"
vendor = "anthropic"
model = "sonnet"
mcp_config = "json"
workdir = "/work/{worker}"
pass_env = ["PATH", "HOME", "ANTHROPIC_API_KEY"]
command = [
  "claude", "-p", "Take work with the kbl work_next tool and do it, until it hands you none.",
  "--model", "{model}",
  "--mcp-config", "{mcp_config_path}",
  "--strict-mcp-config",
]
```

`--strict-mcp-config` keeps the worker to the one server in its config. Add `--allowedTools` for the tools the
worker may use without asking, as the Claude Code docs describe.

## Codex CLI

Codex reads MCP servers from `config.toml` under `CODEX_HOME`, so the profile writes the worker's config there.

```toml
[profiles.codex]
kind = "command"
vendor = "openai"
model = "gpt-5-codex"
mcp_config = "toml"
mcp_config_file = "{workdir}/.codex/config.toml"
workdir = "/work/{worker}"
set_env = { CODEX_HOME = "{workdir}/.codex" }
pass_env = ["PATH", "HOME", "OPENAI_API_KEY"]
command = [
  "codex", "exec", "--model", "{model}", "--cd", "{workdir}",
  "Take work with the kbl work_next tool and do it, until it hands you none.",
]
```

A separate `CODEX_HOME` also hides the saved login under `~/.codex`, so this example logs in with an API key.

Keep `mcp_config_file` out of anything the worker commits or pushes: the file holds the worker's token until the
worker stops. `.codex/` here sits beside the checkout, not inside it.

## GitHub Copilot CLI

```toml
[profiles.copilot]
kind = "command"
vendor = "github"
model = "claude-sonnet-4.5"
mcp_config = "json"
workdir = "/work/{worker}"
pass_env = ["PATH", "HOME", "GH_TOKEN"]
command = [
  "copilot", "-p", "Take work with the kbl work_next tool and do it, until it hands you none.",
  "--model", "{model}",
  "--additional-mcp-config", "@{mcp_config_path}",
  "--allow-all-tools",
]
```

## The fake profile

Tests and CI use a scripted MCP client instead of a vendor. It reads the worker's JSON config, starts the `kbl-mcp`
it names, completes the items `work_next` hands it, and exits when there are none.

```toml
[profiles.fake]
kind = "command"
vendor = "fake"
model = "scripted"
mcp_config = "json"
pass_env = ["PATH"]
command = ["node", "dev/fake-worker.mjs", "{mcp_config_path}"]
```
