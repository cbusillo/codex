#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." >/dev/null 2>&1 && pwd)"

cd "$REPO_ROOT"

if ! command -v code >/dev/null 2>&1; then
	echo "the 'code' command is required; run 'just local-code-rebuild' first" >&2
	exit 1
fi

if ! git diff --quiet -- VERSION CHANGELOG.md docs/release-notes/RELEASE_NOTES.md ||
	! git diff --cached --quiet -- VERSION CHANGELOG.md docs/release-notes/RELEASE_NOTES.md; then
	echo "release metadata files have uncommitted changes; commit or stash them first" >&2
	exit 1
fi

current_version=$(tr -d '[:space:]' <VERSION)
latest_tag=$(git tag --list 'v*' --sort=-v:refname | head -n 1 | sed 's/^v//' || true)
latest_tag="${latest_tag:-0.0.0}"
new_version=$(printf '%s\n%s\n' "$current_version" "$latest_tag" | sort -V | tail -n1)

if git rev-parse "v${new_version}" >/dev/null 2>&1; then
	IFS='.' read -r major minor patch <<<"$new_version"
	new_version="${major}.${minor}.$((patch + 1))"
fi

while git rev-parse "v${new_version}" >/dev/null 2>&1; do
	IFS='.' read -r major minor patch <<<"$new_version"
	new_version="${major}.${minor}.$((patch + 1))"
done

prev_tag="v${latest_tag}"
if ! git rev-parse "$prev_tag" >/dev/null 2>&1; then
	base=$(git rev-list --max-parents=0 HEAD | tail -n1)
	range="$base..HEAD"
	prev_tag="none"
else
	range="$prev_tag..HEAD"
fi

date_utc=$(date -u +%Y-%m-%d)
mkdir -p docs/release-notes .code/release-notes
context_file=".code/release-notes/context-v${new_version}.md"
prompt_file=".code/release-notes/prompt-v${new_version}.md"

{
	echo "# Commit log (${range})"
	git log --no-color --format='* %h %s (%an)' --abbrev=8 --no-merges "$range"
} >"$context_file"

cat >"$prompt_file" <<PROMPT
You are the Every Code agent preparing local release metadata.

Inputs
- Version: v${new_version}
- Date (UTC): ${date_utc}
- Previous tag: ${prev_tag}
- Commit range: ${range}
- Working directory: ${REPO_ROOT}

Primary Tasks
1. Update VERSION to ${new_version}.
2. Update CHANGELOG.md with a new entry for this version using the exact house style below.
3. Generate GitHub release notes at docs/release-notes/RELEASE_NOTES.md.

CHANGELOG.md House Style
- File header stays as-is. Do not rewrite older sections.
- Insert the new section at the top, above older released versions and below Unreleased, with this header exactly:
  ## [${new_version}] - ${date_utc}
- Synthesize the notable user-visible features, important fixes, UX improvements, performance/stability work, and release-operator changes.
- Ignore internal chores, merge commits, and minor refactors unless they directly affect users, installation, or release operators.
- Use judgment on length: keep the changelog compact and scannable, do not pad thin releases, and do not omit important distinct changes from busy releases.
- Keep each bullet concise, single-line, and present tense.
- Start bullets with a short scope label when helpful, such as "TUI:", "CLI:", "Core:", "Release:", or "Docs:".
- End each bullet with abbreviated commit SHA(s) in parentheses, using 7-8 hex chars, comma-separated when multiple.
- Map changes from the git log in ${range}; ignore pure chores/merges unless they affect users or release operators.
- Do not add links, tables, code blocks, subheadings, or PR author attributions in the changelog.
- Idempotent: if a section for ${new_version} already exists, replace only that section body and keep the header intact.

Release Notes
- Write exactly these sections in order:
  1. Title: ## Every Code v${new_version}
  2. One brief intro sentence.
  3. Section header: ### Changes
     - Curate the most interesting release highlights for readers of a GitHub release page.
     - Prioritize notable features, important fixes, UX improvements, performance/stability wins, and release-operator changes.
     - Right-size the list to the release: one or two bullets is fine for tiny releases, a normal release should usually fit in a handful, and a larger release can use more when each item is distinct and useful.
     - Do not pad with chores or exhaustively restate the commit log.
     - Rewrite for readability and impact; bullets do not need to match the changelog verbatim.
     - Omit SHAs, links, tables, PR numbers, and author attributions.
  4. Section header: ### Install
     Code block with exactly:
     gh release download v${new_version} --repo cbusillo/code
- Optional final line only when a previous tag exists:
  Compare: https://github.com/cbusillo/code/compare/${prev_tag}...v${new_version}
- Do not include "See CHANGELOG.md for details.".
- Keep notes concise. Do not add sections other than those listed above.

Rules
- Use the provided git log as source of truth and inspect the repo when commit messages are unclear.
- Basic markdown only. No emojis.
- Do not commit. Leave the working tree changes for review.
- After editing, run scripts/check-release-notes-version.sh --version ${new_version} and fix any validation failures.

Context follows:
PROMPT
cat "$context_file" >>"$prompt_file"

echo "Preparing local release metadata for v${new_version} from ${range}"
echo "Prompt: $prompt_file"
code exec --cd "$REPO_ROOT" --sandbox workspace-write --skip-git-repo-check <"$prompt_file"
scripts/check-release-notes-version.sh --version "$new_version"

echo "Local release metadata is ready for review:"
echo "  VERSION"
echo "  CHANGELOG.md"
echo "  docs/release-notes/RELEASE_NOTES.md"
