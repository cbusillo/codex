#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/local-release-build.sh [--tag TAG] [--skip-tests] [--no-clean]

Build the exact binaries for a tagged fork release (rust-vX.Y.Z[-alpha.YYYYMMDD])
locally. By default, picks the most recent `rust-v*` tag in this repo.

Options:
  --tag TAG       Build this tag instead of the latest (e.g. rust-v0.32.0-alpha.20250910)
  --skip-tests    Skip running `cargo test` before building
  --no-clean      Keep build artifacts and the temporary build branch

The script will:
  1) fetch tags
  2) checkout the tag on a temporary branch
  3) run a lean test set (core/common/protocol)
  4) build the release binaries
  5) print `codex --version` and `codex --help` snippet
  6) clean artifacts and delete the temp branch (unless --no-clean)
USAGE
}

die() { echo "error: $*" >&2; exit 1; }

# Parse args
TAG=""
SKIP_TESTS=0
NO_CLEAN=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG=${2-}; shift 2;;
    --skip-tests) SKIP_TESTS=1; shift;;
    --no-clean) NO_CLEAN=1; shift;;
    -h|--help) usage; exit 0;;
    *) die "unknown arg: $1";;
  esac
done

command -v git >/dev/null || die "git is required"
command -v cargo >/dev/null || die "cargo is required"

# Fetch tags from origin
git fetch origin --tags --prune || true

if [[ -z "$TAG" ]]; then
  TAG=$(git tag -l 'rust-v*' --sort='-version:refname' | head -n1)
  [[ -n "$TAG" ]] || die "no rust-v* tags found"
fi

[[ $TAG == rust-v* ]] || die "TAG must start with 'rust-v' (got '$TAG')"

BASE_BRANCH=$(git rev-parse --abbrev-ref HEAD)
BUILD_BRANCH="build-${TAG//\//-}-$(date -u +%Y%m%d%H%M%S)"

echo "==> Checking out $TAG on $BUILD_BRANCH"
git switch -c "$BUILD_BRANCH" "tags/$TAG"

pushd codex-rs >/dev/null

if [[ $SKIP_TESTS -eq 0 ]]; then
  echo "==> Running smoke tests (core/common/protocol)"
  cargo test -p codex-core -p codex-common -p codex-protocol --quiet
fi

echo "==> Building release binaries (workspace)"
cargo build --workspace --release

BIN="target/release/codex"
if [[ ! -x "$BIN" && -x "${BIN}.exe" ]]; then BIN="${BIN}.exe"; fi

if [[ -x "$BIN" ]]; then
  echo "==> Version check"
  "$BIN" --version || true
  echo "==> Help (first 20 lines)"
  "$BIN" --help | head -n 20 || true
  echo "==> Binary path: $BIN"
else
  echo "warn: codex binary not found at $BIN; check build output" >&2
fi

if [[ $NO_CLEAN -eq 0 ]]; then
  echo "==> Cleaning artifacts and removing temp branch"
  cargo clean || true
  popd >/dev/null
  git switch "$BASE_BRANCH" >/dev/null 2>&1 || true
  git branch -D "$BUILD_BRANCH" >/dev/null 2>&1 || true
else
  popd >/dev/null
  echo "(kept artifacts and branch $BUILD_BRANCH)"
fi

echo "Done."

