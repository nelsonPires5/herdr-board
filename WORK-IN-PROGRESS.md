# WORK-IN-PROGRESS — o que está aberto agora

Worktree: `/tmp/herdr-board-ui-redesign-1786043528` · branch `feat/tui-mobile-first-redesign` · base `main` = `9d0afee1432fa25f216cbaf23f47fb21d36fbecf` (checkout principal limpo, **nenhum commit feito**).

---

## 1. Por que a sessão parou

A sessão prime-agent morreu por **erros 402 do OpenRouter**, não por erro de código. Últimas 4 mensagens do assistant (07/08/2026 17:43:33 → 17:43:51 UTC) vieram todas com `stopReason: "error"`, modelo `openrouter/deepseek/deepseek-v4-flash-0731`:

```
402 Prompt tokens limit exceeded: 329159 > 168823
402 Prompt tokens limit exceeded: 329159 > 168823
402 This request requires more credits, or fewer max_tokens. You requested up to 32000 tokens, but can only afford 29281
402 Prompt tokens limit exceeded: 329159 > 105330
```

(Já tinha havido a mesma família de erros às 11:54 com `openrouter/deepseek/deepseek-v4-pro`: *"You requested up to 32000 tokens, but can only afford 17331 / 11141"*.)

Depois disso a sessão ficou apenas emitindo `agent_status: needs_input` até 20:22 UTC — **nenhum trabalho adicional foi feito**. O contexto acumulado (~329k tokens de prompt) excede o limite diário da chave OpenRouter; para retomar é preciso **aumentar o limite da chave, trocar de provider/modelo, ou compactar/reiniciar a sessão** (a sessão já estava enorme e o `/compact` da vez anterior não chegou a executar como slash command na API).

---

## 2. Última coisa que estava sendo feita

Sequência exata das últimas ações bem-sucedidas (07/08, entre 17:16 e 17:43):

1. Corrigido o **deadlock** de `detail_section_heights` (loop `while` infinito) — `cargo test` que ficou 3h47m preso foi cancelado pelo usuário.
2. `view/detail.rs::draw_runs` passou a clipar a lista de runs com `.skip(app.detail_runs_scroll).take(visible)` — a action bar in-card (`[Open pane] / [Retry run] / [Cancel run]`) estava sobrescrevendo a última linha de run.
3. Criada `pub fn runs_viewport_height(layout: &DetailLayout) -> usize { layout.runs.height.saturating_sub(3) }` (reserva título + action bar in-card + borda inferior) e adicionada ao `pub use detail::{...}` em `view/mod.rs` (isso faltou na primeira tentativa → 4× `E0425`).
4. Migrados de `height-1` para o helper: `app/detail.rs` (≈ linhas 48 e 64 em `scroll_detail_to_latest`/`scroll_detail`, e ≈ 154 e 208 nos clamps de `follow_run_focus`), `tests/layout.rs:381`, `tests/update/detail.rs:256, 425, 973, 1048`.
5. `tests/update/detail.rs::driver_with_detail_open` passou a setar `d.app.last_area = Rect::new(0,0,110,44)` e chamar `d.app.scroll_detail_to_latest()` após abrir o detail.
6. `tests/mouse.rs::card_detail_run_row_click_focuses_runs_and_selects_exact_older_run` ganhou `d.app.detail_runs_scroll = 0;` após semear as runs.
7. **Regeneração dos snapshots insta** após a mudança de clip das run rows:
   `INSTA_UPDATE=always timeout 400 cargo test -p board-tui --all-features --test snapshots` → `rc=0`, **69 passed**. (Este foi literalmente o último passo concluído.)
8. Rodada final de verificação → 1 teste ainda vermelho (§3). Ao tentar diagnosticá-lo, os 402 mataram a sessão.

---

## 3. ITEM QUE ESTÁ FALHANDO AGORA (único)

```
test detail::shrinking_detail_to_popup_reanchors_history_to_latest ... FAILED
crates/board-tui/tests/update/detail.rs:1049
assertion `left == right` failed: comments re-anchor to the latest visible row
  left: 1
 right: 0
```

Suíte `update`: **112 passed / 1 failed**. Todo o resto está verde (help 8, lib 17+1 ignorado, 3, 2, layout 15, mouse 35, snapshots 69), `cargo fmt` OK e `cargo clippy -p board-tui --all-targets --all-features -- -D warnings` rc=0. Confirmado re-executando agora, o estado não mudou.

### Diagnóstico (verificado no código, ainda não corrigido)

O teste monta o detail em `Rect::new(0,0,254,67)` com `detail_fullscreen = true`, chama `scroll_detail_to_latest()`, aperta `f` (que faz `toggle_detail_fullscreen` → encolhe para popup e re-chama `scroll_detail_to_latest`) e então assere:

```rust
let (_, comments_visible) = board_tui::view::comments_viewport(&app, &layout);
assert_eq!(app.detail_comments_scroll,
           detail.comments.len().saturating_sub(comments_visible),   // ← len de COMENTÁRIOS
           "comments re-anchor to the latest visible row");
```

Mas a implementação (`app/detail.rs::scroll_detail_to_latest`) é **row-based**, não item-based:

```rust
let comments_total = crate::view::comment_wrapped_rows(detail, layout.comments.width);
let (_, comments_visible) = crate::view::comments_viewport(self, &layout);
self.detail_comments_scroll = comments_total.saturating_sub(comments_visible.max(1));
```

Ou seja: o teste compara `comments.len() - visible` contra um offset calculado sobre **linhas wrapped**. Com o viewport de comentários agora reservando 3 linhas (`comments_viewport` = `height - 3`, por causa da action bar dentro do card) em vez de 1, as linhas wrapped passaram a exceder o visível em 1 → `scroll = 1`, enquanto a expectativa do teste satura em `0`.

A segunda assertiva (runs, `detail.runs.len() - runs_viewport_height`) é consistente porque runs ocupam 1 linha cada; a de comentários não é.

**Correção provável** (decidir e aplicar): trocar a expectativa do teste por `board_tui::view::comment_wrapped_rows(detail, layout.comments.width).saturating_sub(comments_visible.max(1))`, alinhando o teste ao mesmo par de helpers usado pela implementação. Alternativa (menos provável de ser a intenção): mudar `scroll_detail_to_latest` para ancorar por índice de comentário — mas isso quebraria o scroll wrapped e outros testes de comentário.

Arquivos envolvidos:
- `crates/board-tui/tests/update/detail.rs` (≈ 1044–1060, o assert em 1049)
- `crates/board-tui/src/app/detail.rs::scroll_detail_to_latest` (≈ linhas 39–52)
- `crates/board-tui/src/view/detail.rs::comments_viewport` (linha 345) e `runs_viewport_height` (linha 361)

---

## 4. TODOs abertos (ajustes de UI ainda não terminados)

Pedidos do usuário (rodada de 07/08 11:51) que ainda não foram concluídos/validados:

1. **Help overlay em "cards por seção"** — ainda é lista/flat. Era o item marcado como "straw no topo, demora".
2. **Bug do `Esc` que às vezes abre "Move column"** — usuário: *"vira e mexe acontece deu clicar em alguma função e quando aperto esc aparece para mover a coluna. acho que é um bug."* Suspeita registrada: foco de drag/`Esc` no waiter. Não reproduzido com o binário novo.
3. **Forms** — cursor visível em `title`/`description` a confirmar visualmente; new/edit **column** também precisa do visual de cards (cai na mesma `draw_form` via `is_column`, falta conferir).
4. **Botões "sem cor, fundo branco, cor só no label"** — já feito em `ActionBar`/`ActionStrip`; falta conferir pickers, detail e help, onde alguns usam `Paragraph` próprio em vez de `ActionStrip`.
5. **Fullscreen mode em new/edit card** (`f` → `App.form_fullscreen`) — implementado, falta validar visualmente nos 7 layouts.
6. **Panes vazios da sessão Herdr** — usuário pediu para fechar os que não importam.
7. **`PLAN.md` não existe mais** na worktree (perdido quando `/tmp` foi limpo). Se for necessário rastrear T0–T10, recriar a partir de `docs/tui-interactions.md` + deste documento.

---

## 5. Passos exatos para retomar

```bash
cd /tmp/herdr-board-ui-redesign-1786043528

# 1. Corrigir o único teste vermelho (ver §3)
timeout 300 cargo test -p board-tui --all-features --test update shrinking_detail_to_popup 2>&1 | tail -20

# 2. Suíte do crate verde + gates de estilo (SEMPRE com timeout — já houve hang de 3h47m)
timeout 300 cargo test -p board-tui --all-features > /tmp/hb-tui-resume.log 2>&1; echo rc=$?
cargo fmt --all
cargo clippy -p board-tui --all-targets --all-features -- -D warnings

# 3. Só se a renderização mudar de novo:
INSTA_UPDATE=always timeout 400 cargo test -p board-tui --all-features --test snapshots

# 4. Gates completos (precisam ser re-rodados: só passaram antes da rodada de section-cards)
timeout 600 cargo test --workspace --all-features
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
bash e2e/test-harness.sh
bash e2e/ci.sh            # e2e 01–29; 10/13/21/22/26/28 já foram ajustados

# 5. Rebuild + revalidação visual
cargo build --release
```

### Sessão visual (obrigatório manter isolada)

- Sessão: `herdr --session hb-visual-final2-2-71aabd`, workspace `w3` "HB UI Final · v3".
- DB/socket/config isolados em `/tmp/hbvf2.2e35c2`; daemon isolado `target/release/board daemon --foreground` (PID em `/tmp/hbvf2.2e35c2/state.env`, chaves `DAEMON_PID*`); card 13 rodando (harness hold).
- Os 7 panes atuais (`w3:p4..pA`) rodam **binário desatualizado**. Após o rebuild: fechar as tabs antigas e recriar os 7 panes com `stty cols/rows` + `BOARD_DB`/`BOARD_SOCKET`/`HERDR_BOARD_CONFIG`/`BOARD_SCOPE_PATH` isolados e `env -u NO_COLOR TERM=xterm-256color COLORTERM=truecolor <target/release/board> tui`, rotulados:
  `01 Compact 40x20 Board`, `02 Compact 52x24 New Card`, `03 Regular 60x24 Picker`, `04 Regular 80x24 Detail`, `05 Regular 80x24 Help`, `06 Wide 120x35 Board` (focar), `07 Wide 120x35 Detail Full`.

### Regras não negociáveis ao retomar

- **NUNCA** rodar `herdr ... plugin link/install/unlink/uninstall` — o registro de plugins do Herdr é **global** e já foi quebrado uma vez nesta sessão. Só `plugin list` (read-only).
- Manter `main` limpo em `9d0afee` e o plugin global em `nelsonPires5/herdr-board` v0.11.0.
- Sempre limpar `NO_COLOR` nos panes (já causou um falso "regressão visual" reportado pelo usuário).
- **Nenhum commit sem pedido explícito.**
- Entrega final ao usuário = devolver a sessão `herdr --session hb-visual-final2-2-71aabd` (workspace `w3`) para ele validar os 7 layouts.
