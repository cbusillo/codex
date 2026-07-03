# Auto Drive (Retired)

Standalone Auto Drive is retired on the current Codex-fork baseline. Every Code
should not port the old Auto Drive coordinator, TUI cards, settings pane,
observer, config keys, or command surface.

Use current goal-mode, plan, review, guardian, agent, session, and resume
primitives for long-running work. When an old Auto Drive behavior still matters,
preserve it as a goal-mode fixture or a small additive overlay on those current
surfaces.

## Retired Surfaces

- `/auto`
- `/auto settings`
- `code exec --auto`
- `code exec "/auto ..."`
- `AUTO_AGENTS.md`
- `auto_drive_*` config keys and `[auto_drive]` settings
- `code-auto-drive-core` and `code-auto-drive-diagnostics`
- Auto Drive-specific TUI cards, history cells, settings panes, observer
  banners, countdown UI, and VT100 snapshots

## Preserved Concepts

- Explicit goals and deterministic empty-goal behavior belong to goal mode.
- Review-before-finish behavior belongs to current review/guardian primitives.
- Duplicate long-running task ownership should use current session, task,
  worktree, and branch identity guards.
- Long-session status rendering should be tested on current TUI/history surfaces.
- Agent delegation should use normal Every Code agent settings and safeguards.

See [auto-drive-parity.md](auto-drive-parity.md) for the retirement boundary
and the goal-mode fixture map.
