#!/usr/bin/env bash
# Runs the E2E suites one after the other: rust, typescript, javascript, java.
# Usage: ./tests/run_all_e2e.sh      (or: make e2e)
#
# Real language servers are started: Linux and macOS, ~10-20 minutes the first time. See README > End-to-end tests.
# Exit status 0 only when every suite is green and no process was left behind.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/e2e/lib.sh"

command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
if [ "${KNOT_LITE_E2E_SKIP_BUILD:-0}" != "1" ]; then
  (cd "$E2E_REPO_ROOT" && cargo build --release --bins 2>&1 | grep -E '^(error|warning: unused)|Finished' || true)
fi
# The suites must not build again.
export KNOT_LITE_E2E_SKIP_BUILD=1
[ -x "$KL" ] && [ -x "$KLM" ] || { echo "release binaries missing: $KL $KLM" >&2; exit 1; }

E2E_RUN="$E2E_HOME/run/_all"; E2E_WORK="$E2E_HOME/work"
rm -rf "$E2E_RUN"; mkdir -p "$E2E_RUN" "$E2E_WORK"
e2e_baseline

echo "== knot-lite E2E: all suites ($("$KL" --version)) =="
T0=$(date +%s)
SUITES="rust typescript javascript java"
RESULTS=""; TOTAL_PASS=0; TOTAL_FAIL=0; TOTAL_SKIP=0; FAILED_SUITES=""
for suite in $SUITES; do
  # The suite's own exit status: `|| true` after the pipeline would overwrite PIPESTATUS.
  set +e
  "$E2E_REPO_ROOT/tests/run_${suite}_e2e.sh" 2>&1 | tee "$E2E_RUN/$suite.log"
  rc=${PIPESTATUS[0]}
  set -e
  summary="$(grep -E "^-- $suite: " "$E2E_RUN/$suite.log" | tail -1 || true)"
  p="$(printf '%s' "$summary" | sed -n 's/.*: \([0-9]*\) passed.*/\1/p')"
  f="$(printf '%s' "$summary" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
  k="$(printf '%s' "$summary" | sed -n 's/.* \([0-9]*\) skipped.*/\1/p')"
  TOTAL_PASS=$((TOTAL_PASS + ${p:-0})); TOTAL_FAIL=$((TOTAL_FAIL + ${f:-0})); TOTAL_SKIP=$((TOTAL_SKIP + ${k:-0}))
  # Green only when the suite exited 0, printed its summary and counted no failure.
  if [ "$rc" -eq 0 ] && [ -n "$summary" ] && [ "${f:-1}" -eq 0 ]; then RESULTS="$RESULTS$(printf 'PASS  %s\n' "${summary#-- }")
"; else RESULTS="$RESULTS$(printf 'FAIL  %s (exit %s)\n' "${summary:+${summary#-- }}${summary:-$suite: no summary line}" "$rc")
"; FAILED_SUITES="$FAILED_SUITES $suite"; fi
done

# Nothing may be left running after the suites: no server under the work directories, no process carrying an
# ownership mark, no Gradle daemon that was not there before.
ORPHANS_OK=1
left="$(server_procs_in "$E2E_WORK")$(owned_procs)$(gradle_daemons | comm -13 "$E2E_RUN/gradle-baseline.txt" - 2>/dev/null)"
if [ -n "$left" ]; then ORPHANS_OK=0; fi

echo
echo "== summary =="
printf '%s' "$RESULTS"
if [ "$ORPHANS_OK" = 1 ]; then echo "PASS  no process left behind"; else echo "FAIL  processes left behind:"; printf '      %s\n' $left; fi
printf -- '-- all: %d passed, %d failed, %d skipped (%d s)\n' "$TOTAL_PASS" "$TOTAL_FAIL" "$TOTAL_SKIP" $(( $(date +%s) - T0 ))
[ -z "$FAILED_SUITES" ] && [ "$ORPHANS_OK" = 1 ]
