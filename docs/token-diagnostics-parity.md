# Token Diagnostics Parity

This note supports [#401](https://github.com/cbusillo/code/issues/401) and the
Codex fork parity ledger. Current `main` already carries cached input token
counts from the Codex-base token pipeline through app-server replay and TUI
status updates, but the pre-#390 Every Code diagnostic surface also promised a
visible prompt-cache signal.

## Current Coverage

- `thread/tokenUsage/updated` carries `cachedInputTokens` in total and last-turn
  token usage payloads.
- Provider parsing preserves whether cached-input telemetry was present via
  `cachedInputTokensReported`, so clients can distinguish absent telemetry from
  a provider-reported zero cached-input count.
- App-server resume/fork replay restores token usage from rollout `TokenCount`
  records, including non-zero cached input fixtures.
- `/status` renders a derived prompt-cache hit rate when cached input tokens are
  non-zero.
- The configurable footer status line supports the `prompt-cache-hit-rate` item,
  which is omitted until cached input tokens are non-zero.

## Remaining Gaps

- TUI status rendering still treats `cached_input_tokens = 0` as no visible
  cache signal; it does not yet use `cachedInputTokensReported` to distinguish
  “telemetry absent” from a real 0% hit.
- The old rolling-average cache signal is not restored in this slice.
- The old `/limits` overlay and local account-usage history remain separate
  parity decisions from the prompt-cache signal.

## Follow-Up Fixtures

- Footer/status-line fixture for displaying telemetry-present zero-cache usage
  differently from telemetry-absent usage, if that remains a required behavior.
- Footer/status-line fixture for a rolling prompt-cache average if that signal
  remains a required Every Code behavior.
