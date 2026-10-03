#!/usr/bin/env bash
# Apply the branch protection in branch-protection.json to main.
#
#   ./scripts/apply-branch-protection.sh [owner/repo]
#
# The payload lives in a file rather than inline arguments because it is
# reviewable in a diff and re-appliable after someone edits it in the GitHub UI.
# There is no "sync from GitHub" direction: if protection is changed in the
# settings, that change has to be made here too or the next run reverts it.
set -euo pipefail

REPO="${1:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"
BRANCH=main
PAYLOAD="$(dirname "$0")/../.github/branch-protection.json"

if [ ! -f "${PAYLOAD}" ]; then
  echo "missing payload: ${PAYLOAD}" >&2
  exit 1
fi

gh api \
  --method PUT \
  "repos/${REPO}/branches/${BRANCH}/protection" \
  --input "${PAYLOAD}" \
  --silent

echo "applied ${PAYLOAD} to ${REPO}:${BRANCH}"
