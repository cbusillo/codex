# Auto Review Proof Metrics Parity

This map supports [#400](https://github.com/cbusillo/code/issues/400) and keeps
Auto Review porting validation-first. Use `fa0c33944f` as the pre-#390 evidence
anchor and current `origin/main` as the Codex-base substrate. The goal is to
preserve proof value without restoring the old review store or background
reviewer wholesale.

## Current Covered Evidence

| Proof value | Current evidence | Classification | Next action |
| --- | --- | --- | --- |
| Explicit review requests run through a review-specific path. | `review/start`, `code-rs/core/src/session/review.rs`, `code-rs/core/src/tasks/review.rs`, and `code-rs/core/src/review_format.rs`. | Covered | Keep using Codex review primitives for `/review` and `code exec review`. |
| Review turns can use the configured `review_model`. | `session/review.rs` and `tasks/review.rs` select `config.review_model` before falling back to the parent turn model. | Covered | Add deeper fixtures only when changing review model selection. |
| Auto-review approval mode is visible to the model. | `code-rs/core/src/context/permissions_instructions.rs` appends the `approvals_reviewer = auto_review` guidance for on-request approvals. `tools/code-exec-harness/scenarios/auto-review-config-routing.json` now proves this path end to end without live tokens. | Covered | Keep the harness scenario in `tools/code-exec-harness/run-deterministic.sh`. |
| Auto-review decisions are represented in app-server/TUI events. | Guardian assessment and auto-review denial payloads exist in `code-rs/app-server-protocol` and the TUI recent-denials flow. | Covered | Extend only if #400 later needs denial retry ledger evidence. |

## Rewrite Gaps

| Old evidence | User value | Classification | Required fixture before porting |
| --- | --- | --- | --- |
| `code-rs/core/src/review_store.rs` run ledger | Durable status, freshness, elapsed time, token/prompt estimate, error summary, finding count, and compact byte-capped ledger rows. | Rewrite | A small fixture around the current review/guardian event stream that records terminal proof without depending on the deleted store shape. |
| `code-rs/core/src/review_coord.rs` and `review_coord_integration.rs` | One active background review per snapshot, supersession, orphan reconciliation, and ghost-commit/worktree identity. | Rewrite | A coordinator fixture that proves current Codex thread/review primitives can supersede stale evidence before adding any new store. |
| Old `code exec --auto-review` review-model and auto-resolve path | Dedicated background review models and follow-up limits for retired Auto Drive review gates. | Retire for Auto Drive | #398 retires the standalone Auto Drive coordinator boundary. Preserve only review/guardian proof value that #400 maps onto current Codex review primitives. |
| Auto-review run output snapshots | Stable finding digests and reviewer output artifacts for later status display. | Rewrite | A structured `ReviewOutputEvent` fixture with digest/summary expectations. |

## Deferred Or Retired Semantics

| Semantics | Decision | Rationale |
| --- | --- | --- |
| Recreating the old JSONL proof store schema exactly. | Defer | Current Codex-base has app-server thread/review primitives; old storage shape may be an implementation detail rather than product contract. |
| Treating broad background auto-review as the only proof source. | Retire | Current broad auto-review can fail before findings when the diff exceeds size limits. For example, run `398c40ae-0323-4e2b-9e01-46fbb80e8d01` failed before findings because the diff was 1,479,472 chars and the background limit was 120,000 chars. Deterministic no-token fixtures are required for port slices. |
| Reusing deleted `auto_review_model` config keys for plain chat turns. | Retire for now | Current `review_model` belongs to review primitives; auto-review approval routing is configured through `approvals_reviewer = "auto_review"`. |

## Validation Rule

Do not port more Auto Review implementation code until the specific proof metric
has a current fixture in one of these layers:

- Rust unit/integration test for internal review or guardian primitives.
- `tools/code-exec-harness` scenario for CLI/app-server behavior.
- TUI snapshot or app-server protocol fixture for user-visible review evidence.

Every code-bearing Auto Review slice still ends with `./build-fast.sh` passing
cleanly.
