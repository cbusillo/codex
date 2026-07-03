# Code Exec Harness

`tools/code-exec-harness/harness.py` runs isolated `code exec --json` scenarios
and saves compact evidence under `.tmp/code-exec-harness/`.

Use it to compare Every Code behavior across prompt, memory, skill, model, and
configuration variants without writing to real GitHub or reusing the real
`CODE_HOME`.

Agent-specific testing policy lives in `AGENTS.md` in this directory. In short,
use this harness as the proving ground for effectiveness-adjusted token
efficiency work: prefer fake `/v1/responses` assertions first, use live tokens
when model behavior is the question, and preserve evidence that future runs can
compare.

The harness defaults `code exec` to `danger-full-access` because external tool
shims such as fake `gh` need to write logs and state outside the fixture
workspace. Run it only with trusted scenarios.

By default, each scenario gets an empty `CODE_HOME`. Pass `--inherit-auth` only
when you want a live model-backed run; it copies Code auth and GitHub CLI config
into the isolated run home without copying the rest of your config.

`HOME`, `ZDOTDIR`, `XDG_CONFIG_HOME`, and `XDG_CACHE_HOME` are also redirected
inside the run directory so shell startup files and home-directory tooling do
not silently use the real user profile. When auth is inherited, `GH_CONFIG_DIR`
points at the copied GitHub CLI config inside that redirected home.

## Run

Run the deterministic no-token smoke suite after `./build-fast.sh`:

```sh
just harness-smoke
```

`just harness-smoke` targets `code-rs/target/dev-fast/code` by default so the
suite validates the binary built by this checkout, not whichever `code` happens
to be on `PATH`. To test another binary, set `CODE_EXEC_HARNESS_BIN`:

```sh
CODE_EXEC_HARNESS_BIN=/path/to/code just harness-smoke
```

The deterministic suite uses fake `/v1/responses` fixtures and does not spend
live model tokens. It is the Dogfood Parity 1 P0 gate for `code exec --json`:
if the built binary is missing, cannot start, stops emitting expected JSONL, or
regresses the covered request-shape contracts, this suite should fail.

Run a single scenario directly when you need narrower evidence:

```sh
python3 tools/code-exec-harness/harness.py \
  tools/code-exec-harness/scenarios/exec-basic-smoke.json \
  --code-bin code-rs/target/dev-fast/code
```

Run the live GitHub planning smoke only when model behavior is the thing being
tested:

```sh
python3 tools/code-exec-harness/harness.py \
  tools/code-exec-harness/scenarios/github-plan-smoke.json \
  --skill-root /Users/cbusillo/Developer/codex-skills \
  --inherit-auth
```

Each run writes:

- `artifacts/stdout.jsonl`: raw `code exec --json` events
- `artifacts/stderr.log`: stderr from the run
- `artifacts/summary.json`: final answer, token usage, tool commands, fake `gh`
  calls, fake GitHub state, and expectation failures
- `artifacts/gh-calls.jsonl`: fake `gh` invocations when the scenario enables it
- `artifacts/gh-state.json`: fake issue state after the run

## Scenario Shape

Scenarios are JSON files. Common fields:

- `prompt`: prompt passed to `code exec`
- `files`: workspace files to create before the run
- `skill_roots`: skill roots copied or symlinked into isolated `CODE_HOME/skills`
- `gh`: fake GitHub fixture; when present, the harness prepends a fake `gh` to
  `PATH`
- `config_toml`: isolated `CODE_HOME/config.toml` contents
- `config_overrides`: `-c key=value` arguments passed to `code exec`
- `inherit_auth`: copy Code auth and GitHub CLI config for this scenario
- `turns`: run multiple `code exec` turns in one isolated `CODE_HOME`; later
  turns resume the first turn's session id
- `responses_api`: start a local fake `/v1/responses` server and point the run
  at it with a dummy API key, allowing request-body assertions without spending
  live model tokens. When `responses_api` is set, inherited Code auth is
  suppressed even if `inherit_auth` is requested, but inherited GitHub CLI
  auth still applies.
- `expect`: simple assertions over the final answer, commands, fake `gh` calls,
  and exit code
- `expect.workspace_files`: assertions over files in the isolated workspace after
  the run, including `exists`, `equals`, `contains`, `contains_all`, and
  `not_contains`
- `expect.workspace_git_status`: assertions over `git status --porcelain` in the
  isolated workspace, including `equals`, `contains_all`, and `not_contains`

The harness is intentionally black-box: the unit under test is the real `code
exec` binary and its emitted JSONL stream.
