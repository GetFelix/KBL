# Example launch profiles

These are examples for the supervisor's `KBL_SUPERVISE_CONFIG` (see [design/supervise.md](design/supervise.md)).
Vendors change their flags; check each one against the vendor's current docs before you use it. KBL assumes no
vendor: a profile is an argv array and an MCP config format, nothing more.

Every profile below runs without a shell. `{model}`, `{worker}`, `{mcp_config_path}` and `{workdir}` are the only
placeholders. The worker gets an empty environment plus `pass_env`, so list what the vendor's CLI needs to log in
(an API key variable, or `HOME` for a saved login). On macOS, `claude` also needs `USER` to read its saved login from
the Keychain; without it every worker exits within seconds with "Not logged in". Use `USER`, or `ANTHROPIC_API_KEY`
instead. Add `LOGNAME` or `TMPDIR` if a vendor's CLI asks for them.

## Commit identity and vendor attribution

A worker's commits carry whatever identity git finds, which on a laptop is the global one, often a work email. Some
vendor CLIs also add a `Co-Authored-By` trailer to commits and a "generated with" footer to pull requests. A target
repo's CLA check or contribution policy can reject both, and a supervised run of four PRs was closed for exactly that.

Set the identity per profile with `git_identity = { name, email }`. `kbl-mcp` turns it into `GIT_AUTHOR_NAME`,
`GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL` for that worker's process, and refuses an email that
is not one when the config loads. The same four names in `set_env` do the same by hand, and a `set_env` entry wins over
`git_identity` for the name it sets. Then turn the vendor's attribution off, as each section below shows. The worker's
`worker_started` record does not carry the identity (that would need a new tracker format), so keep it in the profile.

## Claude Code

```toml
[profiles.claude]
kind = "command"
vendor = "anthropic"
model = "sonnet"
mcp_config = "json"
workdir = "/work/{worker}"
pass_env = ["PATH", "HOME", "USER", "ANTHROPIC_API_KEY"]
git_identity = { name = "Your Name", email = "you@example.com" }
command = [
  "claude", "-p", "Take work with the kbl work_next tool and do it, until it hands you none.",
  "--model", "{model}",
  "--mcp-config", "{mcp_config_path}",
  "--strict-mcp-config",
  "--settings", "/work/claude-settings.json",
]
```

with `/work/claude-settings.json` holding

```json
{ "includeCoAuthoredBy": false, "attribution": { "commit": "", "pr": "" } }
```

`includeCoAuthoredBy: false` is the older switch for the commit trailer and `attribution.commit` / `attribution.pr`
(empty strings) replace it in newer Claude Code, which also covers the pull request footer. Older versions ignore
`attribution` and newer ones still accept `includeCoAuthoredBy` but may mark it deprecated, so keep both and check
Claude Code's settings docs for the version you run.

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
pass_env = ["PATH", "HOME", "USER", "OPENAI_API_KEY"]
command = [
  "codex", "exec", "--model", "{model}", "--cd", "{workdir}",
  "Take work with the kbl work_next tool and do it, until it hands you none.",
]
```

Add `git_identity` to this profile as in the Claude Code one. Codex CLI has no attribution setting this page could
verify; check its docs and a test commit before relying on it.

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
pass_env = ["PATH", "HOME", "USER", "GH_TOKEN"]
git_identity = { name = "Your Name", email = "you@example.com" }
command = [
  "copilot", "-p", "Take work with the kbl work_next tool and do it, until it hands you none.",
  "--model", "{model}",
  "--additional-mcp-config", "@{mcp_config_path}",
  "--allow-all-tools",
]
```

Copilot CLI's commit trailer and its switch to turn it off are not verified here; check its docs and a test commit.

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
set_env = { FAKE_WORKER_CONFIG_ROOT = "/path/to/state_dir" }
command = ["node", "dev/fake-worker.mjs", "{mcp_config_path}"]
```
