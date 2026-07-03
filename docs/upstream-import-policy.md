# Upstream Import And Runtime Policy

Every Code owns its product direction, defaults, releases, and UX while using
Codex CLI as the upstream substrate. `main` is the canonical product branch and
GitHub default branch for the local `code` command on this machine.

Every Code owns `main`; upstream imports are synchronization work into the
product branch, not a long-running feature branch overlay.

## Naming Ledger

- **Every Code** is the product name. Use it in prose, docs, UI copy, issue text,
  release text, and first mentions.
- **Every Code Lab** is the `cbusillo/code` runtime/build tag, similar to a beta
  or lab channel label. Use it where users need to distinguish this build from
  upstream `just-every/code`; do not treat it as a product rebrand.
- **Every Code CLI** is the product CLI surface. **The `code` command** is the
  executable users type. Avoid “code CLI” in prose.
- **The Every Code agent** is the assistant identity. **Code** is only a short
  display name after Every Code context is established.
- **Every Code harness** is the runtime/session wrapper that we restart and
  dogfood. **Every Code runtime** is appropriate for process, session, and tool
  execution internals.
- **Upstream import sources** are `just-every/code` and `openai/codex`.
  `just-every/code` is a fork upstream/import source. `openai/codex` / Codex CLI
  is the original/direct upstream and provenance source.
- `codex-rs/` is a read-only local mirror of `openai/codex:main`. `code-rs/` is
  the editable Every Code Rust implementation built on the Codex CLI substrate.
  Align `code-rs/` toward upstream Codex CLI shapes first, then layer Every Code
  behavior through focused product overlays. Imported Codex substrate crates may
  keep `codex-*` names only while they remain compatibility-critical or mostly
  upstream-shaped; Every Code-owned crates and new product-layer crates should
  use `code-*` names unless a documented external compatibility contract
  requires the upstream spelling.
- `CODE_HOME` / `~/.code` are primary config and state locations. `CODEX_HOME` /
  `~/.codex` are compatibility fallbacks. Keep `CODEX_*` names where external,
  backend, or upstream compatibility requires them; add `CODE_*` aliases or
  rename only through scoped migrations.

## CODE And CODEX Compatibility Policy

Use `CODE_*` names for new Every Code-owned environment variables, config paths,
automation metadata, and locally-authored integration contracts. Use
`EVERY_CODE_*` only when the variable needs to be unmistakably product-branded
outside this repo, such as cross-repo remote inbox metadata.

Keep `CODE_HOME` / `~/.code` as the primary config and state location. Keep
`CODEX_HOME` / `~/.codex` as a compatibility fallback. The lookup order is
`CODE_HOME`, then `CODEX_HOME`, then default `~/.code`, with legacy `~/.codex`
read only where the specific subsystem supports migration or compatibility.
Every Code-owned writes should go to `~/.code` / `CODE_HOME` unless a scoped
compatibility feature explicitly says otherwise.

Preserve `CODEX_*` names when they are part of an external contract, upstream
merge surface, backend/API behavior, or documented compatibility behavior. This
includes names such as `CODEX_HOME`, `CODEX_API_KEY`, cloud/backend variables,
Bazel/upstream CI variables, and protocol/security names until a dedicated
migration adds `CODE_*` aliases with tests.

Dev-only or test-only `CODEX_TUI_*` variables are rename candidates, not silent
cleanup. A change should add a `CODE_TUI_*` alias first, keep the old
`CODEX_TUI_*` spelling for a compatibility window, document the precedence, and
include focused tests for both names before any removal.

Do not mass-rename deep Rust identifiers, telemetry prefixes, generated schema
types, model names, imported crate names, or `codex-rs` mirror paths just to
remove `codex`. Those names often preserve upstream compatibility, historical
provenance, or external dashboard continuity.

The pre-cutover `origin/main` history was archived at
`archive/pre-every-code-main-2026-05-24` before `main` was repointed to the Every
Code product branch. Treat `local/cbusillo-overlay` as a retired branch name;
do not revive it for new work.

Treat `codex-rs/` as a read-only local mirror of `openai/codex:main`; put
editable Rust changes under `code-rs/`. `code-rs/` must not depend on sibling
`../codex-rs` paths. Import or port the needed upstream source into `code-rs`
instead.

During Codex CLI substrate alignment, crate names are ownership markers:

- Imported Codex substrate crates may keep `codex-*` names when they remain
  compatibility-critical or mostly upstream-shaped.
- Every Code-owned crates and new product-layer crates should use `code-*`
  names unless a documented external compatibility contract requires the
  upstream spelling.
- Renaming a crate from `codex-*` to `code-*` should happen only when ownership,
  compatibility impact, and upstream merge cost are understood in that PR.

## Codex CLI Substrate Direction

[Issue #377](https://github.com/cbusillo/code/issues/377) proved the Codex
Desktop app launches its bundled CLI app-server and can reach the main chat UI
when a copied app bundle uses an Every Code binary.
[Issue #384](https://github.com/cbusillo/code/issues/384) owns the migration
from an Every Code-divergent Rust workspace toward Codex CLI as the substrate
with Every Code layered on top.

The working migration direction is:

- Make `code-rs/` the editable Codex CLI-substrate Every Code product workspace.
- Keep `codex-rs/` as the read-only `openai/codex:main` mirror for upstream
  review and provenance until a later issue explicitly changes that role.
- Prefer bringing Every Code code in `code-rs/` closer to upstream Codex CLI
  surfaces before adding product overlays. Do not make `codex-rs/` mirror Every
  Code.
- Keep imported Codex substrate crates internally named `codex-*` only when they
  remain compatibility-critical or mostly upstream-shaped.
- Add or port Every Code-owned features as `code-*` crates or product-layer
  modules unless a documented external compatibility contract requires upstream
  spelling.
- Keep the Desktop-facing app-server/session/protocol contract upstream-shaped;
  expose Every Code extensions through additive product surfaces rather than
  mutating compatibility-critical responses without evidence.

The raw `spike/codex-base-port` branch demonstrated feasibility but is not a
merge plan. Issue #384 is the acceptance gate for turning that spike into
reviewable PR slices. Those slices should be documented on #384 before they are
started, and each slice must state its compatibility target, ownership boundary,
and validation evidence. Each slice still ends with the repository gate,
`./build-fast.sh`, passing cleanly.

Current `main` is provisional until [#397](https://github.com/cbusillo/code/issues/397)
classifies the pre-#390 Every Code fixture and behavior surface. Use
`docs/codex-fork-parity-ledger.md` before assuming that an Every Code feature
missing from the Codex-base workspace was intentionally retired.

Do not remove the remote inbox backend merely because Codex Desktop is becoming
the preferred GUI. Remote inbox currently carries GitHub/LaunchPlane-created
session continuity and remote approval/reply flows. Drop or replace
Discord-specific presentation only after a Desktop or other replacement path can
discover, resume/continue, and answer externally-created sessions.

Remote map:

- `upstream`: `just-every/code`, a fork upstream/import source.
- `origin` / `fork`: `cbusillo/code`, where Every Code product branches and tags
  are pushed.
- `openai`: remote for `openai/codex`, the original/direct upstream and
  provenance source. Use it for direct upstream intake when intentional; do not
  push Every Code changes there.

Do not describe either import source as the permanent primary upstream. The
current import path may vary by task; the durable distinction is that
`just-every/code` is a fork upstream/import source and `openai/codex` is the
original/direct upstream and provenance source.

## Every Code-Owned Surfaces

- **Remote Inbox:** remote session control and request-user-input forwarding.
  Keep transport/protocol code isolated from TUI event handling where possible.
- **Goal-mode automation:** standalone Auto Drive is retired. Preserve useful
  long-running-work semantics through current goal/review/session surfaces
  instead of porting Auto Drive coordinator UI, commands, or status cards.
- **Patch harness:** local validation for changed files, project tool discovery,
  and workspace-aware validator execution.
- **Release workflow:** GitHub Releases and local PATH rebuilds are Every Code
  infrastructure. GitHub Releases are the canonical update source; npm and
  Homebrew publishing are not part of the current release path.
- **Model defaults:** local defaults may intentionally differ from upstream, but
  request wire compatibility should use upstream model metadata when available.

## Upstream Sync

Use the import helper from a clean `main` branch:

```sh
just local-upstream-import
```

After conflicts are resolved, preserve imported fixes unless they contradict an
Every Code-owned behavior above. During conflict resolution, prefer the import
source unless the conflict touches one of those owned areas. Keep
`scripts/local/upstream-picks.txt` empty unless a patch intentionally lives
outside the product branch and must be replayed.

### Upstream Review Cursors

Every Code is a product branch with two upstream references. Track
where review left off in `.github/upstream-cursors.json`, and inspect that state
with:

```sh
just local-upstream-cursors
```

The cursor file records `lastExamined` and `lastImported` separately for each
upstream source:

- `lastExamined` is the newest upstream commit whose delta has been reviewed or
  intentionally skipped. This is the cursor future sessions should compare from
  to avoid rereading old upstream commits.
- `lastImported` is the newest upstream commit actually merged, cherry-picked, or
  manually ported into Every Code. It can be older than, newer than, or absent
  relative to `lastExamined`, especially for direct `openai/codex` provenance
  reviews.

Do not infer either value from `git merge-base`. Merge bases are useful branch
health diagnostics, but they do not say what a human or agent already examined.

When reviewing upstream without importing, advance only `lastExamined` for that
source after the review is complete. When importing or manually porting upstream
work, update `lastImported` as well and include the product commit that contains
the port. Keep cursor changes in the same PR as the review/import so the next
session can start from the recorded SHA.

Fetch branch refs without tags when checking cursors. Upstream and Every Code
share `v*` tag names, and tag fetches can collide even when branch refs are
healthy.

## Release Cadence

Every Code releases are intentional dogfood distribution events, not an
automatic follow-up to every upstream import, local fix, or merged PR. Use the
release policy in `.github/github.json` to decide whether to cut immediately or
defer until nearby release-worthy fixes settle into one batch.

The active release workflow file is `.github/workflows/release.yml`, and GitHub
displays it as `Release Intent`. That name is intentional: the workflow runs
after relevant `main` pushes, first decides whether the committed `VERSION`
represents a new release, and only then publishes GitHub
Release assets.

Prepare release metadata locally with the Every Code harness only when cutting a
release intentionally: the local command bumps `VERSION`, updates `CHANGELOG.md`,
and writes `docs/release-notes/RELEASE_NOTES.md`. For this repo, the committed
`VERSION` value is release intent. If that version already has a tag, the
workflow exits successfully as a no-op; if the tag does not exist, it validates
the metadata and publishes exactly that committed version instead of generating
fallback notes in CI. The full preflight, macOS/Linux release matrix, and
Windows asset build run only on the publish pass after the metadata PR has
merged.

Release tags use the plain `v<version>` format, for example `v0.6.101`.

Required release gate:

```sh
git status --short --branch
./build-fast.sh
```

Install and smoke-check the local binary before tagging:

```sh
just local-code-rebuild-preflight
just local-code-rebuild
code --version
code exec -m gpt-5.5 --sandbox read-only --max-seconds 30 "Reply with exactly OK."
```

`just local-code-rebuild-preflight` does not build or mutate files. It prints
the current PATH entry, the release binary target, whether the release binary is
a symlink that would be restored after a failed rebuild, and a `SAFE` or
`WARNING` verdict for the local dogfood command wiring. Use
`just local-code-rebuild-preflight-check` to run deterministic temp-directory
fixtures for this preflight path without touching the real PATH-resolved
`code` command.

Run `just local-code-rebuild` after any release-readiness `./build-fast.sh` run:
the fast build creates dev-fast artifacts for validation, while the rebuild
recipe owns the PATH-resolved release binary and embeds `VERSION`.

Generate and review release metadata before pushing the release PR:

```sh
just local-release-notes
scripts/check-release-notes-version.sh
```

Commit the resulting `VERSION`, `CHANGELOG.md`, and
`docs/release-notes/RELEASE_NOTES.md` changes on a release metadata branch.

If an old manual Homebrew link exists, remove it so PATH resolution stays
repo-owned and predictable:

```sh
just local-remove-homebrew-code-link
```

Then push the release metadata branch to `origin`, merge the release metadata
PR, and monitor the `Release Intent` workflow:

```sh
git push origin HEAD
scripts/wait-for-gh-run.sh --workflow 'Release Intent' --branch main
```

A successful workflow run is not enough evidence that a release was published:
non-release runs also complete successfully as no-ops when `VERSION`
already has a tag. When cutting a release, verify the tag or GitHub Release
directly after the workflow succeeds:

```sh
gh release view v<version> --repo cbusillo/code
```

Do not push Every Code releases to `upstream` or `openai`.

## Local Cache Cleanup

During active local work, keep rebuildable caches bounded while preserving the
currently wired fast-build cache bucket:

```sh
just local-cleanup-space --apply --keep-current-fast-cache
```

This is the normal cleanup path when `.code/working/_target-cache` grows large.
It removes stale per-branch `./build-fast.sh` buckets and local debug artifacts,
but keeps the active `code-rs/target/dev-fast/code` cache warm for the next
build.

When intentionally ending a local work session and accepting a colder next
`./build-fast.sh`, reclaim all rebuildable artifacts:

```sh
just local-cleanup-space --apply
```

The cold cleanup is intentionally aggressive: it removes `codex-rs/target`,
legacy root targets, `code-rs` debug/dev-fast artifacts, all `./build-fast.sh`
target cache buckets, and release dependency cache. It preserves
`code-rs/target/release/code`, the PATH-resolved local binary built by
`just local-code-rebuild`.

Run without `--apply` to preview deletions. Use `--keep-release-cache` only when
you intentionally want to preserve release dependency cache as well.

## Fork Health

Run this before and after upstream syncs:

```sh
just local-product-health
```

Pay particular attention to changes under `code-rs/tui/src/chatwidget.rs`,
`code-rs/core/src/patch_harness.rs`, model/version files, and release scripts.
Those are the highest-conflict long-term carry points.
