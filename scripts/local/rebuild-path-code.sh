#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
code_rs_root="$repo_root/code-rs"
release_bin="$code_rs_root/target/release/code"
cargo_target_dir="${CARGO_TARGET_DIR:-$code_rs_root/target}"
if [[ "$cargo_target_dir" != /* ]]; then
	cargo_target_dir="$(pwd)/$cargo_target_dir"
fi
build_profile="${LOCAL_CODE_REBUILD_PROFILE:-perf}"
if [[ -z "$build_profile" ]]; then
	build_profile="perf"
fi
cargo_profile_dir="$build_profile"
if [[ "$build_profile" == "release" ]]; then
	cargo_profile_arg=(--release)
	cargo_profile_dir="release"
else
	cargo_profile_arg=(--profile "$build_profile")
fi
cargo_release_bin="$cargo_target_dir/$cargo_profile_dir/code"

resolve_code_version() {
	tr -d '[:space:]' <"$repo_root/VERSION"
}

code_version="${CODE_VERSION:-$(resolve_code_version || true)}"

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
echo "Using Cargo profile: $build_profile"
if [[ -L "$release_bin" ]]; then
	removed_release_symlink_target="$(readlink "$release_bin")"
	trap restore_release_symlink_on_error EXIT
	echo "Replacing release-bin symlink with a real binary: $release_bin"
	rm -f "$release_bin"
fi
if [[ -n "$code_version" ]]; then
	echo "Embedding CODE_VERSION=$code_version"
	CODE_VERSION="$code_version" cargo build --manifest-path "$code_rs_root/Cargo.toml" -p code-cli "${cargo_profile_arg[@]}"
else
	echo "warning: could not resolve CODE_VERSION from VERSION; building without override" >&2
	cargo build --manifest-path "$code_rs_root/Cargo.toml" -p code-cli "${cargo_profile_arg[@]}"
fi
trap - EXIT

if [[ "$cargo_release_bin" != "$release_bin" ]]; then
	mkdir -p "$(dirname "$release_bin")"
	rm -f "$release_bin"
	ln -s "$cargo_release_bin" "$release_bin"
	echo "Updated release-bin symlink for external target dir: $release_bin -> $cargo_release_bin"
fi

path_code="$(command -v code || true)"
if [[ -z "$path_code" ]]; then
	echo "error: could not find 'code' on PATH" >&2
	exit 1
fi

resolved_path="$(
	python3 - "$path_code" <<'PY'
import os, sys
print(os.path.realpath(sys.argv[1]))
PY
)"
resolved_release_bin="$(
	python3 - "$release_bin" <<'PY'
import os, sys
print(os.path.realpath(sys.argv[1]))
PY
)"

echo
echo "PATH entry:   $path_code"
echo "Resolves to:  $resolved_path"
echo "Release bin:  $release_bin"

if [[ "$resolved_path" != "$release_bin" && "$resolved_path" != "$resolved_release_bin" ]]; then
	echo "warning: PATH does not resolve to the freshly built binary" >&2
fi

stat -f 'Updated: %Sm %N' "$release_bin"
"$path_code" --version
