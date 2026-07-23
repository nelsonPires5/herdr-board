//! Modal form model: card create/edit, column create/edit, and add-comment.
//!
//! A [`Form`] is a flat list of [`Field`]s plus a focus index. Fields are either
//! free text (backed by a `tui_textarea::TextArea` so `Ctrl+E` can hand the buffer
//! to `$EDITOR`) or a cyclic [`Choice`]. Rendering lives in `view`; this module
//! owns construction, focus movement, field cycling, and turning a submitted form
//! into a protocol params struct.

mod builders;
mod fields;
mod options;
mod submit;

pub use fields::{ChoiceOpt, ChoiceVal, Field, FieldId, FieldKind, Form, FormKind, Submit};
pub use submit::session_name_from_socket;

use builders::{build_card_fields, column_fields_from_values, CardValues, ColumnValues};
