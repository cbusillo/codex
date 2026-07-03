# Standalone Auto Drive Retirement

This note supports [#398](https://github.com/cbusillo/code/issues/398) and the
Codex fork parity ledger. It supersedes the earlier Auto Drive parity plan: do
not port the old standalone Auto Drive coordinator, command surface, settings
pane, cards, or crates to the current Codex-fork baseline.

Codex now has goal-mode-shaped primitives for long-running work: thread goals,
plan/review commands, guardian review events, token accounting, generic dynamic
tool rendering, agent delegation, and session resume/rollout infrastructure.
Every Code should align with those primitives instead of making Codex mirror the
old Every Code Auto Drive implementation. Useful old Auto Drive behavior can be
preserved only as goal-mode-compatible fixtures or small additive overlays.

## Classification

| Old evidence | User value | Classification | Goal-mode boundary |
| --- | --- | --- | --- |
| `code-rs/code-auto-drive-core/src/controller.rs` | Continue modes, countdown/manual approval, run phases, stop/pause semantics. | Retire standalone port | If goal mode needs pause/continue controls, add them to current task/session events with fixtures named for goal mode, not Auto Drive. |
| `code-rs/code-auto-drive-core/src/auto_coordinator.rs` | Maintainer-style loop that plans, delegates, runs checks, compacts, and resumes. | Retire standalone port | Use current Codex thread/session/goal primitives as the source of truth. Do not reintroduce a separate coordinator crate. |
| `code-rs/code-auto-drive-core/src/coordinator_router.rs` | Route user status/plan/stop messages while automation is active. | Retire standalone port | Keep command routing on current slash-command, queued-command, and goal surfaces. |
| `code-rs/code-auto-drive-core/src/retry.rs` | Bounded retry, backoff, rate-limit waits, and terminal failure status. | Port concept only | Add focused retry/backoff tests only when goal mode exposes a concrete retry policy. |
| `code-rs/code-auto-drive-diagnostics/` | Structured completion/QA check before declaring success. | Port concept only | Prefer current review/guardian/check-result primitives; add completion evidence there if needed. |
| `code-rs/core/src/auto_drive_pid.rs` | Prevent duplicate Auto Drive ownership. | Port concept only | Reuse current session/task ownership and worktree/branch identity guards. Do not add an Auto Drive PID file. |
| `code-rs/tui/src/bottom_pane/auto_drive_settings_view.rs` | User-facing defaults for agents, review, diagnostics, continue mode, model routing. | Retire standalone port | Keep settings on existing goal, agent, review, model, and validation surfaces. Do not add an Auto Drive settings pane. |
| `code-rs/tui/src/chatwidget/auto_drive_cards.rs`, `code-rs/tui/src/history_cell/auto_drive.rs`, `vt100_chatwidget_snapshot__auto_drive_*.snap` | Visible run state, action log, countdown, review footer, success/failure status, long-session rendering performance. | Retire standalone port | Render through current generic TUI/history surfaces. Add goal-mode snapshots only for real current surfaces. |
| `code-rs/tui/tests/auto_drive_long_session_perf.rs` | Countdown ticks must not force full-history rerendering. | Port concept only | Recreate as a goal-mode or generic TUI rendering performance test if countdown/status updates return. |
| `docs/auto-drive.md`, `docs/settings.md`, `docs/agents.md`, `docs/config.md` | Product promise and operator model. | Retire stale commands | Document Auto Drive as retired and keep active commands/settings aligned with current Codex-derived surfaces. |

## Goal-Mode Fixtures To Preserve

1. Goal handling: explicit goals become current thread goals; empty-goal flows
   fail cheaply or derive from current history only if goal mode supports that.
2. Review gate: a finish candidate can route through current review/guardian
   evidence before continuing or finishing.
3. Duplicate ownership: a second long-running goal for the same session/worktree
   is rejected, resumes the existing task, or otherwise resolves deterministically.
4. Long-session rendering: status updates do not rebuild the full transcript.
5. Agent delegation: goal-mode agents use the normal agent configuration and
   read-only safeguards instead of Auto Drive-specific toggles.

## Retired

- Do not restore `code-auto-drive-core` or `code-auto-drive-diagnostics`.
- Do not restore `/auto`, `/auto settings`, `code exec --auto`, or
  `code exec "/auto ..."` as compatibility shims.
- Do not restore Auto Drive-specific TUI cards, history cells, settings panes,
  observer banners, model-routing tables, or `AUTO_AGENTS.md` loading.
- Do not restore the old Auto Review proof store as part of this topic; #400
  maps review proof metrics onto current Codex review/guardian primitives.

## Open Questions

- Which goal-mode controls, if any, need explicit pause/continue affordances
  beyond the current Codex-derived interaction model.
- Which old completion/QA guarantees are still missing after #400's
  review/guardian proof-metric audit.
