use board_core::protocol::{CardStatus, RunOutcome};
use crossterm::event::{KeyCode, KeyEvent};

use crate::forms::Form;

use super::{App, Confirm, ConfirmPurpose, DetailScrollTarget, Effect, Screen};

pub(super) fn detail_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let card_id = app.detail.as_ref().map(|d| d.card.id);
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.screen = Screen::Board;
            app.detail = None;
            app.detail_fullscreen = false;
            app.detail_comments_scroll = 0;
            app.detail_runs_scroll = 0;
        }
        KeyCode::Char('f') => app.toggle_detail_fullscreen(),
        KeyCode::Tab => {
            app.detail_scroll_target = match app.detail_scroll_target {
                DetailScrollTarget::Comments => DetailScrollTarget::Runs,
                DetailScrollTarget::Runs => DetailScrollTarget::Comments,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => app.scroll_detail(-1),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_detail(1),
        KeyCode::Char('e') => {
            if let Some(card) = app.detail.as_ref().map(|d| d.card.clone()) {
                app.form_from_detail = true;
                app.form = Some(Form::card_edit(&card));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('a') => {
            let Some(card) = app.detail.as_ref().map(|detail| &detail.card) else {
                return vec![];
            };
            if card.archived_at.is_none()
                && matches!(
                    card.status,
                    CardStatus::Queued
                        | CardStatus::Running
                        | CardStatus::Blocked
                        | CardStatus::Awaiting
                )
            {
                app.set_toast("card has an active run; cancel it before archiving", true);
            } else {
                return vec![Effect::CardArchive {
                    id: card.id,
                    archived: card.archived_at.is_none(),
                }];
            }
        }
        KeyCode::Char('c') => {
            if let Some(id) = card_id {
                app.form_from_detail = true;
                app.form = Some(Form::comment(id));
                app.screen = Screen::CardForm;
            }
        }
        KeyCode::Char('o') => {
            if let Some(id) = card_id {
                return vec![Effect::FocusRun(id)];
            }
        }
        // Enter on an `awaiting` card confirms completion: the same `run.done`
        // (ok) channel as `board done ok`. Other statuses: Enter is a no-op
        // (`done` is a final visual state).
        KeyCode::Enter => {
            if let Some(detail) = &app.detail {
                if detail.card.status == CardStatus::Awaiting {
                    return vec![Effect::RunDone(detail.card.id, RunOutcome::Ok)];
                }
            }
        }
        KeyCode::Char('x') => {
            if let Some(id) = card_id {
                app.confirm = Some(Confirm {
                    message: "Cancel the running run?".into(),
                    purpose: ConfirmPurpose::CancelRun(id),
                });
                app.screen = Screen::Confirm;
            }
        }
        KeyCode::Char('r') => {
            if let Some(id) = card_id {
                return vec![Effect::RunRetry(id)];
            }
        }
        KeyCode::Char('?') => app.screen = Screen::Help,
        _ => {}
    }
    vec![]
}
