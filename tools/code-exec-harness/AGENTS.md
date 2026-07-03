# Code Exec Harness Agent Guidance

Use this harness as the default proving ground for realistic `code exec`
behavior when working on token efficiency, prompt/context composition, skills,
memory, compaction, resume, model routing, tool choice, and GitHub automation.

For Dogfood Parity 1, the deterministic no-token smoke suite is the P0 gate for
the built `code` binary. Run `just harness-smoke` after `./build-fast.sh`; it
uses `code-rs/target/dev-fast/code` by default and fails clearly if the binary is
missing or the covered `code exec --json` contract regresses. Use
`CODE_EXEC_HARNESS_BIN=/path/to/code just harness-smoke` only when intentionally
checking another binary.

The goal is effectiveness-adjusted token efficiency: reduce wasted tokens only
when task success, reliability, instruction following, and useful tool behavior
are preserved or improved. Saving tokens by making the agent less capable is a
regression.

Validation order:

1. Prefer deterministic fake `/v1/responses` scenarios when request bodies,
   JSONL events, fake service state, or tool calls can prove the behavior.
2. Use live model runs when the question depends on model behavior that a fake
   response cannot represent.
3. Use local model runs when available and adequate for the scenario,
   especially for repeated exploratory checks.
4. Keep lower-level Rust tests for local mechanics, but add a harness scenario
   when the risk crosses config loading, skills, memory, resume, prompt
   assembly, tool routing, or full `code exec` behavior.

When adding or running scenarios, preserve evidence future agents can compare:

- request shape and duplicated context markers
- token usage when available
- tool commands and fake service calls
- final answer quality or task completion signal
- resume and compaction behavior across turns
- unexpected retries, confusion, or wasted work

For context-bloat regressions, prefer exact-count assertions over broad
contains/not_contains checks. A project doc, skill manifest, memory block,
summary, or large artifact placeholder should appear the intended number of
times, usually once.

Assert the request shape that the real executable sends, not the shape of a
lower-level helper. For example, `code exec` renders project guidance as an
`AGENTS.md instructions for ...` user-context message, while lower-level core
helpers may use internal separators such as `--- project-doc ---`.

When a harness scenario exposes a stale or ambiguous config surface, document it
as a separate issue or issue comment instead of forcing a scenario to pass by
asserting the wrong behavior. Keep each scenario focused on one product claim.

Spending real OpenAI tokens in this harness is acceptable when it answers an
effectiveness question. Try fake responses first, but do not optimize away the
evidence needed to avoid larger token waste in real sessions.
