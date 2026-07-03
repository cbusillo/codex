#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/local/rebuild-path-code.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/code-rebuild-preflight.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/repo/code-rs/target/release" "$tmp/bin" "$tmp/external-target/release"
release_bin="$tmp/repo/code-rs/target/release/code"
external_bin="$tmp/external-target/release/code"

write_fake_code() {
	local path="$1"
	local label="$2"
	mkdir -p "$(dirname "$path")"
	cat >"$path" <<EOF
#!/usr/bin/env bash
echo "$label"
EOF
	chmod +x "$path"
}

run_preflight() {
	local output="$1"
	shift
	set +e
	CODE_LOCAL_REBUILD_CODE_RS_ROOT="$tmp/repo/code-rs" \
		CODE_LOCAL_REBUILD_RELEASE_BIN="$release_bin" \
		CARGO_TARGET_DIR="$tmp/external-target" \
		PATH="$tmp/bin:$PATH" \
		"$script" --preflight "$@" >"$output" 2>&1
	local status="$?"
	set -e
	return "$status"
}

assert_contains() {
	local file="$1"
	local expected="$2"
	if ! grep -Fq "$expected" "$file"; then
		echo "error: expected output to contain: $expected" >&2
		echo "--- output ---" >&2
		cat "$file" >&2
		exit 1
	fi
}

assert_not_exists() {
	local path="$1"
	if [[ -e "$path" || -L "$path" ]]; then
		echo "error: preflight unexpectedly created or modified $path" >&2
		exit 1
	fi
}

write_fake_code "$external_bin" "external target code"
ln -s "$external_bin" "$release_bin"
ln -s "$release_bin" "$tmp/bin/code"
matching_output="$tmp/matching.out"
run_preflight "$matching_output"
assert_contains "$matching_output" "Local code rebuild preflight"
assert_contains "$matching_output" "Release bin is a symlink: $release_bin -> $external_bin"
assert_contains "$matching_output" "restore it if the build failed"
assert_contains "$matching_output" "SAFE: PATH resolves to the local release binary."

rm -f "$tmp/bin/code"
write_fake_code "$tmp/bin/code" "unrelated path code"
mismatch_output="$tmp/mismatch.out"
if run_preflight "$mismatch_output"; then
	echo "error: mismatched PATH preflight unexpectedly succeeded" >&2
	exit 1
fi
assert_contains "$mismatch_output" "WARNING: PATH does not resolve to the freshly built binary"
assert_contains "$mismatch_output" "External target dir is active"

missing_release="$tmp/missing/release/code"
missing_output="$tmp/missing.out"
release_bin="$missing_release"
if run_preflight "$missing_output"; then
	echo "error: missing release-bin preflight unexpectedly succeeded with unrelated PATH code" >&2
	exit 1
fi
assert_contains "$missing_output" "Release bin does not exist yet and would be created by cargo build."
assert_not_exists "$missing_release"

missing_path_output="$tmp/missing-path.out"
empty_path="$tmp/empty-path"
mkdir -p "$empty_path"
set +e
CODE_LOCAL_REBUILD_CODE_RS_ROOT="$tmp/repo/code-rs" \
	CODE_LOCAL_REBUILD_RELEASE_BIN="$release_bin" \
	CARGO_TARGET_DIR="$tmp/external-target" \
	PATH="$empty_path:/usr/bin:/bin" \
	"$script" --preflight >"$missing_path_output" 2>&1
missing_path_status="$?"
set -e
if [[ "$missing_path_status" -ne 1 ]]; then
	echo "error: missing PATH preflight expected exit 1, got $missing_path_status" >&2
	cat "$missing_path_output" >&2
	exit 1
fi
assert_contains "$missing_path_output" "NOT SAFE: could not find 'code' on PATH"

echo "code rebuild preflight checks passed"
