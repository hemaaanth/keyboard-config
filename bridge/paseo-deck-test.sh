#!/usr/bin/env bash
# Read-only smoke test against the live paseo daemon. Never calls
# permit allow/deny or agent send.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DECK="$SCRIPT_DIR/paseo-deck.sh"
PASEO=/home/system/.local/share/mise/shims/paseo
JQ=/home/system/.local/share/mise/shims/jq
[ -x "$JQ" ] || JQ=$(command -v jq || echo /home/system/.local/share/mise/installs/jq/1.8.2/jq)

fail=0
pass() { echo "PASS: $1"; }
failed() { echo "FAIL: $1"; fail=1; }

# 1. workspace ls --json parses and has >=1 entry.
ws_count=$("$PASEO" workspace ls --json 2>/dev/null | "$JQ" 'length' 2>/dev/null)
if [ -n "${ws_count:-}" ] && [ "$ws_count" -ge 1 ] 2>/dev/null; then
    pass "workspace ls --json parses with >=1 entry ($ws_count)"
else
    failed "workspace ls --json parses with >=1 entry"
fi

# 2. resolve 1 prints an agent id, or fails with a clean one-line error.
resolve_out=$("$DECK" resolve 1 2>&1)
resolve_rc=$?
if [ $resolve_rc -eq 0 ] && [ -n "$resolve_out" ]; then
    pass "resolve 1 -> $resolve_out"
elif [ $resolve_rc -ne 0 ] && [ -n "$resolve_out" ] && [ "$(wc -l <<<"$resolve_out")" -eq 1 ]; then
    pass "resolve 1 failed cleanly (no agent) -> $resolve_out"
else
    failed "resolve 1 plumbing (rc=$resolve_rc out=$resolve_out)"
fi

# 3. permit ls --json parses (empty list is fine).
permit_type=$("$PASEO" permit ls --json 2>/dev/null | "$JQ" -r 'type' 2>/dev/null)
if [ "$permit_type" = "array" ]; then
    pass "permit ls --json parses (array)"
else
    failed "permit ls --json parses"
fi

if [ "$fail" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
else
    echo "SOME FAILED"
    exit 1
fi
