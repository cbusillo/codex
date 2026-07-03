#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
code_rs_root="${CODE_LOCAL_REBUILD_CODE_RS_ROOT:-$repo_root/code-rs}"
release_bin="${CODE_LOCAL_REBUILD_RELEASE_BIN:-$code_rs_root/target/release/code}"
cargo_target_dir="${CARGO_TARGET_DIR:-$code_rs_root/target}"
if [[ "$cargo_target_dir" != /* ]]; then
	cargo_target_dir="$(pwd)/$cargo_target_dir"
fi
cargo_release_bin="$cargo_target_dir/release/code"
preflight=0

resolve_code_version() {
	tr -d '[:space:]' <"$repo_root/VERSION"
}

code_version="${CODE_VERSION:-$(resolve_code_version || true)}"

usage() {
	cat <<USAGE
Usage: scripts/local/rebuild-path-code.sh [--preflight]

Build the release binary that the PATH-resolved local 'code' command should use.

Options:
  --preflight  Print the current PATH/release-bin wiring and safety decisions
               without building, deleting, or replacing anything.
  --dry-run    Alias for --preflight.
  -h, --help   Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
	case "$1" in
	--preflight | --dry-run)
		preflight=1
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "error: unknown option: $1" >&2
		usage >&2
		exit 2
		;;
	esac
	shift
done

real_path() {
	python3 - "$1" <<'PY'
import os, sys
print(os.path.realpath(sys.argv[1]))
PY
}

print_path_wiring() {
	local path_code="$1"
	local resolved_path=""
	local resolved_release_bin=""

	if [[ -n "$path_code" ]]; then
		resolved_path="$(real_path "$path_code")"
	fi
	resolved_release_bin="$(real_path "$release_bin")"

	echo
	echo "PATH entry:   ${path_code:-<missing>}"
	if [[ -n "$path_code" ]]; then
		echo "Resolves to:  $resolved_path"
	fi
	echo "Release bin:  $release_bin"
	echo "Release real: $resolved_release_bin"

	if [[ -z "$path_code" ]]; then
		echo "NOT SAFE: could not find 'code' on PATH" >&2
		return 1
	fi

	if [[ "$resolved_path" != "$release_bin" && "$resolved_path" != "$resolved_release_bin" ]]; then
		echo "WARNING: PATH does not resolve to the freshly built binary" >&2
		return 2
	fi

	echo "SAFE: PATH resolves to the local release binary."
	return 0
}

path_code="$(command -v code || true)"

if [[ "$preflight" -eq 1 ]]; then
	echo "Local code rebuild preflight"
	echo "Repo root:    $repo_root"
	echo "Code root:    $code_rs_root"
	echo "Target dir:   $cargo_target_dir"
	echo "Cargo bin:    $cargo_release_bin"
	if [[ -L "$release_bin" ]]; then
		echo "Release bin is a symlink: $release_bin -> $(readlink "$release_bin")"
		echo "A rebuild would replace it during cargo build and restore it if the build failed."
	elif [[ -e "$release_bin" ]]; then
		echo "Release bin is an existing file and would be replaced by cargo build."
	else
		echo "Release bin does not exist yet and would be created by cargo build."
	fi
	if [[ "$cargo_release_bin" != "$release_bin" ]]; then
		echo "External target dir is active; a successful rebuild would link $release_bin -> $cargo_release_bin."
	fi
	set +e
	print_path_wiring "$path_code"
	status="$?"
	set -e
	if [[ "$status" -eq 2 ]]; then
		exit 2
	fi
	exit "$status"
fi

removed_release_symlink_target=""
restore_release_symlink_on_error() {
	local exit_code="$?"
	if [[ "$exit_code" -ne 0 && -n "$removed_release_symlink_target" && ! -e "$release_bin" && ! -L "$release_bin" ]]; then
		mkdir -p "$(dirname "$release_bin")"
		ln -s "$removed_release_symlink_target" "$release_bin"
		echo "Restored release-bin symlink after failed rebuild: $release_bin -> $removed_release_symlink_target" >&2
	fi
	exit "$exit_code"
}

echo "Building release binary from $code_rs_root"
if [[ -L "$release_bin" ]]; then
	removed_release_symlink_target="$(readlink "$release_bin")"
	trap restore_release_symlink_on_error EXIT
	echo "Replacing release-bin symlink with a real binary: $release_bin"
	rm -f "$release_bin"
fi
if [[ -n "$code_version" ]]; then
	echo "Embedding CODE_VERSION=$code_version"
	CODE_VERSION="$code_version" cargo build --manifest-path "$code_rs_root/Cargo.toml" -p code-cli --release
else
	echo "warning: could not resolve CODE_VERSION from VERSION; building without override" >&2
	cargo build --manifest-path "$code_rs_root/Cargo.toml" -p code-cli --release
fi
trap - EXIT

if [[ "$cargo_release_bin" != "$release_bin" ]]; then
	mkdir -p "$(dirname "$release_bin")"
	rm -f "$release_bin"
	ln -s "$cargo_release_bin" "$release_bin"
	echo "Updated release-bin symlink for external target dir: $release_bin -> $cargo_release_bin"
fi

set +e
print_path_wiring "$path_code"
wiring_status="$?"
set -e
if [[ "$wiring_status" -eq 1 ]]; then
	exit 1
fi

ls -l "$release_bin"
if [[ "$wiring_status" -eq 0 ]]; then
	"$path_code" --version
else
	echo "Skipping PATH 'code --version' because PATH does not resolve to the rebuilt binary." >&2
fi
