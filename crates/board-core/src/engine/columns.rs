use crate::model::Column;

/// Resolve a user-typed column reference against one board's columns.
///
/// A numeric reference is an id and only matches a column present in
/// `columns`; anything else (including a number that is not an id on this
/// board) is matched against the column name, case-insensitively. The first
/// name match wins, so duplicate names resolve to the earliest column in the
/// supplied order. `None` means "no column matches"; callers own the error
/// message.
pub fn resolve_column(columns: &[Column], reference: &str) -> Option<i64> {
    if let Ok(id) = reference.parse::<i64>() {
        if columns.iter().any(|col| col.id == id) {
            return Some(id);
        }
    }
    let lower = reference.to_lowercase();
    columns
        .iter()
        .find(|col| col.name.to_lowercase() == lower)
        .map(|col| col.id)
}
