//! Terminal setup, the clock, and the draw/input loop.
//!
//! The only module that touches a real terminal or the wall clock; everything
//! it drives ([`Driver`], `app::update`, `view::view`) stays testable without
//! either.

use std::io::Stdout;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use board_core::client::{BoardClient, UnixClient};
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

const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(100);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubscriptionSignal {
    Changed,
    Reconnected,
}

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
    // Live updates use a dedicated socket. A Unix client also supplies the
    // exact path needed to recover after boardd replacement; embedded/fake
    // clients keep the old one-shot/action-driven fallback.
    let (tx, rx) = mpsc::channel::<SubscriptionSignal>();
    let initial_stream = driver.subscribe().ok();
    match driver.reconnect_path() {
        Some(path) => {
            std::thread::spawn(move || {
                supervise_subscription(
                    initial_stream,
                    move || {
                        let mut client = UnixClient::connect(&path)?;
                        client.subscribe()
                    },
                    tx,
                    std::thread::sleep,
                );
            });
        }
        None => {
            if let Some(stream) = initial_stream {
                std::thread::spawn(move || forward_events(stream, &tx));
            }
        }
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
    rx: &mpsc::Receiver<SubscriptionSignal>,
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
            // Drain and coalesce pending refresh/reconnect signals. A daemon
            // replacement invalidates the request socket as well as the event
            // stream, so install a fresh request client before refetching.
            let mut refreshed = false;
            let mut reconnected = false;
            while let Ok(signal) = rx.try_recv() {
                match signal {
                    SubscriptionSignal::Changed => refreshed = true,
                    SubscriptionSignal::Reconnected => reconnected = true,
                }
            }
            if reconnected {
                refreshed = driver
                    .reconnect_path()
                    .as_deref()
                    .is_some_and(|path| driver.reconnect(path));
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

fn forward_events(
    stream: Box<dyn Iterator<Item = Event> + Send>,
    tx: &mpsc::Sender<SubscriptionSignal>,
) {
    for _event in stream {
        if tx.send(SubscriptionSignal::Changed).is_err() {
            break;
        }
    }
}

fn supervise_subscription<Reconnect, Sleep>(
    mut stream: Option<Box<dyn Iterator<Item = Event> + Send>>,
    reconnect: Reconnect,
    tx: mpsc::Sender<SubscriptionSignal>,
    sleep: Sleep,
) where
    Reconnect: Fn() -> Result<Box<dyn Iterator<Item = Event> + Send>>,
    Sleep: Fn(Duration),
{
    let mut backoff = RECONNECT_BACKOFF_MIN;
    loop {
        if let Some(current) = stream.take() {
            forward_events(current, &tx);
        }

        match reconnect() {
            Ok(next) => {
                // This ping forces a complete snapshot fetch even if every
                // event from the outage window was lost before resubscribe.
                if tx.send(SubscriptionSignal::Reconnected).is_err() {
                    return;
                }
                stream = Some(next);
                backoff = RECONNECT_BACKOFF_MIN;
            }
            Err(_) => {
                sleep(backoff);
                backoff = backoff.saturating_mul(2).min(RECONNECT_BACKOFF_MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::protocol::{BoardChangedReason, Event};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn changed_event() -> Event {
        Event::BoardChanged {
            reason: BoardChangedReason::CardUpdated,
            board_id: Some(2),
            card_id: Some(7),
            column_id: Some(3),
        }
    }

    #[test]
    fn subscription_retries_then_forces_refetch_before_forwarding_events() {
        let (signal_tx, signal_rx) = mpsc::channel();
        let attempts = Arc::new(Mutex::new(0_u8));
        let attempts_in_reconnect = attempts.clone();
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let sleeps_in_fn = sleeps.clone();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        struct HeldStream {
            events: VecDeque<Event>,
            release: Arc<Mutex<mpsc::Receiver<()>>>,
        }

        impl Iterator for HeldStream {
            type Item = Event;

            fn next(&mut self) -> Option<Event> {
                self.events
                    .pop_front()
                    .or_else(|| self.release.lock().ok()?.recv().ok().and(None))
            }
        }

        let worker = std::thread::spawn(move || {
            supervise_subscription(
                Some(Box::new(std::iter::empty())),
                move || {
                    let mut attempts = attempts_in_reconnect.lock().unwrap();
                    *attempts += 1;
                    if *attempts == 1 {
                        anyhow::bail!("daemon absent")
                    }
                    Ok(Box::new(HeldStream {
                        events: VecDeque::from([changed_event()]),
                        release: release_rx.clone(),
                    })
                        as Box<dyn Iterator<Item = Event> + Send>)
                },
                signal_tx,
                move |delay| sleeps_in_fn.lock().unwrap().push(delay),
            );
        });

        assert_eq!(signal_rx.recv().unwrap(), SubscriptionSignal::Reconnected);
        assert_eq!(signal_rx.recv().unwrap(), SubscriptionSignal::Changed);
        assert_eq!(*sleeps.lock().unwrap(), vec![RECONNECT_BACKOFF_MIN]);

        drop(signal_rx);
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(*attempts.lock().unwrap(), 3);
    }
}
