//! board-tui — ratatui kanban app (OWNED BY PHASE C).
//!
//! The full kanban TUI over a [`BoardClient`](board_core::client::BoardClient).
//! Phase D's `board tui` calls [`run`]; tests drive the same [`Driver`] with
//! synthetic events and a `TestBackend`.
//!
//! Design: a pure state machine (`app::update`) + a pure renderer (`view::view`),
//! with all I/O confined to the [`driver`] (client calls, `$EDITOR`) and
//! [`runtime`] (terminal, clock) modules. See `docs/design.md` §4.
//!
//! Layering, outermost first:
//!
//! | module | owns |
//! |---|---|
//! | [`runtime`] | terminal setup, the epoch clock, the draw/input loop |
//! | [`driver`] | effect execution: client calls, `$EDITOR`, redraw flags |
//! | [`app`] | the pure state machine (`Screen`/`App`/`update`) |
//! | [`view`], [`widgets`] | the pure renderer |
//! | [`forms`], [`editor`] | form model and `$EDITOR` launching |
//! | [`origin`] | the Herdr/plugin boundary |
//!
//! Everything the external test crates and `board-cli` use is re-exported here,
//! so `board_tui::{Driver, OriginContext, run, run_with_board}` stay valid paths.

pub mod app;
pub mod driver;
pub mod editor;
pub mod forms;
pub mod origin;
pub mod runtime;
#[cfg(feature = "fake-client")]
pub mod testkit;
pub mod view;
pub mod widgets;

pub use driver::Driver;
pub use origin::OriginContext;
pub use runtime::{run, run_with_board};
