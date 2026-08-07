#!/usr/bin/env bash
# 30-pane-reuse.sh — a non-fresh auto chain reuses ONE managed agent pane (and
# one conversation) across stages; a fresh column still mints a new pane, and a
# manual landing keeps the pane open for review.
#
# Setup(fresh,auto) → Implement(auto) → Review(auto) → Manual(manual). The
# looping fake Pi completes each stage on the SAME pane/process: the fresh Setup
# run mints the conversation + pane, and each non-fresh resume hop re-prompts
# that pane (no new pane.split, no new agent.start) instead of opening another.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

trap e2e_cleanup EXIT
e2e_enable_fake_pi
# A same-conversation resume hop re-prompts the SAME process on its pane, so the
# fake Pi loops: read one stage's prompt, comment, done, then the next prompt.
export FAKE_PI_LOOP=1
e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)

e2e_ws_create reuse; WS_ID="$E2E_WS"

step "Create a Setup(fresh,auto)→Implement(auto)→Review(auto)→Manual(manual) chain"
MANUAL_ID="$(col_create '{"name":"Manual","trigger":"manual"}')"
REVIEW_ID="$(col_create "{\"name\":\"Review\",\"trigger\":\"auto\",\"on_success_column_id\":$MANUAL_ID,\"fresh_session\":false}")"
IMPL_ID="$(col_create "{\"name\":\"Implement\",\"trigger\":\"auto\",\"on_success_column_id\":$REVIEW_ID,\"fresh_session\":false}")"
SETUP_ID="$(col_create "{\"name\":\"Setup\",\"trigger\":\"auto\",\"on_success_column_id\":$IMPL_ID,\"fresh_session\":true}")"
[ -n "$SETUP_ID" ] && [ -n "$IMPL_ID" ] && [ -n "$REVIEW_ID" ] && [ -n "$MANUAL_ID" ] \
  || fail "could not create the chain columns"

step "Create a managed Pi card and start it in the fresh Setup column"
card_json="$("$BOARD_BIN" card new --title 'Reuse Card' -d 'traverse the chain' \
  --harness pi --model reuse/pi-model --effort low \
  --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID Setup -> managed Pi starts; reuse chain begins"
e2e_board_herdr_mutate -- move "$CARD_ID" Setup --json >/dev/null

pane_reuse_failure_diag() {
  local panes
  printf '\n--- pane reuse failure diagnostics (disposable session only) ---\n' >&2
  e2e_card_failure_diag "$CARD_ID"
  "$BOARD_BIN" card show "$CARD_ID" --json 2>/dev/null | python3 -c '
import json, sys
try: d=json.load(sys.stdin)
except Exception: print("runs: <unavailable>", file=sys.stderr); raise SystemExit(0)
print("card: status=%s column_id=%s" % (d.get("card",{}).get("status"), d.get("card",{}).get("column_id")), file=sys.stderr)
for r in d.get("runs", []):
    safe = {k:v for k,v in r.items() if "prompt" not in k and k not in ("argv", "env")}
    print("run: %s" % safe, file=sys.stderr)
for c in d.get("comments", []):
    print("comment: id=%s author=%s body_len=%s" %
          (c.get("id"), c.get("author"), len(c.get("body") or "")), file=sys.stderr)
' || true
  panes="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}" 2>/dev/null || true)"
  printf '%s\n' "pane.list: ${panes:-<unavailable>}" >&2
  printf 'fixture evidence:\n' >&2
  find "$E2E_TMP" -maxdepth 2 -type f \( -name 'fake-pi-run-*.json' -o -name 'fake-pi-loop-*.log' \) -print 2>/dev/null >&2 || true
  for trace in "$E2E_TMP"/fake-pi-loop-*.log; do
    [ -f "$trace" ] || continue
    printf '%s\n' "trace $trace:" >&2
    cat "$trace" >&2
  done
  printf '%s\n' 'daemon.log tail:' >&2
  tail -80 "$E2E_TMP/daemon.log" 2>/dev/null >&2 || true
  printf '%s\n' '--- end pane reuse diagnostics ---' >&2
}

step "Wait for the card to traverse Setup→Implement→Review (three runs)"
last="$(wait_runs "$CARD_ID" 3 90)" || {
  pane_reuse_failure_diag
  fail "card did not produce 3 runs (last='$last')"
}
[ "$last" = ok ] || {
  pane_reuse_failure_diag
  fail "last run did not complete ok (got '$last')"
}

step "Assert all three runs share ONE agent pane and ONE conversation"
"$BOARD_BIN" card show "$CARD_ID" --json >"$E2E_TMP/card.json"
tabs_json="$(hrpc tab.list "{\"workspace_id\":\"$WS_ID\"}")"
panes_json="$(hrpc pane.list "{\"workspace_id\":\"$WS_ID\"}")"
python3 - "$E2E_TMP/card.json" "$tabs_json" "$panes_json" "$CARD_ID" <<'PY' || fail "reuse assertions failed"
import json, sys
card_path, tabs_json, panes_json, card_id = sys.argv[1:]
card = json.load(open(card_path, encoding="utf-8"))
runs = card["runs"]
assert len(runs) == 3, f"expected 3 runs, got {len(runs)}"
assert all(r.get("outcome") == "ok" for r in runs), "every stage must finish ok"
panes = json.loads(panes_json).get("panes", [])
tabs = json.loads(tabs_json).get("tabs", [])
# The fresh Setup run mints the conversation + pane; every non-fresh resume hop
# reuses that same pane + conversation (no second pane/process beside it).
pane_ids = {r.get("herdr_pane_id") for r in runs}
session_ids = {r.get("session_id") for r in runs}
assert len(pane_ids) == 1, f"runs span {len(pane_ids)} panes: {pane_ids}"
assert len(session_ids) == 1, f"runs span {len(session_ids)} sessions: {session_ids}"
shared_pane = next(iter(pane_ids))
assert shared_pane, "runs have no herdr_pane_id"
# Exactly ONE managed agent pane (the shared one) lives in the card's tab.
card_tabs = [t for t in tabs if t.get("label") == f"card-{card_id}"]
assert len(card_tabs) == 1, f"expected one card tab, got {len(card_tabs)}"
owned = [p for p in panes if p.get("tab_id") == card_tabs[0]["tab_id"]]
agents = [p for p in owned if p.get("agent") == "pi"]
assert len(agents) == 1, f"expected exactly one Pi agent pane, got {len(agents)} ({[p.get('pane_id') for p in agents]})"
assert agents[0]["pane_id"] == shared_pane, "the agent pane is not the shared run pane"
print(f"[ok] 3 runs reuse one pane {shared_pane} and one conversation", file=sys.stderr)
PY

step "Assert the card landed idle in the manual column with its pane still open"
python3 - "$E2E_TMP/card.json" <<'PY' || fail "manual-landing assertion failed"
import json, sys
c = json.load(open(sys.argv[1], encoding="utf-8"))["card"]
assert c["status"] == "idle", f"expected idle in the manual column, got {c['status']}"
print(f"[ok] card idle in column {c['column_id']} (manual landing, no auto run)", file=sys.stderr)
PY
python3 - "$panes_json" "$E2E_TMP/card.json" <<'PY' || fail "pane-preserved assertion failed"
import json, sys
panes = json.loads(sys.argv[1]).get("panes", [])
runs = json.load(open(sys.argv[2], encoding="utf-8"))["runs"]
shared_pane = runs[-1]["herdr_pane_id"]
agents = [p for p in panes if p.get("agent") == "pi" and p.get("pane_id") == shared_pane]
assert len(agents) == 1, f"expected shared agent pane {shared_pane} still open after manual landing"
print(f"[ok] agent pane {shared_pane} still open for human review", file=sys.stderr)
PY

ok "non-fresh chain reused one pane/conversation; fresh column minted; manual landing kept the pane open"
step "30-pane-reuse: PANE REUSE ACROSS RESUME HOPS PASSED"
