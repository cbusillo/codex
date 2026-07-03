# Codex-Base Every Code Feature Inventory

This inventory supports [#386](https://github.com/cbusillo/code/issues/386) and
is now gated by [#397](https://github.com/cbusillo/code/issues/397). It compares
the practical pre-#390 Every Code Rust workspace anchor `fa0c33944f` with the
Codex CLI-substrate `code-rs` workspace after PR #391 (`114ba08163`). The companion
[Codex fork parity ledger](codex-fork-parity-ledger.md) compares the same
pre-#390 anchor against current `origin/main` for deleted fixtures, tests, and
behavior probes.

Operationally, #397 owns the short-term parity gate: missing Every Code behavior
must be classified there before broad feature ports resume under #386. This
inventory remains the broader domain map for deciding what to port, what to
replace with a Codex-native surface, and what can be dropped.

Because this inventory was first written against the post-#391 baseline, verify
each status cell against current `origin/main` before using it to justify a port
or retirement decision.

Current `main` should be treated as a provisional Codex CLI fork baseline until
the parity ledger in [codex-fork-parity-ledger.md](codex-fork-parity-ledger.md)
has classified the removed Every Code fixtures, tests, and behavior probes. Do
not treat a missing old feature as intentionally retired unless that ledger or a
linked issue says so.

`code-rs` is now the editable Every Code product workspace built on the Codex
CLI substrate. `codex-rs` remains the read-only `openai/codex:main` mirror. Do
not re-add dependencies from `code-rs` to sibling `../codex-rs` while porting or
aligning features.

## Decision Summary

| Domain | Pre-substrate Every Code anchors | Codex-base status | Decision | Risk | Next slice |
| --- | --- | --- | --- | --- | --- |
| Auto Drive core and TUI | `code-rs/code-auto-drive-core/`, `code-rs/code-auto-drive-diagnostics/`, `code-rs/core/src/auto_drive_pid.rs`, `code-rs/tui/src/auto_drive_*`, `code-rs/tui/src/chatwidget/auto_drive_cards.rs`, `docs/auto-drive.md` | Standalone Auto Drive is retired on the current Codex-fork baseline. Current `main` does not expose the old coordinator, `/auto`, `code exec --auto`, Auto Drive cards, or settings pane. The #398 retirement boundary lives in [auto-drive-parity.md](auto-drive-parity.md). | Retire standalone Auto Drive. Preserve only goal-mode-compatible concepts on current Codex primitives. | Medium | Do not port Auto Drive crates, commands, cards, or settings. Add goal-mode fixtures only when current goal/review/session surfaces need them. |
| Remote inbox, Discord UI, and external session continuity | `code-rs/tui/src/remote_inbox/client.rs`, `code-rs/tui/src/remote_inbox/protocol.rs`, `code-rs/tui/src/remote_inbox/mod.rs`; historical commits include Discord and Code Everywhere remote inbox work | The old remote inbox modules are absent. Codex-base has `app-server`, remote-control transport, thread list/resume/fork/start APIs, and Desktop-facing session APIs, but no proven replacement for GitHub/LaunchPlane-created sessions. | Preserve capability until #388 proves Desktop or another UI can discover, resume/continue, and answer externally-created sessions. Discord presentation is droppable only after that replacement exists. | High | #388 decision and validation slice. |
| Auto Review, review storage, and proof metrics | `code-rs/core/src/review_coord.rs`, `code-rs/core/src/review_store.rs`, `code-rs/core/src/tasks/review.rs`, `code-rs/core/docs/auto-review.md`, `code-rs/core/tests/review_coord_integration.rs`, `code-rs/tui/src/bottom_pane/review_settings_view.rs` | Codex-base has reviewer mode, `review/start`, guardian auto-approval review events, `code-rs/core/src/session/review.rs`, `code-rs/core/src/review_format.rs`, and TUI auto-review denials. The old Every Code proof ledger and background ghost-commit reviewer are not restored as-is. | Keep the user value, map it onto Codex review/guardian primitives, and port proof metrics only where Codex lacks equivalent evidence. | High | Inventory proof-metric gaps, then add a focused review-ledger port. |
| Token, rate-limit, and prompt-cache diagnostics | Historical `code-rs/core/src/token_data.rs`, app-server `ThreadTokenUsageUpdatedNotification` schemas, prompt-cache release notes and TUI/footer docs | Codex-base keeps token usage and rate-limit surfaces through `login/src/token_data.rs`, `app-server/src/request_processors/token_usage_replay.rs`, `code-rs/tui/src/token_usage.rs`, and app-server thread token events. Prompt-cache hit-rate reporting from Every Code docs may not be fully present in current TUI. | Keep token/rate-limit diagnostics. Defer prompt-cache hit-rate UI until the new token pipeline is audited. | Medium | Audit current token events against the release-note promises before code changes. |
| Code Bridge and browser control | `code-rs/browser/`, `code-rs/cli/src/bridge.rs`, `code-rs/core/src/bridge_client.rs`, browser TUI history cells and snapshots | The old dedicated browser crate is absent. Codex-base has hooks browser UI, remote-control transport, and app-server/browser-relevant realtime surfaces, but not the Every Code Code Bridge control channel as a product feature. The #399 boundary map lives in [code-bridge-browser-parity.md](code-bridge-browser-parity.md). | Must preserve Code Bridge capability or explicitly replace it. Port as an additive Every Code-owned integration, not as a mutation to Desktop compatibility responses. | Medium-high | Add fake bridge metadata/control fixtures, then port the local control channel and screenshot/console/error events. |
| App-server and Desktop compatibility | `code-rs/app-server/`, `code-rs/app-server-protocol/`, old v1 conversation schemas | Codex-base app-server is the substrate target and now exposes v2 thread lifecycle, `review/start`, `thread/resume`, `thread/fork`, remote-control transport, auth, plugin, MCP, and filesystem surfaces. PR #391 intentionally kept Codex Desktop and Codex Cloud naming where it refers to upstream products. | Keep upstream-shaped Desktop compatibility. Add Every Code extensions only through additive protocol surfaces with validation. | High | #387 Desktop validation, then additive extension ports. |
| GitHub/LaunchPlane session launch path | Remote inbox docs/policy, GitHub settings UI, cloud task/session continuity paths | Codex-base has cloud tasks, thread store, app-server thread list/resume/fork APIs, and GitHub helper samples, but the GitHub/LaunchPlane-created-session flow is not yet proven end to end. | Preserve until replaced. Do not remove remote-inbox backend assumptions solely because Codex Desktop exists. | High | Validate through #388 and #387 together. |
| Exec, sandbox, and patch harness | `code-rs/exec/`, `code-rs/execpolicy/`, `code-rs/apply-patch/`, historical `code-rs/git-apply/` and `code-rs/git-tooling/`, `code-rs/linux-sandbox/`, `code-rs/core/src/patch_harness.rs` | Codex-base keeps and expands the execution substrate through `exec`, `exec-server`, `execpolicy`, `execpolicy-legacy`, `apply-patch`, `sandboxing`, `bwrap`, `windows-sandbox-rs`, `shell-command`, `git-utils`, and `utils/pty`. Old Every Code patch harness behavior must be compared rather than blindly restored. | Keep, using Codex-native crates where equivalent. Port only Every Code-specific validation/discovery behavior. | Medium-high | Diff old `patch_harness.rs` behavior against current apply/exec surfaces. |
| Model providers and agent selectors | `code-rs/ollama/`, `code-rs/core/src/model_provider_info.rs`, `code-rs/common/src/model_presets.rs`, agent docs/config | Codex-base has richer provider crates: `model-provider`, `model-provider-info`, `models-manager`, `ollama`, `lmstudio`, `chatgpt`, `codex-client`, and `codex-api`. Every Code built-in agent selector docs remain in `docs/agents.md` and `docs/config.md`. | Keep Every Code selector UX, but map it to Codex-base provider metadata instead of restoring old provider tables wholesale. | Medium | Validate built-in selectors and config docs against current provider registry. |
| Cloud tasks | `code-rs/cloud-tasks/`, `code-rs/cloud-tasks-client/`, `code-rs/code-backend-openapi-models/` | Codex-base includes `cloud-tasks`, `cloud-tasks-client`, `cloud-tasks-mock-client`, `cloud-requirements`, and `codex-backend-openapi-models`. | Keep. Audit Every Code-specific task UX and env detection after provider/auth work settles. | Medium | Focused cloud-task smoke after app-server/Desktop validation. |
| Config migration and product identity | `code-rs/core/src/config/`, `code-rs/common/src/config_override.rs`, `code-rs/cli/`, root docs | PR #391 restored the first CLI identity/config layer: `code` command help, `~/.code` primary config copy, and package scripts. Codex-base still contains upstream docs URLs and Codex copy in deeper TUI/config/runtime surfaces. | Keep `CODE_HOME > CODEX_HOME > ~/.code`. Continue targeted identity passes, not mass renames. | Medium | Later identity sweep for TUI/config/runtime and release metadata. |
| Observability, analytics, and logging | `code-rs/otel/`, `code-rs/core/src/telemetry/mod.rs`, TUI/session logs | Codex-base includes `otel`, `analytics`, session telemetry events, app-server analytics utilities, and login auth-env telemetry. Every Code-specific Code Bridge telemetry remains a separate port concern. | Keep. Preserve external dashboard compatibility until a scoped migration says otherwise. | Medium | Audit telemetry names only when touching each subsystem. |
| Docs and release promises | `README.md`, `docs/auto-drive.md`, `docs/settings.md`, release notes | Several docs still describe features removed by the substrate reset, especially Auto Review, browser/Code Bridge, and prompt-cache UI. Auto Drive is explicitly retired as a standalone feature. | Keep docs aligned with restored features or explicit rescope decisions. | Medium | Remove stale promises as domains are classified. |

## Port Order

1. **Compatibility and session substrate:** finish app-server/Desktop validation
   and remote session continuity decisions (#387 and #388). This prevents Auto
   Drive and remote inbox work from building on the wrong session API.
2. **Low-conflict diagnostics:** token/rate-limit replay, prompt-cache signals,
   and review proof metrics that do not require TUI event-loop rewrites.
3. **Bridge and browser control:** restore Code Bridge as an Every Code-owned
   additive integration with app-server and local control boundaries documented.
4. **Auto Review workflow:** map ghost-commit/background review behavior onto
   Codex-base `review/start` and guardian review primitives.
5. **Goal-mode automation:** preserve any useful Auto Drive concepts only as
   fixtures or small overlays on current goal/review/session surfaces.
6. **Docs/release cleanup:** update README, docs, and release notes once each
   user-visible promise is either restored or deliberately rescoped.

## Guardrails

- Do not remove remote-inbox backend capability until #388 proves a replacement
  for GitHub/LaunchPlane-created sessions. The replacement must have validation
  evidence for discovery, resume/continue, and approval/reply handling for an
  externally-created session.
- Do not drop Discord-specific presentation until the replacement can discover,
  resume/continue, and handle approval/reply flow for externally-created
  sessions.
- Keep Desktop-facing app-server behavior upstream-shaped; Every Code features
  should be additive unless a compatibility test proves otherwise.
- Keep internal `codex-*` substrate names when they remain upstream-shaped or
  compatibility-critical. New Every Code-owned crates should use `code-*` unless
  an external contract requires upstream spelling.
- Every code-bearing port slice must end with `./build-fast.sh` passing cleanly.
