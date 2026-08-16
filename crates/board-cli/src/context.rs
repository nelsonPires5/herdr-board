//! The per-invocation command context.
//!
//! Every handler used to open with `let mut c = connect_or_start()?;` followed
//! by `open_selected_board(&mut c, selector)?`. [`Ctx`] owns both, lazily: the
//! client is only dialed on first use (so a refused confirmation never
//! auto-starts boardd), and the selected board snapshot is fetched at most once
//! per invocation.

use anyhow::{anyhow, Result};
use board_core::client::UnixClient;
use board_core::model::Column;
use board_core::protocol::BoardSnapshot;

use crate::daemon::connect_or_start;
use crate::scope::{open_selected_board, resolve_column_in};

pub(crate) struct Ctx<'a> {
    selector: Option<&'a str>,
    json: bool,
    client: Option<UnixClient>,
    board: Option<BoardSnapshot>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(selector: Option<&'a str>, json: bool) -> Self {
        Self {
            selector,
            json,
            client: None,
            board: None,
        }
    }

    /// The global `--board` selector, if any.
    pub(crate) fn selector(&self) -> Option<&'a str> {
        self.selector
    }

    /// Whether this invocation asked for JSON output.
    pub(crate) fn json(&self) -> bool {
        self.json
    }

    /// The boardd client, connecting (and auto-starting boardd) on first use.
    pub(crate) fn client(&mut self) -> Result<&mut UnixClient> {
        if self.client.is_none() {
            self.client = Some(connect_or_start()?);
        }
        self.client
            .as_mut()
            .ok_or_else(|| anyhow!("boardd client unavailable"))
    }

    /// The selected board snapshot, fetched once per invocation.
    pub(crate) fn board(&mut self) -> Result<&BoardSnapshot> {
        if self.board.is_none() {
            let selector = self.selector;
            let board = open_selected_board(self.client()?, selector)?;
            self.board = Some(board);
        }
        self.board
            .as_ref()
            .ok_or_else(|| anyhow!("board snapshot unavailable"))
    }

    pub(crate) fn board_id(&mut self) -> Result<i64> {
        Ok(self.board()?.board.id)
    }

    /// Resolve a column reference (id or case-insensitive name) on the selected
    /// board.
    pub(crate) fn column_id(&mut self, reference: &str) -> Result<i64> {
        let board = self.board()?;
        resolve_column_in(board, reference)
    }

    pub(crate) fn optional_column_id(&mut self, reference: Option<&str>) -> Result<Option<i64>> {
        match reference {
            Some(reference) => self.column_id(reference).map(Some),
            None => Ok(None),
        }
    }

    /// A copy of one column of the selected board.
    pub(crate) fn column(&mut self, reference: &str) -> Result<Column> {
        let id = self.column_id(reference)?;
        let board = self.board()?;
        board
            .columns
            .iter()
            .find(|column| column.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("column {id} not found"))
    }

    /// Give up the context, keeping the already-connected client (the TUI takes
    /// ownership of it).
    pub(crate) fn into_client(self) -> Result<UnixClient> {
        match self.client {
            Some(client) => Ok(client),
            None => connect_or_start(),
        }
    }
}
