//! The single output path for every command.
//!
//! Handlers never branch on `--json` themselves: they hand a serializable value
//! to [`emit`] (types with one canonical text form implement [`Render`]) or to
//! [`emit_line`] (a payload plus a confirmation message). Text listings all go
//! through [`table`], so every list in the CLI aligns the same way.

use std::io::{self, Write};

use anyhow::Result;
use board_core::capability::HarnessCapabilities;
use board_core::model::{Board, Card, Column, Comment, CommentHistory, CommentRecord};
use board_core::protocol::{
    BoardSnapshot, CardDetail, DaemonStatus, ProjectDetail, ProjectListResult, ProjectOpenResult,
    SessionListResult, SpaceListResult,
};
use serde::{Serialize, Serializer};

use crate::helpers::efforts_str;

/// The human (non-`--json`) rendering of one emitted value.
pub(crate) trait Render {
    fn render(&self, out: &mut dyn Write) -> io::Result<()>;
}

/// Emit `value`: pretty JSON with `--json`, otherwise its [`Render`] form.
pub(crate) fn emit<T: Serialize + Render + ?Sized>(value: &T, json: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if json {
        serde_json::to_writer_pretty(&mut out, value)?;
        out.write_all(b"\n")?;
    } else {
        value.render(&mut out)?;
    }
    out.flush()?;
    Ok(())
}

/// Emit a JSON payload whose text form is a plain message. `text` may be empty
/// (nothing is printed) or span several lines.
pub(crate) fn emit_line<T: Serialize + ?Sized>(
    value: &T,
    json: bool,
    text: impl Into<String>,
) -> Result<()> {
    emit(
        &Message {
            value,
            text: text.into(),
        },
        json,
    )
}

/// A payload plus its text form; serializes as the payload alone so the JSON
/// contract is unchanged by the message.
struct Message<'a, T: ?Sized> {
    value: &'a T,
    text: String,
}

impl<T: Serialize + ?Sized> Serialize for Message<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

impl<T: ?Sized> Render for Message<'_, T> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        if self.text.is_empty() {
            return Ok(());
        }
        writeln!(out, "{}", self.text)
    }
}

/// Render `rows` as space-aligned columns. Every cell is padded to the widest
/// value in its column and trailing padding is trimmed, so a ragged last column
/// (an absent `session=`, say) never leaves stray whitespace.
pub(crate) fn table(out: &mut dyn Write, rows: &[Vec<String>]) -> io::Result<()> {
    let mut widths: Vec<usize> = Vec::new();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            let width = cell.chars().count();
            match widths.get_mut(index) {
                Some(current) => *current = (*current).max(width),
                None => widths.push(width),
            }
        }
    }
    for row in rows {
        let mut line = String::new();
        for (index, cell) in row.iter().enumerate() {
            if index > 0 {
                line.push_str("  ");
            }
            line.push_str(cell);
            if index + 1 < row.len() {
                let pad = widths
                    .get(index)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(cell.chars().count());
                line.push_str(&" ".repeat(pad));
            }
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

/// One comment line, shared by `card show` and `card comment show` so the same
/// entity reads identically wherever it appears.
fn comment_row(
    id: i64,
    card_id: i64,
    author: &str,
    created_at: &str,
    body: &str,
    deleted: bool,
) -> String {
    format!(
        "#{id} card={card_id} {author} ({created_at}): {body}{}",
        if deleted { " [deleted]" } else { "" }
    )
}

// -- boards -------------------------------------------------------------------

impl Render for Vec<Board> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self
            .iter()
            .map(|board| {
                let name = if board.archived_at.is_some() {
                    format!("{} (archived)", board.name)
                } else {
                    board.name.clone()
                };
                vec![
                    format!("#{}", board.id),
                    name,
                    board.scope_path.clone().unwrap_or_default(),
                    board
                        .archived_at
                        .as_deref()
                        .map(|ts| format!("archived={ts}"))
                        .unwrap_or_default(),
                ]
            })
            .collect();
        table(out, &rows)
    }
}

impl Render for BoardSnapshot {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let archived = self
            .board
            .archived_at
            .as_deref()
            .map(|ts| format!(" archived={ts}"))
            .unwrap_or_default();
        writeln!(out, "#{}  {}{}", self.board.id, self.board.name, archived)?;
        if let Some(path) = &self.board.scope_path {
            writeln!(out, "scope: {path}")?;
        }
        writeln!(
            out,
            "columns: {}  cards: {}",
            self.columns.len(),
            self.cards.len()
        )
    }
}

impl Render for ProjectListResult {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self
            .projects
            .iter()
            .map(|info| {
                let name = if info.project.archived_at.is_some() {
                    format!("{} (archived)", info.project.name)
                } else {
                    info.project.name.clone()
                };
                let archived = info
                    .project
                    .archived_at
                    .as_deref()
                    .map(|ts| format!("archived={ts}"))
                    .unwrap_or_default();
                vec![
                    format!("#{}", info.project.id),
                    name,
                    info.project.scope_path.clone().unwrap_or_default(),
                    format!("boards:{}", info.boards.len()),
                    archived,
                ]
            })
            .collect();
        table(out, &rows)
    }
}

impl Render for ProjectDetail {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let archived = self
            .project
            .archived_at
            .as_deref()
            .map(|ts| format!(" archived={ts}"))
            .unwrap_or_default();
        writeln!(
            out,
            "#{}  {}  {}{}",
            self.project.id,
            self.project.name,
            self.project.scope_path.as_deref().unwrap_or_default(),
            archived
        )?;
        match &self.selected_board {
            Some(board) => writeln!(
                out,
                "boards: {} (selected: {})",
                self.boards.len(),
                board.name
            )?,
            None => writeln!(out, "boards: {}", self.boards.len())?,
        }
        for board in &self.boards {
            let suffix = board
                .archived_at
                .as_deref()
                .map(|ts| format!(" (archived {})", ts))
                .unwrap_or_default();
            writeln!(out, "#{}  {}{}", board.id, board.name, suffix)?;
        }
        Ok(())
    }
}

impl Render for ProjectOpenResult {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(
            out,
            "#{}  {}  {}",
            self.project.id,
            self.project.name,
            self.project.scope_path.as_deref().unwrap_or_default()
        )?;
        self.board.render(out)
    }
}

// -- columns ------------------------------------------------------------------

fn column_row(column: &Column) -> Vec<String> {
    vec![
        format!("#{}", column.id),
        format!("pos={}", column.position),
        format!("[{}]", column.trigger),
        column.name.clone(),
    ]
}

impl Render for Column {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        table(out, &[column_row(self)])
    }
}

impl Render for Vec<Column> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self.iter().map(column_row).collect();
        table(out, &rows)
    }
}

// -- cards --------------------------------------------------------------------

impl Render for Vec<Card> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self
            .iter()
            .map(|card| {
                vec![
                    format!("#{}", card.id),
                    format!("[{}]", card.status),
                    format!("col={}", card.column_id),
                    card.title.clone(),
                    card.session
                        .as_deref()
                        .map(|session| format!("session={session}"))
                        .unwrap_or_default(),
                    if card.archived_at.is_some() {
                        "archived".to_string()
                    } else {
                        String::new()
                    },
                ]
            })
            .collect();
        table(out, &rows)
    }
}

impl Render for CardDetail {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(
            out,
            "#{} {}  [{}{}]",
            self.card.id,
            self.card.title,
            self.card.status,
            if self.card.archived_at.is_some() {
                ", archived"
            } else {
                ""
            }
        )?;
        // Daemon-stamped label: the resolved session name, or the
        // `default session` marker. Old-daemon fallback: the raw wire field.
        if !self.card.labels.session.is_empty() {
            writeln!(out, "session: {}", self.card.labels.session)?;
        } else if let Some(session) = &self.card.session {
            writeln!(out, "session: {session}")?;
        }
        if !self.card.description.is_empty() {
            writeln!(out, "\n{}", self.card.description)?;
        }
        if !self.comments.is_empty() {
            writeln!(out, "\nComments:")?;
            for comment in &self.comments {
                writeln!(out, "  {}", comment_row_for(comment))?;
            }
        }
        if !self.runs.is_empty() {
            writeln!(out, "\nRuns:")?;
            let rows: Vec<Vec<String>> = self
                .runs
                .iter()
                .map(|run| {
                    vec![
                        format!("  #{}", run.id),
                        format!("col={}", run.column_id),
                        run.outcome
                            .map(|outcome| outcome.to_string())
                            .unwrap_or_else(|| "-".into()),
                        format!("started={:?}", run.started_at),
                        format!("ended={:?}", run.ended_at),
                    ]
                })
                .collect();
            table(out, &rows)?;
        }
        Ok(())
    }
}

/// `Comment` is the non-deleted projection, so it never carries a marker.
fn comment_row_for(comment: &Comment) -> String {
    comment_row(
        comment.id,
        comment.card_id,
        &comment.author,
        &comment.created_at,
        &comment.body,
        false,
    )
}

// -- comments -----------------------------------------------------------------

impl Render for CommentRecord {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(
            out,
            "{}",
            comment_row(
                self.id,
                self.card_id,
                &self.author,
                &self.created_at,
                &self.body,
                self.deleted_at.is_some(),
            )
        )
    }
}

impl Render for Vec<CommentHistory> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        for entry in self {
            writeln!(
                out,
                "#{} comment=#{} {} ({}): {}{}",
                entry.id,
                entry.comment_id,
                entry.author,
                entry.created_at,
                entry.body,
                if entry.deleted_at.is_some() {
                    " [deleted]"
                } else {
                    ""
                }
            )?;
        }
        Ok(())
    }
}

// -- discovery ----------------------------------------------------------------

impl Render for DaemonStatus {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(
            out,
            "boardd {}  db={}  herdr={}  active={}  queued={}",
            self.version,
            self.db_path,
            if self.herdr_connected {
                "connected"
            } else {
                "absent"
            },
            self.active_runs,
            self.queued_runs
        )
    }
}

impl Render for Vec<String> {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        for value in self {
            writeln!(out, "{value}")?;
        }
        Ok(())
    }
}

impl Render for HarnessCapabilities {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self
            .models
            .iter()
            .map(|model| vec![model.id.clone(), efforts_str(&model.efforts)])
            .collect();
        table(out, &rows)?;
        if self.model_freeform {
            if self.models.is_empty() {
                writeln!(
                    out,
                    "(any model string accepted; catalog comes from harness config)"
                )?;
            } else {
                writeln!(
                    out,
                    "\n(any model string accepted; these are known aliases)"
                )?;
            }
        }
        Ok(())
    }
}

impl Render for SpaceListResult {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        let rows: Vec<Vec<String>> = self
            .spaces
            .iter()
            .map(|space| vec![space.id.clone(), space.label.clone()])
            .collect();
        table(out, &rows)
    }
}

impl Render for SessionListResult {
    fn render(&self, out: &mut dyn Write) -> io::Result<()> {
        // The marker for the default session is daemon-sent (ready string).
        // Empty only when an older daemon predates the field: fall back to the
        // legacy marker rather than blanking the column.
        let default_label = if self.default_label.is_empty() {
            "(default)".to_string()
        } else {
            self.default_label.clone()
        };
        let rows: Vec<Vec<String>> = self
            .sessions
            .iter()
            .map(|session| {
                vec![
                    session.name.clone(),
                    if session.running {
                        "running"
                    } else {
                        "stopped"
                    }
                    .to_string(),
                    if session.default {
                        default_label.clone()
                    } else {
                        String::new()
                    },
                ]
            })
            .collect();
        table(out, &rows)
    }
}

#[cfg(test)]
#[path = "render/tests.rs"]
mod tests;
