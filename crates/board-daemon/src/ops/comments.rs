use super::*;

pub(super) fn comment_add(d: &Arc<Daemon>, p: CommentAddParams) -> Result<Value> {
    let author = p.author.as_deref().unwrap_or("user");
    let comment = d.store.lock().add_comment(p.card_id, author, &p.body)?;
    d.emit_changed(BoardChangedReason::CommentAdded, Some(p.card_id), None);
    Ok(json!(comment))
}
