# Codex Fork Parity Ledger

This ledger supports [#397](https://github.com/cbusillo/code/issues/397). Current
`main` is a provisional Codex CLI fork baseline: the Codex-base foundation has
landed, but Every Code feature parity is not complete until removed fixtures,
tests, and behavior probes are classified and either restored, rewritten,
replaced, deferred, or retired with rationale.

This ledger is the fixture/test/probe view. The broader feature-domain map lives
in [codex-base-feature-inventory.md](codex-base-feature-inventory.md), which also
covers domains with less obvious deleted fixture evidence such as cloud tasks,
observability, model providers, and GitHub/LaunchPlane launch paths.

Use `fa0c33944f` as the practical pre-#390 Every Code comparison anchor. It is a
tree anchor, not a promise that every behavior in that tree remains desired.
Forward work should start from current `origin/main`; the `spike/codex-base-port`
branch is historical evidence, not an implementation base.

## Classification Keys

| Key | Meaning |
| --- | --- |
| Port | Restore the behavior with a fixture or test, adapting implementation to the Codex-base architecture. |
| Rewrite | Preserve the regression value, but rewrite the fixture/test against Codex-base primitives. |
| Covered | Current Codex-base tests or behavior already cover the user value; link that evidence before closing. |
| Defer | Keep tracked, but do not block the first parity slices. |
| Retire | Drop only with an explicit issue comment or PR rationale. |

Rows use a single primary classification. When the current tree may already
cover part of a domain, that uncertainty belongs in the current-status or next
action cell until a follow-up issue links concrete evidence.

## Mechanical Diff Summary

Command used for the first pass:

```sh
git diff --name-status --find-renames fa0c33944f..origin/main -- \
  code-rs/tui/tests \
  code-rs/tui/src \
  code-rs/core/tests \
  code-rs/core/src \
  code-rs/cli/src \
  code-rs/app-server-protocol \
  code-rs/browser \
  code-rs/code-auto-drive-core \
  code-rs/code-auto-drive-diagnostics \
  code-rs/core/docs \
  docs
```

That scoped comparison showed 560 deleted paths, 354 modified paths, 1,393
added paths, and a small number of detected renames. The ledger below groups
the deleted paths by user-facing capability so follow-up work can preserve
concepts without blindly restoring old code layout.

A deleted path is not automatically a lost behavior. Before porting any code,
check the current tree for a Codex-native successor, a renamed suite test, or a
newer feature-specific issue. Prefer preserving the regression value over
preserving the old module boundary.

## Parity Domains

| Domain | Deleted evidence from `fa0c33944f` | Current status | Classification | Next action |
| --- | --- | --- | --- | --- |
| Auto Drive coordinator, state, and TUI | `code-rs/code-auto-drive-core/`, `code-rs/code-auto-drive-diagnostics/`, `code-rs/core/src/auto_drive_pid.rs`, `code-rs/tui/src/auto_drive_strings.rs`, `code-rs/tui/src/auto_drive_style.rs`, `code-rs/tui/src/bottom_pane/auto_coordinator_view.rs`, `code-rs/tui/src/bottom_pane/auto_drive_settings_view.rs`, `code-rs/tui/src/chatwidget/auto_drive_cards.rs`, `code-rs/tui/tests/auto_drive_long_session_perf.rs`, and `vt100_chatwidget_snapshot__auto_drive_*.snap` | Current `main` has no live `/auto`, `code exec --auto`, old coordinator crates, Auto Drive card, or settings pane. Codex-base does have thread goals, plan/review commands, guardian review events, token accounting, generic TUI rendering, agents, and resume/rollout primitives. See [auto-drive-parity.md](auto-drive-parity.md). | Retire | #398 owns the retirement boundary. Do not port the standalone coordinator, commands, crates, cards, or settings. Preserve only goal-mode-compatible concepts on current goal/review/session/agent primitives. |
| Code Bridge and browser control | `code-rs/browser/`, `code-rs/browser/tests/local_navigation.rs`, `code-rs/core/src/bridge_client.rs`, `code-rs/tui/src/chatwidget/browser_sessions.rs`, `code-rs/tui/src/history_cell/browser.rs`, `vt100_chatwidget_snapshot__browser_session_*.snap`, `vt100_chatwidget_snapshot__context_cell_browser_badge.snap` | The dedicated browser crate and Code Bridge client are absent. Current app-server/Desktop compatibility has remote-control shims, but not the Every Code local bridge/control channel. See [code-bridge-browser-parity.md](code-bridge-browser-parity.md) for the #399 boundary map. | Port | Preserve console/error/pageview/screenshot/control semantics as an additive Every Code overlay. Add fake bridge metadata/control fixtures before implementation ports. |
| Auto Review proof ledger and review workflow | `code-rs/core/src/review_coord.rs`, `code-rs/core/src/review_store.rs`, `code-rs/core/docs/auto-review.md`, `code-rs/core/tests/review_coord_integration.rs`, `code-rs/tui/src/bottom_pane/review_settings_view.rs`, `vt100_chatwidget_snapshot__auto_drive_review_*.snap` | Codex-base has `review/start`, reviewer mode, guardian review events, and review formatting, but the old proof store/background reviewer evidence is not restored as-is. | Rewrite | #400 owns the review/proof parity audit. Compare proof metrics and background review guarantees against Codex guardian/review primitives before implementation. |
| Remote inbox and external session continuity | `code-rs/tui/src/remote_inbox/client.rs`, `code-rs/tui/src/remote_inbox/protocol.rs`, `code-rs/tui/src/remote_inbox/mod.rs`, plus app-server v1 conversation schemas such as `ResumeConversationParams`, `ListConversationsResponse`, `SessionConfiguredNotification`, and old TypeScript conversation/event types | Current app-server has v2 thread list/resume/fork/start and Desktop startup compatibility. As of this first ledger pass, #388 still gates whether Desktop or another UI replaces GitHub/LaunchPlane-created session discovery, resume/continue, and approval/reply flow. | Port | Keep #388 blocking retirement. Build an external-session fixture before removing backend or Discord-specific presentation. |
| App-server v1 compatibility and generated schemas | Deleted `code-rs/app-server-protocol/schema/json/v1/*`, root `EventMsg.json`, root TypeScript request/event models, `code-rs/app-server-protocol/src/protocol/v2.rs`, and removed remote skill schema files | Current protocol is Codex-shaped and Desktop-oriented, with PR #393 shimming narrow Desktop startup gaps. Some v1 conversation concepts may map to v2 threads; others may be obsolete. | Rewrite | Do not restore v1 wholesale. For each external client expectation, add a compatibility fixture or document the Codex v2 equivalent. |
| TUI settings, overlays, and approval UX | `code-rs/tui/src/bottom_pane/settings_overlay.rs`, many settings section views, `approval_modal_view.rs`, `approval_ui.rs`, `request_user_input_view.rs`, `model_selection_view.rs`, `undo_timeline_view.rs`, `validation_settings_view.rs`, `verbosity_selection_view.rs`, `user_approval_widget.rs`, `settings_overlay_agents.rs`, and settings snapshots | Codex-base has its own TUI app, settings, approval, and status surfaces. Every Code-specific settings sections for agents, GitHub, review, validation, notifications, and planning need explicit decisions. Auto Drive settings are retired by #398. | Rewrite | Split by feature domain. Preserve user workflows such as agent settings, approval clarity, and validation controls where still required. |
| TUI history cells and transcript rendering | Deleted `code-rs/tui/src/history_cell/*`, `code-rs/tui/src/history/compat.rs`, `code-rs/tui/src/chatwidget/*` modules for agents, terminal, tools, web search, rate limits, streaming, history render, plus `vt100_chatwidget_snapshot.rs` and many snapshots | Current TUI has Codex-native history/rendering modules and snapshots. Some Every Code-specific cells represented Auto Drive, browser sessions, agents, rate limits, context, and tool grouping. | Rewrite | Preserve snapshot coverage for required overlays. Avoid restoring old visual architecture unless it carries behavior current TUI cannot express. |
| Token, rate-limit, and prompt-cache diagnostics | `code-rs/core/src/token_data.rs`, `code-rs/core/src/account_usage.rs`, `code-rs/tui/src/chatwidget/limits_overlay.rs`, `code-rs/tui/src/rate_limits_view.rs`, `code-rs/tui/src/history_cell/rate_limits.rs`, `codex_tui__chatwidget__tests__limits_*.snap` | Codex-base has token usage replay, app-server token events, login token data, and TUI token usage. The first #401 slice restores prompt-cache hit-rate display from existing cached-token telemetry; schema-level telemetry-present semantics and old limits UI parity remain open. See [token-diagnostics-parity.md](token-diagnostics-parity.md). | Rewrite | #401 owns the token/prompt-cache audit. Add telemetry-present and old limits/rate diagnostics fixtures before deeper UI work. |
| Agents, multi-agent workflows, and selectors | `code-rs/core/src/agent_defaults.rs`, `code-rs/core/src/agent_tool.rs`, `code-rs/core/tests/agent_completion_wake.rs`, `code-rs/core/tests/antigravity_agent_spec.rs`, `code-rs/tui/src/chatwidget/agent*.rs`, `code-rs/tui/src/history_cell/agent.rs`, `vt100_chatwidget_snapshot__agent_*.snap`, `settings_overlay_agents.rs` | Docs still describe built-in multi-agent selectors. Current `main` may contain newer agent infrastructure, but old TUI/status/test parity needs confirmation. | Port | Create focused fixtures for selector resolution, agent run grouping, agent status errors, and settings UI before changing agent orchestration. |
| Config, identity, skills, and prompts | Deleted old `code-rs/core/src/config*`, `config_loader/*`, `custom_prompts.rs`, `slash_commands.rs`, `skills/*`, `external_agent_config.rs`, and tests such as `custom_prompts_discovery.rs`, `skill_command_policy.rs`, `prompt_context_dedup.rs`, `external_agent_config` snapshots | PR #391 and #394 restored important config compatibility, including `CODE_HOME` precedence and legacy `[tui] alternate_screen`. Current Codex-base has its own config, prompt, and skills systems. | Rewrite | Keep compatibility tests for `CODE_HOME`, legacy config shapes, prompts, skills, and external agent import. Mark individual probes covered only after linking current tests. |
| Exec, sandbox, patch, and validation harness | Deleted old `patch_harness.rs`, `workflow_validation.rs`, `dry_run_guard.rs`, `command_safety/*`, `git_worktree.rs`, `exec_command/*`, `tool_hooks.rs`, `git_mutation_guard.rs`, `stuck_exec.rs`, `exec_completion_test.rs`, `wayland_clipboard_feature_regression.rs`, `windows_altgr.rs` | Codex-base has richer exec/sandbox/apply-patch crates. Every Code-specific validator discovery and git safety behavior must be compared rather than restored wholesale. | Rewrite | Add fixtures for changed-file validation, git mutation safety, shell completion, and platform shortcut regressions only where Codex-base lacks equivalent tests. |
| Dogfood exec harness gate | `tools/code-exec-harness/` was preserved across the pivot, but pre-pivot dogfood exec coverage has not yet been recast as the first fork-baseline gate | #404 defines Dogfood Parity 1. #405 owns the deterministic no-live-token `code exec --json` smoke gate using the dev-fast `code` binary from this checkout. | Rewrite | Make `just harness-smoke` pass after `./build-fast.sh`, then extend the suite with old high-signal exec, resume, sandbox, and validation probes before claiming daily-driver parity. |
| Session catalog, resume, rollout, memory, and history | Deleted `active_sessions.rs`, `session_catalog.rs`, `rollout/*`, `conversation_history.rs`, `message_history.rs`, `context_ledger.rs`, `context_timeline/*`, `memories/*`, `retention.rs`, tests including `active_session_warnings.rs`, `session_catalog_resume.rs`, `image_history_replay.rs`, `resume_catalog_integration.rs`, `resume_replay.rs`, `history_cutoff_probe.rs` | Codex-base has thread store/history/resume/rollback primitives and recent resume CLI tests. External-session continuity remains unproven. | Rewrite | Prioritize external-session and image/history replay fixtures. Treat local resume picker parity as covered only after matching current tests are identified. |
| UI polish and layout regression snapshots | `bottom_spacer_clip_regression.rs`, `ui_smoke.rs`, `mid_turn_assistant_styling.rs`, `mid_turn_queueing.rs`, `mcp_session_cleanup.rs`, `non_windows_shortcuts.rs`, bottom spacer snapshots, settings/help snapshots, multiline/tool activity snapshots | Current Codex TUI has its own snapshot suite. Old snapshots may assert implementation-specific styling, but many captured useful regressions around wrapping, spacing, queueing, and keyboard behavior. | Defer | Recreate high-value regressions against current TUI only when they guard active overlay behavior or known user-facing polish regressions. |
| Build, release, dogfood, and cleanup workflows | Pre-#390 workflow/docs/scripts should be compared separately from code/test parity, including release guardrails and local cleanup behavior | #396 owns build-cache strategy follow-up. Release/dogfood docs now need a provisional-baseline warning until parity status is explicit. | Defer | Keep #396 for cache behavior. Add release guardrails before release-bound work; do not treat this first ledger as product parity. |
| Docs and release promises | Docs still include `docs/auto-drive.md`, `docs/settings.md`, `docs/agents.md`, release notes, and feature descriptions whose implementation parity is incomplete | Documentation can preserve intent, but it must not imply that provisional `main` has restored all features. | Rewrite | Update docs only alongside restored features or explicit rescope decisions. Release-bound work must call out missing parity. |

## High-Signal Potential Gaps

These deleted probes did not have obvious same-name successors in the first
mechanical pass and should be checked early:

| Probe | Classification | Owner | Why it matters |
| --- | --- | --- | --- |
| `code-rs/core/tests/git_mutation_guard.rs` | Rewrite | Exec/patch parity follow-up | Guarded repository mutation policy and destructive-git safety. |
| `code-rs/tui/tests/wayland_clipboard_feature_regression.rs` | Rewrite | UI/platform parity follow-up | Protected a platform-specific clipboard/build dependency regression. |
| `code-rs/browser/tests/local_navigation.rs` | Port | #399 | Exercised local browser navigation behavior from the removed browser crate. |
| `code-rs/tui/tests/auto_drive_long_session_perf.rs` and `vt100_chatwidget_snapshot__auto_drive_*.snap` | Retire | #398 | Do not recreate Auto Drive-specific snapshots. Preserve long-session rendering coverage only on current goal-mode or generic TUI surfaces if a concrete regression appears. |
| `code-rs/tui/tests/mcp_session_cleanup.rs` | Rewrite | Exec/patch or MCP parity follow-up | Guarded MCP process/session lifecycle cleanup across TUI sessions. |
| `code-rs/tui/tests/stuck_exec.rs` | Rewrite | Exec/patch parity follow-up | Guarded stale exec/background completion edge cases. |
| `code-rs/tui/tests/resume_replay.rs` and `resume_catalog_integration.rs` | Rewrite | #388 or session/resume parity follow-up | Guarded replay/dedupe/catalog behavior that may or may not be covered by current thread resume tests. |

## First Follow-Up Issues

Created or existing focused issues before implementation work in these areas:

1. #398: Standalone Auto Drive retirement and goal-mode fixture boundary.
2. #399: Code Bridge/browser control parity fixtures.
3. #400: Auto Review proof metrics and background review evidence.
4. #388: External-session continuity fixture for GitHub/LaunchPlane/remote-origin sessions.
5. #401: Token/rate-limit/prompt-cache diagnostics parity.
6. #404: Dogfood Parity 1 daily-driver baseline.
7. #405: Make `tools/code-exec-harness` the deterministic P0 exec gate.
8. Needed: Agent selector and multi-agent status parity.
9. Needed: Exec/patch/validation harness parity beyond the first smoke gate.
10. Needed: High-signal UI/platform probes not owned by #398/#399/#401.

## Update Protocol

- Each follow-up issue should update this ledger when it classifies a probe as
  covered, deferred, or retired.
- A domain is considered resolved only when every named high-signal probe has an
  issue, a linked current-test equivalent, or a written retire/defer rationale.
- #397 can close only after all Port/Rewrite domains are either represented by
  focused child issues or moved to Covered/Defer/Retire with evidence.

## Guardrails

- Do not continue feature work from `spike/codex-base-port`; use current
  `origin/main` plus focused parity branches.
- Do not delete the spike until the parity-critical fixtures/concepts above are
  captured in issues or PRs.
- Do not remove remote-inbox backend capability or Discord-specific presentation
  before #388 has evidence for externally-created session discovery,
  resume/continue, and approval/reply handling.
- Do not release or dogfood a build as full Every Code parity unless the missing
  feature status is explicit and acceptable.
- Prefer failing or ignored parity fixtures over speculative implementation
  ports when the desired behavior is known but the Codex-base architecture is
  not yet settled.
