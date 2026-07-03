#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
CODE_BIN=${CODE_EXEC_HARNESS_BIN:-"$ROOT_DIR/code-rs/target/dev-fast/code"}

if [ ! -x "$CODE_BIN" ]; then
  cat >&2 <<EOF
error: code exec harness binary is missing or not executable:
  $CODE_BIN

Run ./build-fast.sh first, or set CODE_EXEC_HARNESS_BIN=/path/to/code.
EOF
  exit 2
fi

scenarios=(
  "$ROOT_DIR/tools/code-exec-harness/scenarios/auto-review-config-routing.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/context-ledger-request-summary.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/exec-apply-patch-roundtrip.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/exec-basic-smoke.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/exec-readonly-write-denied.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/exec-shell-command-roundtrip.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/exec-workspace-write-edit.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/image-history-replay.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/manual-skill-explicit-invocation.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/manual-skill-not-implicit.json"
  "$ROOT_DIR/tools/code-exec-harness/scenarios/project-doc-skill-dedup.json"
)

python3 "$ROOT_DIR/tools/code-exec-harness/harness.py" \
  "${scenarios[@]}" \
  --code-bin "$CODE_BIN"
