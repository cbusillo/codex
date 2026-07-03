# Agents & Subagents

Every Code can launch external CLI “agents” and orchestrate them in multi-agent “subagent” flows such as `/plan`, `/solve`, and `/code`.

## Agent configuration (`[[agents]]` in `config.toml`)
```toml
[[agents]]
name = "code-gpt-5.5"       # slug or alias shown in pickers
command = "coder"                # executable; defaults to name
args = ["--foo", "bar"]          # base argv
args_read_only = ["-s", "read-only", "-a", "never", "exec", "--skip-git-repo-check"]
args_write = ["-s", "workspace-write", "--dangerously-bypass-approvals-and-sandbox", "exec", "--skip-git-repo-check"]
env = { CODE_FOO = "1" }
read_only = false                 # force RO even if session allows writes
enabled = true                    # hide from pickers when false
description = "Frontline coding agent"
instructions = "Preamble added to this agent’s prompt"
```
Field recap: `name` (slug/alias), `command` (absolute paths ok), `args*` (RO/RW lists override base), `env`, `read_only`, `enabled`, optional `description` and `instructions`.

### Built-in defaults
If no `[[agents]]` are configured, Every Code advertises built-in agent/model selectors (gated by env `CODE_ENABLE_CLOUD_AGENT_MODEL` for cloud variants): `code-gpt-5.5`, `code-gpt-5.4`, `code-gpt-5.4-mini`, `claude-opus-4.8`, `antigravity`, `claude-sonnet-4.6`, `claude-haiku-4.5`, `qwen3-coder-plus`, `cloud-gpt-5.1-codex-max`. Built-ins strip any user `--model/-m` flags to avoid conflicts and inject their own when the target CLI supports model flags.

`code-gpt-5.4` is the GPT selector to reach for when correctness or very large context matters. In Every Code, GPT-5.4 defaults to the expensive 1m-token context path (`context_mode = "auto"`) so long histories and broad repository sweeps can survive. Suggest it only when preserving that context is worth the added cost; use `context_mode = "disabled"` to keep GPT-5.4 on its standard context window.

Tip: `antigravity` uses Google's Antigravity CLI (`agy`) as the Google/Gemini-family agent path. Gemini/Google intent can resolve to `antigravity`, but AGY uses its configured model rather than a per-run Gemini Pro/Flash flag. Consumer Gemini CLI is no longer a built-in default; configure it manually only when you intentionally rely on enterprise/API-key Gemini CLI access.

## Subagents (`[[subagents.commands]]`)
```toml
[[subagents.commands]]
name = "plan"                     # slash name (/plan, /solve, /code, or custom)
read_only = true                  # default plan/solve=true, code=false
agents = ["code-gpt-5.4", "claude-opus-4.8"]  # falls back to enabled agents or built-ins
orchestrator_instructions = "Guidance for the Every Code agent before spawning agents"
agent_instructions = "Preamble added to each spawned agent"
```
- `name`: slash command created/overridden.
- `read_only`: forces spawned agents to RO when true.
- `agents`: explicit list; empty → enabled `[[agents]]`; none configured → built-in roster.
- `orchestrator_instructions`: appended to the Every Code agent's prompt before issuing `agent.create`.
- `agent_instructions`: appended to each spawned agent prompt.

The orchestrator fans out agents, waits for results, and merges reasoning according to your `hide_agent_reasoning` / `show_raw_agent_reasoning` settings.

### Preloaded context files

`files` remains a lightweight list of paths the subagent should consider. It does not inline file contents. Use `context_files` in an `agent.create` call when the subagent must receive selected text file contents in its initial prompt:

```json
{
  "files": ["src/"],
  "context_files": [".code/context/large-context-bundle.txt"],
  "context_budget_tokens": 700000,
  "models": ["code-gpt-5.4"]
}
```

Large `context_files` launches are intentionally expensive. The runtime only inlines regular UTF-8 text files inside the workspace, estimates the inlined context size, defaults to a conservative budget, and fails fast unless `context_budget_tokens` is high enough. Use `code-gpt-5.4` for very large curated context; it is the built-in GPT path intended for 1m-context work. For strict one-shot rollout/model evaluation, prefer `code llm request --message-file` so the call stays out of agent mode.

When you ask the Every Code agent to "ask agents" or gather dissent, it should prefer a small, diverse batch when the task benefits from multiple viewpoints and budget allows. A typical diverse batch includes GPT, Claude, and `antigravity` for the Google/Gemini-family perspective. Multi-agent release/workflow, infrastructure, security, and product-risk work should proactively include `antigravity` unless there is a clear reason to skip it. Narrow mechanical work can use fewer agents; if an obvious family is skipped, the agent should briefly say why.

## TUI controls
- `/agents` opens the settings overlay to the Agents section: toggle enabled/read-only, view defaults, and open editors.
- Agent editor: create or edit a single agent (enable/disable, read-only, instructions). Args/env come from `config.toml`.
- Subagent editor: configure per-command agent lists, read-only flag, and instructions. Built-in `/plan` `/solve` `/code` can be overridden the same way.
- Model pickers are modal and return to the invoking section after selection.

## Goal-mode interaction
- Standalone Auto Drive is retired; do not add Auto Drive-specific agent toggles
  or `AUTO_AGENTS.md` loading.
- Goal-mode agent delegation uses the normal Every Code agent settings and the
  existing read-only safeguards.

## AGENTS.md and project memory
- Every Code loads AGENTS.md files along the path (global, repo root, cwd) up to 32 KiB total; deeper files override higher-level ones.
- Contents become system/developer instructions on the first turn; direct user/developer prompts still take precedence.

## Windows discovery tips
- On Windows, include extensions in `command` (`.exe`, `.cmd`, `.bat`, `.com`).
- NPM globals often live under `C:\\Users\\<you>\\AppData\\Roaming\\npm\\`.
- If PATH is unreliable, use absolute `command` paths in `[[agents]]`.

## Notifications and reasoning visibility
- `hide_agent_reasoning = true` removes agent reasoning streams in both the TUI and `code exec`.
- `show_raw_agent_reasoning = true` surfaces raw chains-of-thought when provided by the model.
- Notification filtering is controlled via `/notifications` or `config.toml` `notify` / `tui.notifications`.

## Automatic retries
- Agent provider failures that look transient, such as overloads, rate limits,
  timeouts, temporary upstream errors, and transport resets, are retried
  automatically with bounded backoff.
- Auth, configuration, missing-command, policy, and cancellation failures fail
  fast. Retry attempts appear in agent progress and result metadata.

## Headless `code exec`
- `code exec --json` streams JSONL events (agent turns included).
- `--output-schema <schema.json>` enforces structured JSON output; combine with `--output-last-message` to capture only the final payload.
- `code exec` defaults to read-only; use a writable sandbox and the explicit
  bypass flag only for agents that are allowed to edit.

## Quick examples
- Custom agent:
```toml
[[agents]]
name = "my-coder"
command = "/usr/local/bin/coder"
args_write = ["-s", "workspace-write", "--dangerously-bypass-approvals-and-sandbox", "exec", "--skip-git-repo-check"]
enabled = true
```
- Custom context sweep command:
```toml
[[subagents.commands]]
name = "context"
read_only = true
agents = ["code-gpt-5.4", "claude-opus-4.8"]
orchestrator_instructions = "Have each agent summarize the most relevant files and tests."
agent_instructions = "Return paths plus 1–2 sentence rationale; do not edit files."
```
