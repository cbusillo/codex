#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SCRIPT="$ROOT_DIR/scripts/wait-for-gh-run.sh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  [[ "$haystack" == *"$needle"* ]] || fail "expected output to contain: $needle"
}

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p "$tmp_dir/bin"
cat >"$tmp_dir/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"$GH_MOCK_LOG"

if [[ "${1:-}" == "-R" ]]; then
  shift 2
fi

case "$GH_MOCK_SCENARIO:${1:-} ${2:-}" in
  "waiting:run view")
    cat <<'JSON'
{"status":"waiting","conclusion":null,"displayTitle":"Deploy production","workflowName":"Deploy","headBranch":"main","url":"https://github.com/example/repo/actions/runs/4242","startedAt":"2026-07-25T20:01:33Z","updatedAt":"2026-07-25T20:01:40Z","jobs":[]}
JSON
    ;;
  "waiting:api repos/example/repo/actions/runs/4242/pending_deployments")
    cat <<'JSON'
[{"environment":{"id":7,"name":"launchplane-authz-admin"},"current_user_can_approve":false,"wait_timer":0,"reviewers":[]}]
JSON
    ;;
  "timeout:run view")
    cat <<'JSON'
{"status":"in_progress","conclusion":null,"displayTitle":"Build","workflowName":"CI","headBranch":"main","url":"https://github.com/example/repo/actions/runs/4242","startedAt":"2026-07-25T20:01:33Z","updatedAt":"2026-07-25T20:01:40Z","jobs":[{"databaseId":8,"name":"build","status":"in_progress","conclusion":null}]}
JSON
    ;;
  "lifecycle:run view")
    count=0
    if [[ -f "$GH_MOCK_STATE" ]]; then
      count=$(cat "$GH_MOCK_STATE")
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$GH_MOCK_STATE"
    if ((count == 1)); then
      cat <<'JSON'
{"status":"queued","conclusion":null,"displayTitle":"Build","workflowName":"CI","headBranch":"main","url":"https://github.com/example/repo/actions/runs/4242","startedAt":"2026-07-25T20:01:33Z","updatedAt":"2026-07-25T20:01:40Z","jobs":[{"databaseId":8,"name":"build","status":"queued","conclusion":null}]}
JSON
    elif ((count == 2)); then
      cat <<'JSON'
{"status":"in_progress","conclusion":null,"displayTitle":"Build","workflowName":"CI","headBranch":"main","url":"https://github.com/example/repo/actions/runs/4242","startedAt":"2026-07-25T20:01:33Z","updatedAt":"2026-07-25T20:01:41Z","jobs":[{"databaseId":8,"name":"build","status":"in_progress","conclusion":null}]}
JSON
    else
      cat <<'JSON'
{"status":"completed","conclusion":"success","displayTitle":"Build","workflowName":"CI","headBranch":"main","url":"https://github.com/example/repo/actions/runs/4242","startedAt":"2026-07-25T20:01:33Z","updatedAt":"2026-07-25T20:01:42Z","jobs":[{"databaseId":8,"name":"build","status":"completed","conclusion":"success"}]}
JSON
    fi
    ;;
  "select:run list")
    cat <<'JSON'
[{"databaseId":4242,"workflowName":"CI","displayTitle":"Build","headBranch":"main","headSha":"abc123"}]
JSON
    ;;
  "select:run view")
    cat <<'JSON'
{"status":"completed","conclusion":"success","displayTitle":"Build","workflowName":"CI","headBranch":"main","url":"https://github.com/example/repo/actions/runs/4242","startedAt":"2026-07-25T20:01:33Z","updatedAt":"2026-07-25T20:01:42Z","jobs":[{"databaseId":8,"name":"build","status":"completed","conclusion":"success"}]}
JSON
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 64
    ;;
esac
EOF
chmod +x "$tmp_dir/bin/gh"

run_wait() {
  local scenario="$1"
  shift
  : >"$tmp_dir/gh.log"
  rm -f "$tmp_dir/gh.state"
  set +e
  PATH="$tmp_dir/bin:$PATH" \
    GH_MOCK_LOG="$tmp_dir/gh.log" \
    GH_MOCK_SCENARIO="$scenario" \
    GH_MOCK_STATE="$tmp_dir/gh.state" \
    bash "$SCRIPT" "$@" >"$tmp_dir/output" 2>&1
  WAIT_STATUS=$?
  set -e
  WAIT_OUTPUT=$(cat "$tmp_dir/output")
}

run_wait waiting \
  --run 4242 \
  --repo example/repo \
  --interval 1 \
  --timeout 30
[[ $WAIT_STATUS -eq 2 ]] || fail "expected protected-environment exit 2, got $WAIT_STATUS; output: $WAIT_OUTPUT"
assert_contains "$WAIT_OUTPUT" "launchplane-authz-admin"
assert_contains "$WAIT_OUTPUT" "current GitHub CLI identity can approve: no"
assert_contains "$WAIT_OUTPUT" "exact-run babysitter"
assert_contains "$(cat "$tmp_dir/gh.log")" "api repos/example/repo/actions/runs/4242/pending_deployments"
if grep -Eq '(^| )(--method|-X) (POST|PUT|PATCH|DELETE)($| )' "$tmp_dir/gh.log"; then
  fail "generic waiter must not mutate pending deployments"
fi

run_wait timeout \
  --run 4242 \
  --repo example/repo \
  --interval 1 \
  --timeout 1
[[ $WAIT_STATUS -eq 124 ]] || fail "expected timeout exit 124, got $WAIT_STATUS; output: $WAIT_OUTPUT"
assert_contains "$WAIT_OUTPUT" "Timed out after"
assert_contains "$WAIT_OUTPUT" "exact-run babysitter"

run_wait lifecycle \
  --run 4242 \
  --repo example/repo \
  --interval 1 \
  --timeout 5
[[ $WAIT_STATUS -eq 0 ]] || fail "expected queued/in_progress/completed lifecycle to succeed, got $WAIT_STATUS; output: $WAIT_OUTPUT"
assert_contains "$WAIT_OUTPUT" "status: queued"
assert_contains "$WAIT_OUTPUT" "status: in_progress"
assert_contains "$WAIT_OUTPUT" "Run 4242 succeeded"

run_wait select \
  --repo example/repo \
  --workflow CI \
  --branch main \
  --head-sha abc123 \
  --interval 1 \
  --timeout 5
[[ $WAIT_STATUS -eq 0 ]] || fail "expected exact commit selection to succeed, got $WAIT_STATUS; output: $WAIT_OUTPUT"
assert_contains "$(cat "$tmp_dir/gh.log")" "run list --workflow CI --branch main --limit 1"
assert_contains "$(cat "$tmp_dir/gh.log")" "--commit abc123"
if grep -q -- "--limit 20" "$tmp_dir/gh.log"; then
  fail "exact commit selection must use GitHub's commit filter instead of a fixed 20-run window"
fi

echo "PASS: waiting diagnostics, bounded timeout, and normal lifecycle behavior"
