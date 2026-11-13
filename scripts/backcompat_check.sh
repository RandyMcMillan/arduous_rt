#!/usr/bin/env bash
# Backwards-compatibility check: run the governance consumer smoke test
# against published versions listed in scripts/backcompat_versions.txt.
#
#   plain VERSION   must pass — a failure is a regression against a release
#   VERSION!        pinned expected-failure (a known bug fixed later)
#
# Run before publishing: if a plain version regresses, the pending
# release changed behavior downstream users depend on.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSIONS_FILE="$ROOT/scripts/backcompat_versions.txt"
LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

failures=0
while read -r raw; do
    line="${raw%%#*}"
    line="$(echo "$line" | tr -d '[:space:]')"
    [ -z "$line" ] && continue

    expect_fail=0
    version="$line"
    if [[ "$line" == *'!' ]]; then
        expect_fail=1
        version="${line%'!'}"
    fi

    expectation="must pass"
    [ "$expect_fail" -eq 1 ] && expectation="pinned failure"
    echo "== arduous_rt $version ($expectation) =="

    if "$ROOT/scripts/governance_consumer_smoke.sh" "$version" >"$LOG" 2>&1; then
        status=0
    else
        status=1
    fi

    if [ "$expect_fail" -eq 1 ]; then
        if [ "$status" -eq 0 ]; then
            echo "   UNEXPECTED PASS — the pinned bug is fixed; move $version to a plain entry"
            failures=1
        else
            echo "   pinned failure as expected"
        fi
    else
        if [ "$status" -ne 0 ]; then
            echo "   REGRESSION against published $version:"
            tail -n 5 "$LOG" | sed 's/^/   | /'
            failures=1
        else
            echo "   ok"
        fi
    fi
done <"$VERSIONS_FILE"

if [ "$failures" -ne 0 ]; then
    echo "backcompat check FAILED"
    exit 1
fi
echo "backcompat check passed"
