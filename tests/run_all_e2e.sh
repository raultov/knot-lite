#!/bin/bash
# Master test runner for all knot-lite E2E integration test suites.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SCRIPT_DIR/e2e/lib.sh"

echo "=== Running knot-lite E2E Integration Test Suites ==="

# Build release binaries first
cargo build --release --quiet

SUITES_PASSED=0
SUITES_FAILED=0

for suite in "$SCRIPT_DIR"/e2e/test_*.sh; do
  if [ -x "$suite" ]; then
    suite_name="$(basename "$suite")"
    echo -n "Running $suite_name ... "
    if "$suite" > "$E2E_CONFIG_DIR/$suite_name.log" 2>&1; then
      echo "PASS"
      SUITES_PASSED=$((SUITES_PASSED + 1))
    else
      echo "FAIL (log: $E2E_CONFIG_DIR/$suite_name.log)"
      SUITES_FAILED=$((SUITES_FAILED + 1))
    fi
  fi
done

echo "=== E2E Summary: $SUITES_PASSED passed, $SUITES_FAILED failed ==="
if [ "$SUITES_FAILED" -gt 0 ]; then
  exit 1
fi
