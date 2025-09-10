#!/usr/bin/env bash
set -euo pipefail

# Install the newest successful GitHub Release artifact for this fork.
# Defaults:
#   REPO=cbusillo/codex
#   TARGET=auto-detect (host OS/arch)
#   DEST=/usr/local/bin/codex
#
# Usage:
#   scripts/install-latest-release.sh [--repo OWNER/REPO] [--tag TAG]
#                                     [--target TRIPLE] [--dest PATH]
#                                     [--no-sudo] [--dry-run]
# Examples:
#   scripts/install-latest-release.sh
#   scripts/install-latest-release.sh --tag rust-v0.32.0-alpha.20250910
#   scripts/install-latest-release.sh --dest "$HOME/.local/bin/codex" --no-sudo

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
    -h|--help)
      sed -n '1,60p' "$0" | sed -n '1,40p' | sed -n '1,40p' # header
      exit 0;;
    *) die "unknown arg: $1";;
  esac
done

command -v gh >/dev/null || die "gh (GitHub CLI) is required"
command -v jq >/dev/null || die "jq is required"

# Auto-detect target triple and default DEST if not supplied
if [[ -z "$TARGET" ]]; then
  uname_s=$(uname -s)
  uname_m=$(uname -m)
  case "$uname_s" in
    Darwin)
      case "$uname_m" in
        arm64) TARGET="aarch64-apple-darwin";;
        x86_64) TARGET="x86_64-apple-darwin";;
        *) die "unsupported macOS arch: $uname_m";;
      esac;;
    Linux)
      case "$uname_m" in
        x86_64) TARGET="x86_64-unknown-linux-gnu";;
        aarch64) TARGET="aarch64-unknown-linux-gnu";;
        *) die "unsupported Linux arch: $uname_m";;
      esac;;
    MINGW*|MSYS*|CYGWIN*)
      case "$uname_m" in
        x86_64) TARGET="x86_64-pc-windows-msvc";;
        aarch64|arm64) TARGET="aarch64-pc-windows-msvc";;
        *) die "unsupported Windows arch: $uname_m";;
      esac;;
    *) die "unsupported OS: $uname_s";;
  esac
fi

# Default install destination by platform if not provided
if [[ -z "${DEST}" ]]; then
  case "$(uname -s)" in
    Darwin)
      if command -v brew >/dev/null 2>&1; then
        BREW_PREFIX=$(brew --prefix 2>/dev/null || true)
        if [[ -n "$BREW_PREFIX" ]]; then DEST="$BREW_PREFIX/bin/codex"; fi
      fi
      # Fallbacks
      DEST=${DEST:-/opt/homebrew/bin/codex}
      if [[ ! -d "/opt/homebrew/bin" ]]; then
        DEST=${DEST:-/usr/local/bin/codex}
      fi;;
    Linux)
      DEST="/usr/local/bin/codex";;
    MINGW*|MSYS*|CYGWIN*)
      DEST="$HOME/.local/bin/codex.exe";;
    *) DEST="/usr/local/bin/codex";;
  esac
fi

log "repo=$REPO target=$TARGET dest=$DEST"

tmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t codex-install)
cleanup() { rm -rf "$tmpdir"; }
trap cleanup EXIT

# Decide which release tag to use (prefer most recent release with our asset)
if [[ -z "$TAG" ]]; then
  log "querying latest release containing codex-$TARGET.tar.gz"
  TAG=$(gh api -H 'Accept: application/vnd.github+json' \
        "/repos/$REPO/releases?per_page=100" \
        | jq -r --arg pat "codex-$TARGET.tar.gz" '
            map(select(.draft==false))
            | sort_by(.created_at) | reverse
            | map(select(.assets | any(.name == $pat)))
            | .[0].tag_name'
       )
  if [[ -z "$TAG" || "$TAG" == null ]]; then
    # Fallback to .zst if .tar.gz not present
    TAG=$(gh api -H 'Accept: application/vnd.github+json' \
          "/repos/$REPO/releases?per_page=100" \
          | jq -r --arg pat "codex-$TARGET.zst" '
              map(select(.draft==false))
              | sort_by(.created_at) | reverse
              | map(select(.assets | any(.name == $pat)))
              | .[0].tag_name'
         )
  fi
  [[ -n "$TAG" && "$TAG" != null ]] || die "no suitable release found for $TARGET"
fi

log "selected tag: $TAG"

# Decide asset name (handles macOS/Linux and Windows .exe variants)
rel_json=$(gh api -H 'Accept: application/vnd.github+json' "/repos/$REPO/releases/tags/$TAG")
ASSET=$(echo "$rel_json" | jq -r --arg t "$TARGET" '
  (.assets[]? | .name) // empty
  | select(test("^codex-\Q"+$t+"\E(\\.exe)?\\.(tar\\.gz|zst)$"))
  | .[0]?')
if [[ -z "$ASSET" ]]; then
  die "no suitable asset found for $TARGET in $TAG"
fi

log "downloading $ASSET"
if [[ $DRY_RUN -eq 1 ]]; then
  echo "gh release download -R $REPO $TAG -p $ASSET -D $tmpdir"
  exit 0
fi

gh release download -R "$REPO" "$TAG" -p "$ASSET" -D "$tmpdir"

src_bin=""
if [[ "$ASSET" == *.tar.gz ]]; then
  mkdir -p "$tmpdir/unpack"
  tar -C "$tmpdir/unpack" -xzf "$tmpdir/$ASSET"
  # The tar contains a single file named like the asset
  base=$(basename "$ASSET" .tar.gz)
  src_bin="$tmpdir/unpack/$base"
elif [[ "$ASSET" == *.zst ]]; then
  command -v zstd >/dev/null || die "zstd is required to unpack .zst"
  base=$(basename "$ASSET" .zst)
  zstd -d -c "$tmpdir/$ASSET" > "$tmpdir/$base"
  src_bin="$tmpdir/$base"
else
  die "unknown asset format: $ASSET"
fi

[[ -f "$src_bin" ]] || die "binary not found after unpack: $src_bin"

# Decide whether sudo is needed (if not explicitly disabled)
mkdir -p "$(dirname "$DEST")" 2>/dev/null || true
if [[ $USE_SUDO -eq 1 ]]; then
  if [[ -w "$(dirname "$DEST")" ]]; then
    install -m 0755 "$src_bin" "$DEST"
  else
    sudo install -m 0755 "$src_bin" "$DEST"
  fi
else
  install -m 0755 "$src_bin" "$DEST"
fi

log "installed to $DEST"
"$DEST" --version || true
