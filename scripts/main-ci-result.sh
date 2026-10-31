#!/usr/bin/env bash
# Report what main's CI concluded about one commit, so the Release workflow can reuse that
# result instead of running the same checks a second time on the tag. Run by the `verify`
# job in .github/workflows/release.yml; needs `gh` and a token that can read Actions.
#
#   GH_TOKEN=... GITHUB_REPOSITORY=owner/repo ./scripts/main-ci-result.sh <commit-sha>
#
# It looks for runs of ci.yml triggered by a push to main for exactly that commit, and
# prints one of two answers (also written to $GITHUB_OUTPUT as `checks=`, when set):
#
#   reuse  A run succeeded. A run that failed and then passed on a re-run counts, because
#          the API reports a run's latest attempt.
#   run    There is no result to reuse: no run exists, or every run was cancelled. ci.yml
#          cancels a superseded run on main when two merges land close together, and a
#          merge GitHub makes on Dependabot's behalf triggers no workflow at all, so both
#          are ordinary. The release then runs checks.yml itself.
#
# It exits non-zero when a run concluded anything else (failure, timed_out, ...) or is still
# going when the wait runs out: a verdict against the commit, or no verdict yet, both of
# which should stop a release rather than send it down the fallback.
#
# A tag is often pushed seconds after its merge, so it waits: up to MAIN_CI_APPEAR_SECS
# (default 120) for a run to be created, and up to MAIN_CI_WAIT_SECS (default 1800) for a
# created run to finish. Main's CI took 4 to 8 minutes in October 2026.
set -euo pipefail

fail() { echo "::error::$*" >&2; exit 1; }

sha=${1:-}
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || fail "usage: $0 <full 40-character commit sha>"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must be owner/repo}"

appear_secs=${MAIN_CI_APPEAR_SECS:-120}
wait_secs=${MAIN_CI_WAIT_SECS:-1800}
poll_secs=${MAIN_CI_POLL_SECS:-20}

answer() {
  echo "$1"
  if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "checks=$1" >> "$GITHUB_OUTPUT"; fi
  exit 0
}

start=$SECONDS
while :; do
  elapsed=$((SECONDS - start))
  # One line per run: id, status, conclusion (empty while running), attempt, url.
  if ! runs=$(gh api "repos/${GITHUB_REPOSITORY}/actions/workflows/ci.yml/runs?head_sha=${sha}&event=push&branch=main&per_page=100" \
    --jq '.workflow_runs[] | [.id, .status, (.conclusion // ""), .run_attempt, .html_url] | @tsv'); then
    [ "$elapsed" -lt "$wait_secs" ] || fail "could not list main's CI runs for ${sha}"
    echo "listing main's CI runs failed; retrying in ${poll_secs}s" >&2
    sleep "$poll_secs"
    continue
  fi

  if [ -n "$runs" ]; then
    echo "main's CI runs for ${sha} after ${elapsed}s (id status conclusion attempt url):" >&2
    awk -F'\t' '{ print "  " $1, $2, $3, $4, $5 }' <<< "$runs" >&2
  fi

  if grep -q $'\tcompleted\tsuccess\t' <<< "$runs"; then
    answer reuse
  fi

  if [ -z "$runs" ]; then
    if [ "$elapsed" -ge "$appear_secs" ]; then
      echo "no run of ci.yml on main for ${sha} after ${elapsed}s" >&2
      answer run
    fi
  elif grep -qv $'\tcompleted\t' <<< "$runs"; then
    [ "$elapsed" -lt "$wait_secs" ] \
      || fail "main's CI for ${sha} has not finished after ${elapsed}s; re-run this job once it has"
  else
    # Every run completed and none succeeded.
    if grep -qv $'\tcompleted\tcancelled\t' <<< "$runs"; then
      fail "main's CI did not pass for ${sha}; fix main or re-run that run, then re-run this release"
    fi
    echo "every run of ci.yml on main for ${sha} was cancelled" >&2
    answer run
  fi

  sleep "$poll_secs"
done
