#!/usr/bin/env bash
# 09-comment-context.sh — a comment from one auto stage reaches the NEXT
# stage's agent process.
#
# Two chained auto columns: Stage1 (on_success -> Stage2) and Stage2. The fake
# agent posts a distinctive marker comment and reports ok in EACH stage. When
# the card auto-advances, the daemon rebuilds the prompt from the card's
# comments — and this scenario proves the second PROCESS actually received
# that comment-bearing prompt: a witness shim (E2E_FAKE_AGENT override, real
# fake-agent underneath) posts a `STAGE2-WITNESSED:<marker>` comment iff its
# own BOARD_PROMPT contains the Stage1 marker. Only Stage2's process can
# observe it, so the witness comment is proof of delivery, not just storage.
#
# The storage half — comments are baked into the persisted `prompt_snapshot`
# under `## Card comments` — is pinned hermetically in
# `crates/board-daemon/src/dispatch/tests/enqueue.rs`
# (`enqueue_run_persists_comment_context_in_prompt_snapshot`) and
# `crates/board-core/tests/prompt.rs`.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)/lib.sh"

MARKER="E2E-CTX-MARKER-$$"

# Witness shim: runs in place of fake-agent.sh in the real pane. Iff this
# process's BOARD_PROMPT carries the Stage1 marker, it says so through the
# real CLI before delegating to the real fake agent (same sleep/comment/done
# behavior). Stage1's prompt predates the marker, so only Stage2 can witness.
WIT_DIR="$(mktemp -d /tmp/hb-e2e-wit.XXXXXX)"
cat >"$WIT_DIR/witness-agent.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: "${BOARD_PROMPT:=}"
: "${BOARD_CARD_ID:?BOARD_CARD_ID required}"
: "${BOARD_SOCKET:?BOARD_SOCKET required}"
: "${BOARD_BIN:?BOARD_BIN required}"
: "${REAL_FAKE_AGENT:?REAL_FAKE_AGENT required}"
if [ -n "${WITNESS_MARKER:-}" ] && [[ "$BOARD_PROMPT" == *"$WITNESS_MARKER"* ]]; then
  "$BOARD_BIN" comment "STAGE2-WITNESSED:${WITNESS_MARKER}" >/dev/null 2>&1 || true
fi
exec bash "$REAL_FAKE_AGENT" "$@"
EOF
chmod +x "$WIT_DIR/witness-agent.sh"
export E2E_FAKE_AGENT="$WIT_DIR/witness-agent.sh"
export E2E_FAKE_ENV="FAKE_AGENT_COMMENT=${MARKER} WITNESS_MARKER=${MARKER} REAL_FAKE_AGENT=$E2E_LIB_DIR/fake-agent.sh"

e2e_boot   # e2e_init + e2e_build + e2e_isolate + e2e_daemon_start (in that order)
e2e_defer "rm -rf $WIT_DIR"

e2e_ws_standard board-e2e   # step + e2e_ws_create + WS_ID + echo

step "Create two chained auto columns: Stage1 (on_success -> Stage2) and Stage2"
STAGE2_ID="$(col_create '{"name":"Stage2","trigger":"auto"}')"          # create target first
STAGE1_ID="$(col_create "{\"name\":\"Stage1\",\"trigger\":\"auto\",\"on_success_column_id\":$STAGE2_ID}")"
[ -n "$STAGE1_ID" ] && [ -n "$STAGE2_ID" ] || fail "could not create the two stage columns"
echo "  Stage1 id=$STAGE1_ID (on_success -> Stage2 $STAGE2_ID)"

step "Create a card and move it into 'Stage1' (marker='$MARKER')"
card_json="$("$BOARD_BIN" card new --title "Ctx Card" -d "context flow" \
  --harness fake --space-kind workspace --space-ref "$WS_ID" --json)"
CARD_ID="$(printf '%s' "$card_json" | jget id)" || fail "could not parse card id"
echo "  card: $CARD_ID"
mut "board move $CARD_ID Stage1 -> agent.start; on ok auto-advances to Stage2"
e2e_board_herdr_mutate -- move "$CARD_ID" Stage1 --json >/dev/null

step "Wait for BOTH stage runs to finish (chained auto advance)"
oc="$(wait_runs "$CARD_ID" 2)" || { e2e_card_failure_diag "$CARD_ID"; fail "second (Stage2) run never finished"; }
echo "  last (Stage2) run outcome: $oc"
[ "$oc" = "ok" ] || fail "Stage2 run outcome '$oc', expected ok"

step "Assert the Stage2 process witnessed the Stage1 marker in its own prompt"
show="$("$BOARD_BIN" card show "$CARD_ID" --json)"
grep -Fq "STAGE2-WITNESSED:${MARKER}" <<<"$show" \
  || { e2e_card_failure_diag "$CARD_ID"; fail "Stage2 process never observed the Stage1 marker in BOARD_PROMPT"; }
ok "Stage2 process received the comment-bearing prompt (witness comment present)"

step "09-comment-context: ALL CHECKS PASSED"
