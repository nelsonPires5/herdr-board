//! Terminal setup, the clock, and the draw/input loop.
//!
//! The only module that touches a real terminal or the wall clock; everything
//! it drives ([`Driver`], `app::update`, `view::view`) stays testable without
//! either.

use std::io::Stdout;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use board_core::client::BoardClient;
use board_core::protocol::{BoardSnapshot, Event};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::app::Msg;
use crate::editor::RealEditor;
use crate::view::view;
use crate::{Driver, OriginContext};

fn epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The `board tui` entry point: set up the terminal, spawn an event-subscription
/// thread, and run the draw/input loop until quit.
pub fn run(client: Box<dyn BoardClient>) -> Result<()> {
    let mut driver = Driver::new(client)?;
    run_driver(&mut driver)
}

pub fn run_with_board(client: Box<dyn BoardClient>, board: BoardSnapshot) -> Result<()> {
    let mut driver = Driver::with_editor_and_board_and_origin(
        client,
        Box::new(RealEditor),
        board,
        OriginContext::from_environment(),
    )?;
    run_driver(&mut driver)
}

fn run_driver(driver: &mut Driver) -> Result<()> {
    // Live updates: a background thread turns board events into redraw pings.
    // Falls back silently to action-driven refetch when subscribe is empty /
    // unsupported (e.g. FakeBoardClient).
    let (tx, rx) = mpsc::channel::<()>();
    if let Ok(stream) = driver.subscribe() {
        std::thread::spawn(move || {
            let stream: Box<dyn Iterator<Item = Event> + Send> = stream;
            for _ev in stream {
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(driver, &mut terminal, &rx);

    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    res
}

fn event_loop(
    driver: &mut Driver,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    rx: &mpsc::Receiver<()>,
) -> Result<()> {
    loop {
        driver.app.now = epoch_secs();
        driver.app.now_ms = epoch_millis();
        driver.expire_toast();

        let size = terminal.size()?;
        let new_area = Rect::new(0, 0, size.width, size.height);
        // Bug A (resize half): a size change can leave stale cells behind too
        // (e.g. shrinking then growing back); force a full repaint whenever
        // the terminal size differs from what the last frame used.
        driver.sync_frame_area(new_area);

        if driver.take_needs_full_redraw() {
            terminal.clear()?;
        }
        terminal.draw(|f| view(&driver.app, f))?;

        if crossterm::event::poll(Duration::from_millis(200))? {
            match crossterm::event::read()? {
                CtEvent::Key(k) if k.kind == KeyEventKind::Press => {
                    driver.handle(Msg::Key(k));
                }
                CtEvent::Mouse(m) => driver.handle(Msg::Mouse(m)),
                _ => {}
            }
        } else {
            // Drain any pending redraw pings.
            let mut refreshed = false;
            while rx.try_recv().is_ok() {
                refreshed = true;
            }
            if refreshed {
                driver.handle(Msg::Refresh);
            }
        }

        if driver.app.should_quit {
            return Ok(());
        }
    }
}
