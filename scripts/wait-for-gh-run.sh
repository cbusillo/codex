#!/usr/bin/env bash
# Poll a GitHub Actions run until it completes, printing status updates.
#
# Usage examples:
#   scripts/wait-for-gh-run.sh --run 17901972778
#   scripts/wait-for-gh-run.sh --workflow 'Release Intent' --branch main
#   scripts/wait-for-gh-run.sh  # picks latest run on current branch
#
# Dependencies: gh (GitHub CLI), jq.

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: wait-for-gh-run.sh [OPTIONS]

Options:
  -r, --run ID           Run ID to monitor.
  -R, --repo OWNER/REPO  Repository to monitor (default: repository at the current directory).
  -w, --workflow NAME    Workflow name or filename to pick the latest run.
  -b, --branch BRANCH    Branch to filter when selecting a run (default: current branch).
  -s, --head-sha SHA     Commit SHA to match when selecting the latest run.
  -i, --interval SECONDS Polling interval in seconds (default: 8).
  -t, --timeout SECONDS  Overall timeout in seconds (default: 1800; maximum: 7200).
  -L, --failure-logs     Print logs for any job that does not finish successfully.
  -h, --help             Show this help message.

If neither --run nor --workflow is provided, the latest run on the current
branch is selected automatically. A protected-environment wait exits 2 after
printing pending-deployment diagnostics. A timeout exits 124. This command
never approves a deployment.
EOF
}

require_binary() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: '$1' not found in PATH" >&2
    exit 1
  fi
}

RUN_ID=""
REPO=""
WORKFLOW=""
BRANCH=""
HEAD_SHA=""
INTERVAL="8"
TIMEOUT="1800"
MAX_TIMEOUT=7200
PRINT_FAILURE_LOGS=false
AUTO_SELECTED_RUN=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    -r|--run)
      RUN_ID="${2:-}"
      shift 2
      ;;
    -R|--repo)
      REPO="${2:-}"
      shift 2
      ;;
    -w|--workflow)
      WORKFLOW="${2:-}"
      shift 2
      ;;
    -b|--branch)
      BRANCH="${2:-}"
      shift 2
      ;;
    -i|--interval)
      INTERVAL="${2:-}"
      shift 2
      ;;
    -s|--head-sha)
      HEAD_SHA="${2:-}"
      shift 2
      ;;
    -t|--timeout)
      TIMEOUT="${2:-}"
      shift 2
      ;;
    -L|--failure-logs)
      PRINT_FAILURE_LOGS=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option '$1'" >&2
      usage >&2
      exit 1
      ;;
  esac
done

require_binary gh
require_binary jq

if [[ ! "$INTERVAL" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: --interval must be a positive integer" >&2
  exit 1
fi
if [[ ! "$TIMEOUT" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: --timeout must be a positive integer" >&2
  exit 1
fi

INTERVAL=$((10#$INTERVAL))
TIMEOUT=$((10#$TIMEOUT))

if ((TIMEOUT > MAX_TIMEOUT)); then
  echo "error: --timeout must be at most $MAX_TIMEOUT seconds" >&2
  exit 1
fi
if ((INTERVAL > TIMEOUT)); then
  echo "error: --interval must not exceed --timeout" >&2
  exit 1
fi
if [[ -n "$REPO" && ! "$REPO" =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]]; then
  echo "error: --repo must use OWNER/REPO format" >&2
  exit 1
fi

last_run_url=""
wait_started_seconds=$SECONDS

exit_on_timeout() {
  local elapsed=$((SECONDS - wait_started_seconds))
  echo "Timed out after $(format_duration "$elapsed") waiting for GitHub Actions run ${RUN_ID:-selection} (limit: $(format_duration "$TIMEOUT"))." >&2
  [[ -n "$last_run_url" ]] && echo "  $last_run_url" >&2
  echo "Retry gh_run_wait for the same exact run or use the exact-run babysitter for longer monitoring." >&2
  exit 124
}

check_timeout() {
  if ((SECONDS - wait_started_seconds >= TIMEOUT)); then
    exit_on_timeout
  fi
}

sleep_until_next_poll() {
  local elapsed=$((SECONDS - wait_started_seconds))
  local remaining=$((TIMEOUT - elapsed))
  local sleep_seconds=$INTERVAL
  if ((remaining <= 0)); then
    exit_on_timeout
  fi
  if ((sleep_seconds > remaining)); then
    sleep_seconds=$remaining
  fi
  sleep "$sleep_seconds"
}

GH_CAPTURED_OUTPUT=""
GH_CAPTURED_ERROR=""
ACTIVE_COMMAND_PID=""

stop_active_command() {
  local command_pid="${ACTIVE_COMMAND_PID:-}"
  if [[ -z "$command_pid" ]]; then
    return
  fi
  kill -TERM -- "-$command_pid" 2>/dev/null || kill "$command_pid" 2>/dev/null || true
  sleep 0.1
  kill -KILL -- "-$command_pid" 2>/dev/null || kill -9 "$command_pid" 2>/dev/null || true
  wait "$command_pid" 2>/dev/null || true
  ACTIVE_COMMAND_PID=""
}

handle_interrupt() {
  local exit_status="$1"
  stop_active_command
  exit "$exit_status"
}

trap 'handle_interrupt 130' INT
trap 'handle_interrupt 143' TERM HUP
trap stop_active_command EXIT

run_gh_capture() {
  check_timeout
  local stdout_file
  local stderr_file
  local command=(gh)
  local command_pid
  local command_status
  stdout_file=$(mktemp "${TMPDIR:-/tmp}/gh-run-wait.stdout.XXXXXX")
  stderr_file=$(mktemp "${TMPDIR:-/tmp}/gh-run-wait.stderr.XXXXXX")
  if [[ -n "$REPO" ]]; then
    command+=(-R "$REPO")
  fi
  command+=("$@")
  set -m
  "${command[@]}" >"$stdout_file" 2>"$stderr_file" &
  command_pid=$!
  ACTIVE_COMMAND_PID=$command_pid
  set +m
  while kill -0 "$command_pid" 2>/dev/null; do
    if ((SECONDS - wait_started_seconds >= TIMEOUT)); then
      stop_active_command
      rm -f "$stdout_file" "$stderr_file"
      exit_on_timeout
    fi
    sleep 0.1
  done
  if wait "$command_pid"; then
    command_status=0
  else
    command_status=$?
  fi
  ACTIVE_COMMAND_PID=""
  GH_CAPTURED_OUTPUT=$(cat "$stdout_file")
  GH_CAPTURED_ERROR=$(cat "$stderr_file")
  rm -f "$stdout_file" "$stderr_file"
  return "$command_status"
}

default_branch() {
  local branch=""
  if command -v git >/dev/null 2>&1; then
    if branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null); then
      if [[ -n "$branch" && "$branch" != "HEAD" ]]; then
        echo "$branch"
        return 0
      fi
    fi

    if branch=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null); then
      branch="${branch#origin/}"
      if [[ -n "$branch" ]]; then
        echo "$branch"
        return 0
      fi
    fi

  fi

  echo "main"
}

select_latest_run() {
  local workflow="$1"
  local branch="$2"
  local json
  local run_id
  local args=(run list --workflow "$workflow" --branch "$branch" --limit 1 --json databaseId,status,conclusion,displayTitle,workflowName,headBranch,headSha)
  if [[ -n "$HEAD_SHA" ]]; then
    args+=(--commit "$HEAD_SHA")
  fi
  if ! run_gh_capture "${args[@]}"; then
    echo "error: failed to list runs for workflow '$workflow'" >&2
    exit 1
  fi
  json=$GH_CAPTURED_OUTPUT

  if [[ $(jq 'length' <<<"$json") -eq 0 ]]; then
    echo "error: no runs found for workflow '$workflow' on branch '$branch'" >&2
    exit 1
  fi

  if [[ -n "$HEAD_SHA" ]]; then
    run_id=$(jq -r --arg head_sha "$HEAD_SHA" '
      .[0]
      | select(((.headSha // "") | ascii_downcase) == ($head_sha | ascii_downcase))
      | .databaseId // empty
    ' <<<"$json")
    if [[ -z "$run_id" ]]; then
      echo "error: no runs found for workflow '$workflow' on branch '$branch' at commit '$HEAD_SHA'" >&2
      exit 1
    fi
    printf '%s\n' "$run_id"
    return
  fi

  jq -r '.[0].databaseId' <<<"$json"
}

select_latest_run_any() {
  local branch="$1"
  local json
  local run_id
  local args=(run list --branch "$branch" --limit 1 --json databaseId,workflowName,displayTitle,headBranch,headSha)
  if [[ -n "$HEAD_SHA" ]]; then
    args+=(--commit "$HEAD_SHA")
  fi
  if ! run_gh_capture "${args[@]}"; then
    echo "error: failed to list runs on branch '$branch'" >&2
    exit 1
  fi
  json=$GH_CAPTURED_OUTPUT

  if [[ $(jq 'length' <<<"$json") -eq 0 ]]; then
    echo "error: no runs found on branch '$branch'" >&2
    exit 1
  fi

  if [[ -n "$HEAD_SHA" ]]; then
    run_id=$(jq -r --arg head_sha "$HEAD_SHA" '
      .[0]
      | select(((.headSha // "") | ascii_downcase) == ($head_sha | ascii_downcase))
      | .databaseId // empty
    ' <<<"$json")
    if [[ -z "$run_id" ]]; then
      echo "error: no runs found on branch '$branch' at commit '$HEAD_SHA'" >&2
      exit 1
    fi
    printf '%s\n' "$run_id"
    return
  fi

  jq -r '.[0].databaseId' <<<"$json"
}

format_duration() {
  local total="$1"
  local hours=$((total / 3600))
  local minutes=$(((total % 3600) / 60))
  local seconds=$((total % 60))
  if [[ $hours -gt 0 ]]; then
    printf '%dh%02dm%02ds' "$hours" "$minutes" "$seconds"
  elif [[ $minutes -gt 0 ]]; then
    printf '%dm%02ds' "$minutes" "$seconds"
  else
    printf '%ds' "$seconds"
  fi
}

resolve_repo_for_api() {
  RESOLVED_REPO=""
  if [[ -n "$REPO" ]]; then
    RESOLVED_REPO=$REPO
    return 0
  fi
  if ! run_gh_capture repo view --json nameWithOwner --jq .nameWithOwner; then
    return 1
  fi
  RESOLVED_REPO=$GH_CAPTURED_OUTPUT
  [[ -n "$RESOLVED_REPO" ]]
}

diagnose_waiting_run() {
  local run_url="$1"
  local repo=""
  local pending_json=""
  local pending_count="0"
  local pending_read=false

  echo "Run $RUN_ID requires attention: GitHub reports status 'waiting'." >&2
  [[ -n "$run_url" ]] && echo "  $run_url" >&2

  if ! resolve_repo_for_api; then
    echo "Unable to resolve the repository, so pending deployments could not be inspected." >&2
  else
    repo=$RESOLVED_REPO
  fi
  if [[ -n "$repo" ]] && ! run_gh_capture api "repos/$repo/actions/runs/$RUN_ID/pending_deployments"; then
    echo "Unable to read pending deployments for exact run $RUN_ID in $repo." >&2
  elif [[ -n "$repo" ]]; then
    pending_json=$GH_CAPTURED_OUTPUT
    pending_read=true
  fi
  if [[ "$pending_read" == true ]] && ! jq -e 'type == "array"' >/dev/null 2>&1 <<<"$pending_json"; then
    echo "GitHub returned an unexpected pending-deployments response for exact run $RUN_ID." >&2
  elif [[ "$pending_read" == true ]]; then
    pending_count=$(jq 'length' <<<"$pending_json")
    if ((pending_count > 0)); then
      echo "Pending protected environment deployment(s):" >&2
      jq -r '
        .[]
        | ((.environment.name // "(unnamed environment)") | tostring | gsub("[\\r\\n\\t]"; " ")) as $name
        | .current_user_can_approve as $can_approve
        | "  - \($name): current GitHub CLI identity can approve: \(if $can_approve == true then "yes" elif $can_approve == false then "no" else "unknown" end)"
      ' <<<"$pending_json" >&2
    else
      echo "GitHub reported no pending deployments; the wait may be a concurrency, queue, or custom protection-rule gate." >&2
    fi
  fi

  echo "gh_run_wait does not approve protected environments." >&2
  echo "Recommendation: use the exact-run babysitter for run $RUN_ID with separate automation-dispatch and human-review identities." >&2
}

if [[ -z "$BRANCH" ]]; then
  BRANCH=$(default_branch)
fi

if [[ -z "$RUN_ID" ]]; then
  if [[ -n "$WORKFLOW" ]]; then
    RUN_ID=$(select_latest_run "$WORKFLOW" "$BRANCH")
    AUTO_SELECTED_RUN=true
  else
    RUN_ID=$(select_latest_run_any "$BRANCH")
    AUTO_SELECTED_RUN=true
  fi
fi

if [[ ! "$RUN_ID" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: unable to determine a valid numeric run ID" >&2
  exit 1
fi

echo "Waiting for GitHub Actions run $RUN_ID (timeout: $(format_duration "$TIMEOUT"))..." >&2
if [[ "$AUTO_SELECTED_RUN" == true ]]; then
  if [[ -z "$WORKFLOW" ]]; then
    echo "Auto-selected latest run on branch '$BRANCH'." >&2
  else
    echo "Auto-selected latest '$WORKFLOW' run on branch '$BRANCH'." >&2
  fi
elif [[ -n "$WORKFLOW" ]]; then
  echo "Using workflow '$WORKFLOW' on branch '$BRANCH'." >&2
fi

last_status=""
last_jobs_snapshot=""
last_progress_snapshot=""

while true; do
  check_timeout
  json=""
  if ! run_gh_capture run view "$RUN_ID" --json status,conclusion,displayTitle,workflowName,headBranch,url,startedAt,updatedAt,jobs; then
    echo "$(date '+%Y-%m-%d %H:%M:%S') failed to fetch run info; retrying in $INTERVAL s" >&2
    sleep_until_next_poll
    continue
  fi
  json=$GH_CAPTURED_OUTPUT

  status=$(jq -r '.status' <<<"$json")
  conclusion=$(jq -r '.conclusion // ""' <<<"$json")
  workflow_name=$(jq -r '.workflowName // "(unknown workflow)"' <<<"$json")
  display_title=$(jq -r '.displayTitle // "(no title)"' <<<"$json")
  branch_name=$(jq -r '.headBranch // "(unknown branch)"' <<<"$json")
  run_url=$(jq -r '.url // ""' <<<"$json")
  if [[ -n "$run_url" ]]; then
    last_run_url="$run_url"
  fi

  if [[ "$status" != "$last_status" ]]; then
    echo "$(date '+%Y-%m-%d %H:%M:%S') [$workflow_name] $display_title on branch '$branch_name' -> status: $status${conclusion:+, conclusion: $conclusion}" >&2
    [[ -n "$run_url" ]] && echo "  $run_url" >&2
    last_status="$status"
  fi

  if [[ "$status" == "waiting" ]]; then
    diagnose_waiting_run "$run_url"
    exit 2
  fi

  jobs_snapshot=$(jq -r '.jobs[]? | "\(.name // "(no name)")|\(.status)//\(.conclusion // "")"' <<<"$json" | sort)

  if [[ "$jobs_snapshot" != "$last_jobs_snapshot" ]]; then
    if [[ -n "$jobs_snapshot" ]]; then
      echo "$(date '+%Y-%m-%d %H:%M:%S') job summary:" >&2
      jq -r '.jobs[]? | "  - " + (.name // "(no name)") + ": " + (.status // "?") + (if .status == "completed" and .conclusion != null then " (" + .conclusion + ")" else "" end)' <<<"$json" >&2
    fi
    last_jobs_snapshot="$jobs_snapshot"
  fi

  total_jobs=$(jq -r '.jobs | length' <<<"$json")
  completed_jobs=$(jq -r '[.jobs[]? | select(.status == "completed")] | length' <<<"$json")
  in_progress_jobs=$(jq -r '[.jobs[]? | select(.status == "in_progress")] | length' <<<"$json")
  queued_jobs=$(jq -r '[.jobs[]? | select(.status == "queued")] | length' <<<"$json")
  progress_snapshot="$completed_jobs/$total_jobs/$in_progress_jobs/$queued_jobs"

  if [[ "$status" != "completed" && "$total_jobs" != "0" && "$progress_snapshot" != "$last_progress_snapshot" ]]; then
    echo "$(date '+%Y-%m-%d %H:%M:%S') progress: $completed_jobs/$total_jobs completed ($in_progress_jobs in_progress, $queued_jobs queued)" >&2
    last_progress_snapshot="$progress_snapshot"
  fi

  failing_jobs=$(jq -c '
    .jobs[]? | select(
      .status == "completed" and (.conclusion // "") != "" and
      ((.conclusion | ascii_downcase) as $c | $c != "success" and $c != "skipped" and $c != "neutral")
    )
  ' <<<"$json")

  if [[ -n "$failing_jobs" ]]; then
    echo "$(date '+%Y-%m-%d %H:%M:%S') detected failing job(s) while run status is '$status'; exiting early." >&2
    if [[ "$PRINT_FAILURE_LOGS" == true ]]; then
      if [[ "$status" != "completed" ]]; then
        echo "Run $RUN_ID is still $status; skipping log download for now." >&2
      else
        while IFS= read -r job_json; do
          [[ -z "$job_json" ]] && continue
          job_id=$(jq -r '.databaseId // ""' <<<"$job_json")
          job_name=$(jq -r '.name // "(no name)"' <<<"$job_json")
          job_conclusion=$(jq -r '.conclusion // "unknown"' <<<"$job_json")
          echo "--- Logs for job: $job_name (ID $job_id, conclusion: $job_conclusion) ---" >&2
          if [[ -n "$job_id" ]]; then
            if run_gh_capture run view "$RUN_ID" --log --job "$job_id"; then
              printf '%s\n' "$GH_CAPTURED_OUTPUT"
            else
              echo "(failed to fetch logs for job $job_id)" >&2
            fi
          else
            echo "(job has no databaseId; skipping log fetch)" >&2
          fi
          echo "--- End logs for job: $job_name ---" >&2
        done <<<"$failing_jobs"
      fi
    fi
    exit 1
  fi

  if [[ "$status" == "completed" ]]; then
    started_at=$(jq -r '.startedAt // ""' <<<"$json")
    updated_at=$(jq -r '.updatedAt // ""' <<<"$json")
    duration=""
    if [[ -n "$started_at" && -n "$updated_at" ]]; then
      start_epoch=$(date -d "$started_at" +%s 2>/dev/null || true)
      end_epoch=$(date -d "$updated_at" +%s 2>/dev/null || true)
      if [[ -n "$start_epoch" && -n "$end_epoch" && "$end_epoch" -ge "$start_epoch" ]]; then
        duration=$(format_duration $((end_epoch - start_epoch)))
      fi
    fi
    if [[ "$conclusion" == "success" ]]; then
      if [[ -n "$duration" ]]; then
        echo "Run $RUN_ID succeeded in $duration." >&2
      else
        echo "Run $RUN_ID succeeded." >&2
      fi
      exit 0
    else
      if [[ "$PRINT_FAILURE_LOGS" == true ]]; then
        echo "Collecting logs for failed jobs..." >&2
        jq -r '.jobs[]? | select((.conclusion // "") != "success") | "\(.databaseId)\t\(.name // "(no name)")"' <<<"$json" \
          | while IFS=$'\t' read -r job_id job_name; do
              [[ -z "$job_id" ]] && continue
              echo "--- Logs for job: $job_name (ID $job_id) ---" >&2
              if run_gh_capture run view "$RUN_ID" --log --job "$job_id"; then
                printf '%s\n' "$GH_CAPTURED_OUTPUT"
              else
                echo "(failed to fetch logs for job $job_id)" >&2
              fi
              echo "--- End logs for job: $job_name ---" >&2
            done
      fi
      if [[ -n "$duration" ]]; then
        echo "Run $RUN_ID finished with conclusion '$conclusion' in $duration." >&2
      else
        echo "Run $RUN_ID finished with conclusion '$conclusion'." >&2
      fi
      exit 1
    fi
  fi

  sleep_until_next_poll
done
