# Every Code Repository Guidance

Repo workflow metadata lives in `.github/github.json`; keep that
file aligned with branch roles, validation gates, GitHub signal capabilities,
workflow names, PR/release policy, docs routing, and local cleanup policy when
those facts change.

Every Code is the product in this repository; `code` is the command users type.
Use **Every Code** for the product name in prose, docs, UI copy, issue text,
release text, and first mentions. Use **Every Code CLI** for the product CLI
surface, and **the `code` command** when referring to the executable. Use **the
Every Code agent** for the assistant identity; **Code** is only a short display
name after Every Code context is established. Use **Every Code harness** for the
runtime/session wrapper that we restart and dogfood, and **Every Code runtime**
for process/session/tool-execution internals.

Upstream import sources are `just-every/code` and `openai/codex`. Treat
`just-every/code` as a fork upstream/import source. Treat `openai/codex` / Codex
CLI as the original/direct upstream and provenance source. The current product
architecture is Codex CLI as the substrate with Every Code layered on top.
Locally, `codex-rs` is a read-only mirror of `openai/codex:main`; edit Rust
sources under `code-rs`, the Every Code product workspace built on the Codex CLI
substrate.

`CODE_HOME` / `~/.code` are the primary config and state locations.
`CODEX_HOME` / `~/.codex` are compatibility fallbacks. Keep `CODEX_*` names when
they are part of external, backend, or upstream compatibility; add `CODE_*`
aliases or rename only through a scoped migration. See
`docs/upstream-import-policy.md#code-and-codex-compatibility-policy` before
changing environment variable behavior.

Rust implementation lives under `code-rs`:

- Crate names are ownership markers. Imported Codex substrate crates may keep
  their upstream `codex-*` names only when they remain compatibility-critical or
  mostly upstream-shaped. Every Code-owned crates and new product-layer crates
  should use `code-*` names unless a documented external compatibility contract
  requires the upstream spelling.
- When using format! and you can inline variables into {}, always do that.
- Treat `codex-rs` as a read-only mirror of `openai/codex:main`; edit Rust
  sources under `code-rs`, including imported Codex-based sources that become
  part of the Every Code product workspace.
- When aligning with upstream, prefer moving `code-rs` closer to current Codex
  CLI shapes and adding Every Code behavior as a small product-layer overlay.
  Do not make `codex-rs` mirror Every Code or edit it directly.

Completion/build step

- Always validate using `./build-fast.sh` from the repo root. This is the single required check and must pass cleanly.
- `./build-fast.sh` can take 20+min to run from a cold cache!!! Please use long timeout when running `./build-fast.sh` or waiting for it to complete.
- Policy: All errors AND all warnings must be fixed before you’re done. Treat any compiler warning as a failure and address it (rename unused vars with `_`, remove `mut`, delete dead code, etc.).
- Do not run additional format/lint/test commands on completion (e.g., `just fmt`, `just fix`, `cargo test`) unless explicitly requested for a specific task.
- ***NEVER run rustfmt***
- Before release-bound work lands on `main`, run `./pre-release.sh` to mirror the release preflight (dev-fast build, CLI smokes, workspace nextest).

Optional regression checks (recommended when touching the Rust workspace):

- `cargo nextest run --no-fail-fast` — runs all workspace tests with the TUI helpers automatically enabled. The suite is green after the resume fixtures/git-init fallback updates; older Git builds may print a warning when falling back from `--initial-branch`, but tests still pass.
- Focused sweeps stay quick and green: `cargo test -p code-tui --features test-helpers`, `cargo test -p code-cloud-tasks --tests`, and `cargo test -p mcp-types --tests`.

When debugging regressions or bugs, write a failing test (or targeted reproduction script) first and confirm it captures the issue before touching code—if it can’t fail, you can’t be confident the fix works.

## Documentation hygiene

- Keep docs clean, clear, and current; prune stale instructions instead of piling on caveats.
- Avoid excessive verbosity; prioritize concise guidance over long narratives.
- Do not document minor or non-core features; focus on system-critical flows and expectations.
- Never commit temporary "working" docs, plans, or scratch notes.

## Strict Ordering In The TUI History

The TUI enforces strict, per‑turn ordering for all streamed content. Every
stream insert (Answer or Reasoning) must be associated with a stable
`(request_ordinal, output_index, sequence_number)` key provided by the model.

- A stream insert MUST carry a non‑empty stream id. The UI seeds an order key
  for `(kind, id)` from the event's `OrderMeta` before any insert.
- The TUI WILL NOT insert streaming content without a stream id. Any attempt to
  insert without an id is dropped with an error log to make the issue visible
  during development.

## Commit Messages

- Review staged changes before every commit: `git --no-pager diff --staged --stat` (and skim `git --no-pager diff --staged` if needed).
- Write a descriptive subject that explains what changed and why. Avoid placeholders like "chore: commit local work".
- Prefer Conventional Commits with an optional scope: `feat(tui/history): …`, `fix(core/exec): …`, `docs(agents): …`.
- Keep the subject ≤ 72 chars; add a short body if rationale or context helps future readers.
- Use imperative, present tense: "add", "fix", "update" (not "added", "fixes").
- For merge commits, skip custom prefixes like `merge(main<-origin/main):`. Use a clear subject such as `Merge origin/main: <what changed and how conflicts were resolved>`.

Examples:

- `feat(tui/history): show exit code and duration for Exec cells`
- `fix(core/codex): handle SIGINT in on_exec_command_begin to avoid orphaned child`
- `docs(agents): clarify commit-message expectations`

## Upstream Import Workflow

- `main` is the Every Code product branch and the GitHub default branch.
- PR readiness is not merge intent. Use the shared `babysit-pr`/GitHub skills
  and `.github/github.json` PR workflow metadata when watching fix trains,
  auto-review lag, CI, review comments, and merge readiness.
- Before restarting or dogfooding the Every Code harness, run `just local-code-rebuild`
  so the PATH-resolved `code` command uses the current branch's binary.
- Use `just local-code-rebuild` whenever you need to rebuild the current branch into
  the PATH-resolved binary.
- After `./build-fast.sh`, run `just local-code-rebuild` again before release smoke checks; the fast build validates dev-fast artifacts, while the rebuild recipe owns the PATH-resolved release binary and embeds the `VERSION` file value.
- During active local work, run
  `just local-cleanup-space --apply --keep-current-fast-cache` when repo-local
  build caches grow large; this removes stale per-branch `./build-fast.sh`
  buckets while preserving the currently wired `code-rs/target/dev-fast/code`
  cache for warmer rebuilds.
- Before leaving a local work session, run `just local-cleanup-space --apply`
  only when you intentionally want a colder cleanup that removes all rebuildable
  target/cache artifacts while preserving `code-rs/target/release/code`.
- Use `just local-upstream-import` only from a clean `main` branch. The helper
  fetches `upstream/main`, merges it into the current Every Code branch, replays
  any commits listed in `scripts/local/upstream-picks.txt`, then rebuilds the
  release binary.
- Use `just local-upstream-cursors` to inspect the durable upstream review
  cursors in `.github/upstream-cursors.json`. `lastExamined` means the newest
  upstream commit whose delta has been reviewed or intentionally skipped;
  `lastImported` means the newest upstream commit actually merged, cherry-picked,
  or manually ported into Every Code. Do not infer either value from
  `git merge-base` alone.
- Advance `.github/upstream-cursors.json` only after reviewing the new range for
  that upstream. Keep cursor changes in the PR that performs the review/import
  so future sessions compare from the recorded `lastExamined` SHA instead of
  rereading older commits.
- Commits already on the product branch persist automatically across future
  upstream imports. They do not need to be duplicated in
  `scripts/local/upstream-picks.txt`.
- If you make a new Every Code fix that should remain part of the product,
  commit it through the normal task-branch/PR flow into the product branch
  first. Do not leave important local changes only as an unmerged side branch.
- Add a commit SHA to `scripts/local/upstream-picks.txt` only for a patch that
  intentionally lives outside the product branch and still needs to be
  cherry-picked in during `just local-upstream-import`. Keep the list ordered
  and comment each entry with the source branch or purpose.
- Old side branches are archival context, not the runtime source of truth. They
  do not need to merge cleanly as branches; only the specific carried commits
  must cherry-pick cleanly onto the current product branch.
- If a legacy pick no longer cherry-picks cleanly, leave it commented out in
  `scripts/local/upstream-picks.txt`, manually re-port the fix against current
  upstream, commit the new port on the product branch, then replace the old SHA
  in the manifest if you still want automatic replay.
- For Every Code-owned surfaces, release steps, remote names, and conflict hot spots, see `docs/upstream-import-policy.md`.

## How to Git Push

### Merge-and-Push Policy (Do Not Rebase)

When the user asks you to "push" local work:

- Never rebase in this flow. Do not use `git pull --rebase` or attempt to replay local commits.
- Prefer a simple merge of `origin/main` into the current branch, keeping our local history intact.
- If in doubt or if conflicts touch non-trivial areas, pause and ask before resolving.

Quick procedure (merge-only):

- Commit your local work first:
  - Review: `git --no-pager diff --stat` and `git --no-pager diff`
  - Stage + commit: `git add -A && git commit -m "<descriptive message of local changes>"`
- Fetch remote: `git fetch origin`
- Merge without auto-commit: `git merge --no-ff --no-commit origin/main` (stops before committing so you can choose sides)
- Resolve policy:
  - Default to ours: `git checkout --ours .`
- Stage and commit the merge with a descriptive message, e.g.:
  - `git add -A && git commit -m "Merge origin/main: adopt remote version bumps; keep ours elsewhere (<areas>)"`
- Run `./build-fast.sh` and then `git push`

## Command Execution Architecture

The command execution flow in Code follows an event-driven pattern:

1. **Core Layer** (`code-rs/core/src/codex.rs`):
   - `on_exec_command_begin()` initiates command execution
   - Creates `EventMsg::ExecCommandBegin` events with command details

2. **TUI Layer** (`code-rs/tui/src/chatwidget.rs`):
   - `handle_codex_event()` processes execution events
   - Manages `RunningCommand` state for active commands
   - Creates `HistoryCell::Exec` for UI rendering

3. **History Cell** (`code-rs/tui/src/history_cell.rs`):
   - `new_active_exec_command()` - Creates cell for running command
   - `new_completed_exec_command()` - Updates with final output
   - Handles syntax highlighting via `ParsedCommand`

This architecture separates concerns between execution logic (core), UI state management (chatwidget), and rendering (history_cell).

### Long-Running Goal Handling

- Standalone Auto Drive is retired. Do not restore `/auto`, `code exec --auto`,
  Auto Drive settings/cards, `AUTO_AGENTS.md`, or `code-auto-drive-*` crates.
- Preserve useful long-running-work behavior through current Codex goal,
  review, guardian, session, and agent primitives in `code-rs`.
- Approval panes and global Esc handling should remain modal-first and
  interruption-friendly for current TUI surfaces; add focused TUI fixtures when
  changing those semantics.

### TUI Crash Diagnostics

- `code-dev` in `/home/azureuser/.bashrc` auto-enables local crash capture for dev runs. It sets `CODEX_TUI_RECORD_SESSION=1`, chooses a per-run `CODEX_TUI_SESSION_LOG_PATH` under `~/.code/debug_logs/code-dev/`, and forces `RUST_BACKTRACE=full` unless you already overrode it.
- Existing terminal sessions do **not** pick up the updated `code-dev` alias automatically. In every already-open shell that you want to use for debugging, run `source ~/.bashrc` (or restart the shell with `exec bash`). In tmux, do this once per pane/window before launching `code-dev`.
- On startup, `code-dev` prints the exact session log path. Keep that path after a crash; it contains TUI events, token metrics, and structured panic records with backtraces.
- TUI panics are logged in both places: the per-run session JSONL log and the regular `critical.log` under Code's log directory. Check both when debugging long-running sessions.
- If you want telemetry to leave the machine as well, configure `[otel]` in `~/.code/config.toml`.

## Writing New UI Regression Tests

- Start with `make_chatwidget_manual()` (or `make_chatwidget_manual_with_sender()`) to build a `ChatWidget` in isolation with in-memory channels.
- Simulate user input by defining a small enum (`ScriptStep`) and feeding key events via `chat.handle_key_event()`; see `run_script()` in `tests.rs` for a ready-to-use helper that also pumps `AppEvent`s.
- After the scripted interaction, render with a `ratatui::Terminal`/`TestBackend`, then use `buffer_to_string()` (wraps `strip_ansi_escapes`) to normalize ANSI output before asserting.
- Prefer snapshot assertions (`assert_snapshot!`) or rich string comparisons so UI regressions are obvious. Keep snapshots deterministic by trimming trailing space and driving commit ticks just like the existing tests do.
- When adding fixtures or updating snapshots, gate rewrites behind an opt-in env var (e.g., `UPDATE_IDEAL=1`) so baseline refreshes remain explicit.

## VT100 Snapshot Harness

- The VT100 harness lives under `code-rs/tui/tests/vt100_chatwidget_snapshot.rs`. It renders the live `ChatWidget` UI into a `Terminal<VT100Backend>` so snapshots capture the exact PTY output the user sees (including frame chrome, composer rows, and streaming inserts).
- Use `ChatWidgetHarness` helpers from `code_tui::test_helpers` to seed history events and drain `AppEvent`s. Call `render_chat_widget_to_vt100(width, height)` for a single frame, or `render_chat_widget_frames_to_vt100(&[(w,h), ...])` to simulate successive draws while streaming.
- The harness now exports `layout_metrics()` so tests can assert scroll offsets and viewport heights without spelunking through private fields.
- Snapshots are deterministic: tests set `CODEX_TUI_FAKE_HOUR=12` automatically so greeting text (“What can I code for you today?”) doesn’t oscillate. If you need a different hour in a test, override the env var before constructing the harness.
- To add a new scenario, push history/events onto the harness, call `render_*_to_vt100`, and either `insta::assert_snapshot!` the frame(s) or manually assert string contents. For multi-frame streaming, push deltas/events first, then capture frames in the order the UI would display them.
- Run all VT100 snapshots via:
  - `cargo test -p code-tui --test vt100_chatwidget_snapshot --features test-helpers -- --nocapture`
- When you intentionally change rendering, review the `.snap.new` files that appear in `code-rs/tui/tests/snapshots/` and accept them with `cargo insta review` / `cargo insta accept` (limit to this test where possible).

### Monitor Release Workflows After Pushing

- Use `scripts/wait-for-gh-run.sh` to follow GitHub Actions releases without spamming manual `gh` commands.
- Typical release-intent check right after merging release metadata:
  `scripts/wait-for-gh-run.sh --workflow 'Release Intent' --branch main`.
- A successful `Release Intent` workflow may be a publish or a no-op. If a
  release was requested, verify the tag explicitly with
  `gh release view v<version> --repo cbusillo/code`.
- If you already know the run ID (e.g., from webhook output), run `scripts/wait-for-gh-run.sh --run <run-id>`.
- Adjust the poll cadence via `--interval <seconds>` (defaults to 8). The script exits 0 on success and 1 on failure, so it can gate local automation.
- Pass `--failure-logs` to automatically dump logs for any job that does not finish successfully.
- Dependencies: GitHub CLI (`gh`) and `jq` must be available in `PATH`.
