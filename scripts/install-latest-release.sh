#!/usr/bin/env bash
set -euo pipefail

# Install the newest successful GitHub Release artifact for this fork.
# Defaults:
#   REPO=cbusillo/codex
#   TARGET=auto-detect (host OS/arch)
#   DEST=auto-detect (Homebrew on macOS; /usr/local/bin elsewhere)
#
# Usage:
#   scripts/install-latest-release.sh [--repo OWNER/REPO] [--tag TAG]
#                                     [--target TRIPLE] [--dest PATH]
#                                     [--no-sudo] [--dry-run]

REPO="cbusillo/codex"
TAG=""
TARGET=""
DEST=""
USE_SUDO=1
DRY_RUN=0

die() { echo "error: $*" >&2; exit 1; }
log() { echo "==> $*"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) REPO=${2-}; shift 2;;
    --tag) TAG=${2-}; shift 2;;
    --target) TARGET=${2-}; shift 2;;
    --dest) DEST=${2-}; shift 2;;
    --no-sudo) USE_SUDO=0; shift;;
    --dry-run) DRY_RUN=1; shift;;
    -h|--help) sed -n '1,40p' "$0"; exit 0;;
    *) die "unknown arg: $1";;
  esac
done

command -v gh >/dev/null || die "gh (GitHub CLI) is required"
command -v jq >/dev/null || die "jq is required"

# Detect target triple if not supplied
if [[ -z "$TARGET" ]]; then
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin)
      case "$arch" in
        arm64) TARGET="aarch64-apple-darwin";;
        x86_64) TARGET="x86_64-apple-darwin";;
        *) die "unsupported macOS arch: $arch";;
      esac;;
    Linux)
      case "$arch" in
        x86_64) TARGET="x86_64-unknown-linux-gnu";;
        aarch64) TARGET="aarch64-unknown-linux-gnu";;
        *) die "unsupported Linux arch: $arch";;
      esac;;
    MINGW*|MSYS*|CYGWIN*)
      case "$arch" in
        x86_64) TARGET="x86_64-pc-windows-msvc";;
        aarch64|arm64) TARGET="aarch64-pc-windows-msvc";;
        *) die "unsupported Windows arch: $arch";;
      esac;;
    *) die "unsupported OS: $os";;
  esac
fi

# Default install destination
if [[ -z "$DEST" ]]; then
  case "$(uname -s)" in
    Darwin)
      if command -v brew >/dev/null 2>&1; then
        prefix=$(brew --prefix 2>/dev/null || true)
        if [[ -n "$prefix" ]]; then DEST="$prefix/bin/codex"; fi
      fi
      DEST=${DEST:-/opt/homebrew/bin/codex}
      if [[ ! -d /opt/homebrew/bin ]]; then DEST=/usr/local/bin/codex; fi ;;
    Linux) DEST=/usr/local/bin/codex ;;
    MINGW*|MSYS*|CYGWIN*) DEST="$HOME/.local/bin/codex.exe" ;;
    *) DEST=/usr/local/bin/codex ;;
  esac
fi

log "repo=$REPO target=$TARGET dest=$DEST"

tmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t codex-install)
cleanup() { rm -rf "$tmpdir"; }
trap cleanup EXIT

# Choose a tag: newest release that has our asset
if [[ -z "$TAG" ]]; then
  for ext in tar.gz zst; do
    TAG=$(gh api -H 'Accept: application/vnd.github+json' \
          "/repos/$REPO/releases?per_page=100" \
          | jq -r --arg pat "codex-$TARGET.${ext}" '
              map(select(.draft==false))
              | sort_by(.created_at) | reverse
              | map(select(.assets | any(.name == $pat)))
              | .[0].tag_name')
    [[ -n "$TAG" && "$TAG" != null ]] && break
  done
  [[ -n "$TAG" && "$TAG" != null ]] || die "no suitable release found for $TARGET"
fi

log "selected tag: $TAG"

# Decide asset name (no regex quirks)
rel_json=$(gh api -H 'Accept: application/vnd.github+json' "/repos/$REPO/releases/tags/$TAG")
if [[ "$TARGET" == *-pc-windows-msvc ]]; then
  candidates=("codex-$TARGET.exe.tar.gz" "codex-$TARGET.exe.zst" "codex-$TARGET.exe.zip")
else
  candidates=("codex-$TARGET.tar.gz" "codex-$TARGET.zst")
fi
ASSET=""
for cand in "${candidates[@]}"; do
  present=$(echo "$rel_json" | jq -r --arg n "$cand" 'any(.assets[]?; .name==$n)')
  if [[ "$present" == "true" ]]; then ASSET="$cand"; break; fi
done
[[ -n "$ASSET" ]] || die "no suitable asset found for $TARGET in $TAG"

log "downloading $ASSET"
if [[ $DRY_RUN -eq 1 ]]; then
  echo "gh release download -R $REPO $TAG -p $ASSET -D $tmpdir"
  exit 0
fi
gh release download -R "$REPO" "$TAG" -p "$ASSET" -D "$tmpdir"

# Unpack
src_bin=""
if [[ "$ASSET" == *.tar.gz ]]; then
  mkdir -p "$tmpdir/unpack"
  tar -C "$tmpdir/unpack" -xzf "$tmpdir/$ASSET"
  base=${ASSET%.tar.gz}
  src_bin="$tmpdir/unpack/$base"
elif [[ "$ASSET" == *.zst ]]; then
  command -v zstd >/dev/null || die "zstd is required to unpack .zst"
  base=${ASSET%.zst}
  zstd -d -c "$tmpdir/$ASSET" > "$tmpdir/$base"
  src_bin="$tmpdir/$base"
else
  die "unknown asset format: $ASSET"
fi

[[ -f "$src_bin" ]] || die "binary not found after unpack: $src_bin"

# Install (sudo only if needed)
mkdir -p "$(dirname "$DEST")" 2>/dev/null || true
if [[ $USE_SUDO -eq 1 && ! -w "$(dirname "$DEST")" ]]; then
  sudo install -m 0755 "$src_bin" "$DEST"
else
  install -m 0755 "$src_bin" "$DEST"
fi

log "installed to $DEST"
"$DEST" --version || true

