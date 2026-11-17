#!/usr/bin/env bash
# Wait for a GitHub Actions workflow run on ONE commit to finish, then print what it saw.
#
#   deploy/lnp/wait-ci.sh <full-sha> [workflow-file] [timeout-minutes]
#
# Exit 0: the run completed with conclusion "success".
# Exit 1: it completed with anything else (failed jobs are listed).
# Exit 2: no run appeared, or it did not finish before the timeout.
# Run it in the background right after a push or a tag; its exit is the notification.
set -u
SHA="${1:?usage: wait-ci.sh <full-sha> [workflow-file] [timeout-minutes]}"
WORKFLOW="${2:-ci.yml}"
TIMEOUT_MIN="${3:-60}"
REPO="${REPO:-Leadership-Now-Project/opn.onl}"
deadline=$(( $(date +%s) + TIMEOUT_MIN * 60 ))
state="no run found"

while [ "$(date +%s)" -lt "$deadline" ]; do
  # Newest run of this workflow for exactly this commit.
  line=$(gh run list --repo "$REPO" --workflow "$WORKFLOW" --commit "$SHA" --limit 1 \
           --json databaseId,status,conclusion,url \
           --jq '.[0] | select(. != null) | "\(.databaseId) \(.status) \(.conclusion) \(.url)"' 2>/dev/null)
  if [ -n "$line" ]; then
    read -r id status conclusion url <<<"$line"
    state="$status/$conclusion"
    if [ "$status" = "completed" ]; then
      echo "WAIT-CI RESULT: workflow=$WORKFLOW sha=$SHA conclusion=$conclusion"
      echo "$url"
      gh run view "$id" --repo "$REPO" --json jobs \
        --jq '.jobs[] | "\(.conclusion // .status)\t\(.name)"' 2>/dev/null
      [ "$conclusion" = "success" ] && exit 0
      exit 1
    fi
  fi
  sleep 30
done

echo "WAIT-CI TIMEOUT after ${TIMEOUT_MIN} min: workflow=$WORKFLOW sha=$SHA last state: $state"
[ -n "${id:-}" ] && gh run view "$id" --repo "$REPO" --json jobs \
  --jq '.jobs[] | "\(.conclusion // .status)\t\(.name)"' 2>/dev/null
exit 2
