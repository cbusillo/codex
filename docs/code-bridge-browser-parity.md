# Code Bridge And Browser Parity Boundary

This map supports [#399](https://github.com/cbusillo/code/issues/399). It
classifies the pre-#390 Every Code Code Bridge/browser evidence before any
implementation port resumes. Use `fa0c33944f` as the old evidence anchor and
current `origin/main` as the Codex CLI fork baseline.

## Boundary Decision

Code Bridge is an Every Code overlay, not a Desktop compatibility mutation.
Future bridge behavior should be additive and validated through bridge-specific
fixtures before it touches app-server or TUI presentation code.

When Codex CLI already has a similar app-server, protocol, or command surface,
prefer aligning Every Code behavior with that Codex shape instead of making the
Codex-facing surface mirror the old Every Code bridge commands. Preserve Every
Code features, fixes, and fixtures through the smallest additive overlay needed
for behavior that Codex does not already cover.

Code Bridge `control` events must use a distinct transport or namespace from
Codex Desktop remote-control requests. Fake bridge fixtures must prove that
bridge control delivery does not route through Desktop remote-control status,
probe, or request/response paths before implementation code lands.

Keep these current Codex-base surfaces upstream-shaped unless a focused fixture
proves an additive extension is required:

- App-server thread, turn, and Desktop startup responses.
- App-server remote-control compatibility status/probe responses.
- Desktop-facing protocol schemas and generated TypeScript shapes.
- Existing hooks browser UI and hook event semantics.

## Old Evidence Classification

| Old evidence | User value | Classification | First fixture before porting |
| --- | --- | --- | --- |
| `code-rs/browser/tests/local_navigation.rs` | Internal browser can open a local HTTP page, report current URL, execute JavaScript, and read page text in headless and headed modes. | Port | A deterministic local-navigation fixture for the new bridge/browser overlay crate or harness. |
| `code-rs/browser/src/manager.rs`, `page.rs`, and `tools/browser_tools.rs` | Browser session lifecycle, navigation, JavaScript execution, screenshots, and tool-call facing browser actions. | Port | A no-network fixture that drives a local page through navigation, JavaScript, screenshot, and cleanup. |
| `code-rs/core/src/bridge_client.rs` | Discover `.code/code-bridge.json`, subscribe to console/error/pageview/screenshot/control events, batch summaries, and send control requests. | Port | Metadata discovery and subscription normalization tests before reconnect/batching implementation. |
| `code-rs/cli/src/bridge.rs` | CLI-facing bridge discovery, event tailing, screenshot request, JavaScript request, and stale heartbeat handling. | Retire for now | Do not restore the old `code bridge` subcommands while the model-facing `code_bridge` tool covers subscribe, collect, screenshot, JavaScript, and navigate through the current Codex-style tool path. Reconsider only if a concrete human CLI workflow remains after the browser host boundary is implemented. |
| `code-rs/tui/src/chatwidget/browser_sessions.rs` and `code-rs/tui/src/history_cell/browser.rs` | TUI grouping and transcript rendering for browser/page events. | Supersede | Keep using generic dynamic tool-call history rendering for Code Bridge. Avoid restoring browser-specific TUI cells unless a focused fixture proves generic rendering cannot represent a required workflow. |
| `vt100_chatwidget_snapshot__browser_session_*.snap` and `vt100_chatwidget_snapshot__context_cell_browser_badge.snap` | Browser-session grouping, unordered action stability, foreign-event filtering, and context badge visibility. | Supersede | Generic dynamic tool-call snapshots now cover Code Bridge collect/screenshot history. Add future snapshots against that generic surface rather than reviving old browser-session snapshot fixtures. |
| `code-rs/core/templates/compact/history_bridge.md` | Preserve handoff context during compaction. Despite the filename, this was a generic history handoff template, not a Code Bridge-specific artifact. | Retire | Keep the Codex-aligned programmatic compaction path. Preserve collected bridge evidence through the active compaction prompt and transcript summary instead of restoring the old template. |

## Proposed Additive Event Contract

The restored overlay should preserve these event classes without reusing Desktop
remote-control responses as the transport contract:

| Event class | Minimum payload to validate first |
| --- | --- |
| `console` | level, message, url or page id, timestamp. |
| `error` | message, stack or location when available, url or page id, timestamp. |
| `pageview` | url, title when available, page id, timestamp. |
| `screenshot` | mime type, byte length or data reference, page id, timestamp. |
| `control` | request id, action, delivery/result status, optional response payload. |

Control actions start with `ping`, `navigate`, `screenshot`, and `javascript`
because the old CLI and browser tool paths depended on those. `navigate`
travels over the bridge `control_request` channel with a `url` field and must
return a `control_result`; any resulting `pageview` event is separate passive
event evidence rather than the control acknowledgement itself.

## Validation Order

1. Add fake bridge metadata/server fixtures that prove discovery, stale heartbeat
   detection, subscription normalization, and control request/result matching,
   while first proving Code Bridge-shaped control does not enter the Codex
   Desktop remote-control JSON-RPC path. The app-server path separation fixture
   landed in #417; core fake bridge fixtures landed in #418.
2. Port the local navigation probe to the new overlay boundary using a local HTTP
   server and no external network. Local bridge event fixtures for `pageview`,
   `console`, and `screenshot` payloads landed in #419. Bridge `navigate`
   control request fixtures landed after that event schema stabilized, still
   without reintroducing the old browser crate.
3. Add TUI snapshot coverage only after event schema and storage are stable.
   Generic dynamic tool-call rendering and Code Bridge collect/screenshot
   history snapshots landed in #422.
4. Preserve bridge context through Codex-aligned compaction rather than
   restoring the old generic `history_bridge.md` template. The active compact
   prompt should ask the summarizing model to carry forward collected Code
   Bridge console, error, pageview, screenshot, and control evidence when that
   evidence is already present in the transcript. A later compaction fixture
   should seed bridge tool results and assert the resulting summary carries the
   important bridge findings forward.
5. Keep human CLI commands retired unless a later browser-host slice identifies
   a non-model workflow that cannot be handled through the `code_bridge` tool or
   existing debug surfaces.
6. Wire implementation behind the additive Every Code bridge surface.

Do not port the old browser crate, TUI cells, snapshots, or CLI bridge commands
wholesale before the relevant fixture in this document exists.
