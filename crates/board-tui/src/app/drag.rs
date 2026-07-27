//! Mouse-drag lifecycle: begin, hover, finish.
//!
//! Only `app::mouse` drives these, but they are `pub` because they are the
//! seam the external drag tests act on directly — a drag has no key binding to
//! synthesize.

use board_core::protocol::CardMoveParams;

use super::{App, DragKind, DragState, Effect};

impl App {
    pub fn begin_card_drag(&mut self, card_id: i64, from_col: usize) {
        self.drag = Some(DragState {
            kind: DragKind::Card { card_id },
            from_col,
            hover_col: from_col,
        });
    }

    pub fn begin_column_drag(&mut self, column_id: i64, from_col: usize) {
        self.drag = Some(DragState {
            kind: DragKind::Column { column_id },
            from_col,
            hover_col: from_col,
        });
    }

    pub fn drag_hover(&mut self, col: usize) {
        if let Some(d) = &mut self.drag {
            d.hover_col = col;
        }
    }

    /// Complete a drag, producing a move/reorder effect when it landed elsewhere.
    pub fn finish_drag(&mut self) -> Vec<Effect> {
        let Some(d) = self.drag.take() else {
            return vec![];
        };
        if d.hover_col == d.from_col {
            return vec![];
        }
        match d.kind {
            DragKind::Card { card_id } => match self.col_id_at(d.hover_col) {
                Some(column_id) => vec![Effect::CardMove(CardMoveParams {
                    id: card_id,
                    column_id,
                    board_id: None,
                    position: None,
                })],
                None => vec![],
            },
            DragKind::Column { column_id } => vec![Effect::ColumnReorder {
                id: column_id,
                position: d.hover_col as i64,
            }],
        }
    }
}
