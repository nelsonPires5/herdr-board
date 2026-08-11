//! Mouse-drag lifecycle: begin, hover, finish.
//!
//! Only `app::mouse` drives these, but they are `pub` because they are the
//! seam the external drag tests act on directly — a drag has no key binding to
//! synthesize.

use board_core::protocol::CardMoveParams;

use super::{App, DragKind, DragState, Effect};

impl App {
    pub fn begin_card_drag(&mut self, card_id: i64, from_col: usize) {
        let from_card = self
            .col_id_at(from_col)
            .and_then(|col_id| self.cards_of(col_id).iter().position(|c| c.id == card_id));
        self.drag = Some(DragState {
            kind: DragKind::Card { card_id },
            from_col,
            hover_col: from_col,
            from_card,
            hover_card: from_card,
        });
    }

    pub fn begin_column_drag(&mut self, column_id: i64, from_col: usize) {
        self.drag = Some(DragState {
            kind: DragKind::Column { column_id },
            from_col,
            hover_col: from_col,
            from_card: None,
            hover_card: None,
        });
    }

    pub fn drag_hover(&mut self, col: usize) {
        if let Some(d) = &mut self.drag {
            d.hover_col = col;
        }
    }

    /// Hover a card position within a column: `col` is the hovered column and
    /// `card` the hovered card's index there (the position a same-column drop
    /// lands at), `None` when hovering the column's empty space. Only
    /// meaningful for a card drag; column drags track columns only.
    pub fn drag_hover_card(&mut self, col: usize, card: Option<usize>) {
        if let Some(d) = &mut self.drag {
            d.hover_col = col;
            d.hover_card = card;
        }
    }

    /// Complete a drag, producing a move/reorder effect when it landed
    /// elsewhere.
    ///
    /// A card dropped in another column keeps the historical cross-column
    /// move (append, no position). A card dropped back in its own column is a
    /// **reorder**: one same-column `card.move` with the hovered card's
    /// position — never a column change and never an auto-column dispatch.
    /// Dropping back on the card's own slot (or on empty space with no card
    /// hovered) is a no-op.
    pub fn finish_drag(&mut self) -> Vec<Effect> {
        let Some(d) = self.drag.take() else {
            return vec![];
        };
        match d.kind {
            DragKind::Card { card_id } => {
                let Some(column_id) = self.col_id_at(d.hover_col) else {
                    return vec![];
                };
                if d.hover_col == d.from_col {
                    // Same-column reorder. `hover_card` is the card index the
                    // drop lands on; `from_card` is where the drag began.
                    let Some(target) = d.hover_card else {
                        return vec![];
                    };
                    if d.from_card == Some(target) {
                        return vec![];
                    }
                    vec![Effect::CardMove(CardMoveParams {
                        id: card_id,
                        column_id,
                        board_id: None,
                        position: Some(target as i64),
                    })]
                } else {
                    vec![Effect::CardMove(CardMoveParams {
                        id: card_id,
                        column_id,
                        board_id: None,
                        position: None,
                    })]
                }
            }
            DragKind::Column { column_id } => {
                if d.hover_col == d.from_col {
                    return vec![];
                }
                vec![Effect::ColumnReorder {
                    id: column_id,
                    position: d.hover_col as i64,
                }]
            }
        }
    }
}
