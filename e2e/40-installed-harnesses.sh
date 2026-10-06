#!/usr/bin/env bash
# 40-installed-harnesses.sh — harness.list and the new-card default follow
# Herdr's installed integrations, and an explicit create never discovers.
#
# The owned proxy in front of the ephemeral Herdr session injects a
# deterministic `integration.list` response, so the scenario does not depend
# on which integrations the host has installed. Everything else is forwarded
# to the real disposable session: no provider, no dispatch, no Herdr mutation.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

e2e_init
e2e_build
e2e_isolate
# A config section under a builtin name (`[harness.claude]`) is unreachable —
# the builtin adapter is matched first — so it must never re-add claude to the
# filtered list. `fake` is a genuinely config-defined harness and always stays.
cat >>"$HERDR_BOARD_CONFIG" <<'EOF'

[harness.claude]
argv = ["bash", "/bin/false"]
EOF

REAL_SOCKET="$E2E_SESSION_SOCKET"
e2e_proxy_start "$E2E_TMP/herdr-proxy.sock" "$E2E_TMP/proxy-control.sock" "$REAL_SOCKET"
export HERDR_SOCKET_PATH="$E2E_PROXY_SOCKET"
e2e_daemon_start

# assert_harness_list <comma-separated expected list> — one board RPC, exact order.
assert_harness_list() {
  local expected="$1" actual
  actual="$(brpc harness.list '{}' | python3 -c 'import json,sys; print(",".join(json.load(sys.stdin)["harnesses"]))')"
  [ "$actual" = "$expected" ] \
    || fail "harness.list was [$actual], expected [$expected]"
}

# assert_card_harness <expected> [args ...] — create a card and print its harness.
assert_card_harness() {
  local expected="$1" title="$2"
  shift 2
  local card
  card="$("$BOARD_BIN" card new --title "$title" "$@" --json)"
  local actual
  actual="$(printf '%s' "$card" | jget harness)"
  [ "$actual" = "$expected" ] \
    || fail "card '$title' harness was '$actual', expected '$expected'"
  printf '%s\n' "$actual"
}

step "Only the integration-reported builtins appear, mapped to board names"
e2e_proxy_command integration_list_available pi codex >/dev/null
# claude is absent AND carries a colliding config section: neither may appear.
assert_harness_list "pi,codex,fake"
ok "harness.list filtered to installed builtins + config harness (collision skipped)"

step "The antigravity_cli Herdr target maps to the board's antigravity harness"
e2e_proxy_command integration_list_available antigravity_cli >/dev/null
assert_harness_list "antigravity,fake"
ok "antigravity_cli target surfaced as antigravity"

step "An omitted create waits for discovery and uses the filtered default"
e2e_proxy_command integration_list_available codex >/dev/null
assert_harness_list "codex,fake"
assert_card_harness codex "Filtered default" >/dev/null
ok "card.create without a harness defaulted to the only installed builtin"

step "An explicit create uses its harness and never calls integration.list"
BEFORE="$(e2e_proxy_command status)"
CALLS_BEFORE="$(printf '%s' "$BEFORE" | jget integration_list_calls)"
assert_card_harness fake "Explicit config" --harness fake >/dev/null
assert_card_harness pi "Explicit installed builtin" --harness pi >/dev/null
AFTER="$(e2e_proxy_command status)"
CALLS_AFTER="$(printf '%s' "$AFTER" | jget integration_list_calls)"
[ "$CALLS_BEFORE" = "$CALLS_AFTER" ] \
  || fail "explicit harness create triggered discovery ($CALLS_BEFORE -> $CALLS_AFTER)"
ok "explicit --harness creates made no Herdr discovery call"

step "A failing integration.list degrades to the full builtin list"
e2e_proxy_command integration_list_error >/dev/null
assert_harness_list "pi,claude,codex,opencode,antigravity,fake"
ok "Herdr discovery failure kept harness.list served (full fallback)"

echo "40-installed-harnesses: ALL CHECKS PASSED"
