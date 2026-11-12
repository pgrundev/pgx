//! The interactive loop: terminal guard, collector worker, input.
//!
//! The collector runs on its own thread and hands finished snapshots over a
//! bounded channel (capacity 1), so a slow SQL read or filesystem scan never
//! blocks input or drawing; the UI loop wakes at least every 100 ms.
//! [`TerminalGuard`] restores the terminal (raw mode, alternate screen,
//! cursor) on every exit path: normal return, `?` errors, SIGTERM/SIGHUP/
//! SIGINT, and — best effort — panics.
use super::app::{App, Key};
use super::model::TopSnapshot;
use crate::errors::{PgxError, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// Where snapshots come from.
pub enum Source {
    Live(Box<super::collect::LiveCollector>),
    /// The simulator, with the instant the timeline started.
    Demo(std::time::Instant),
}

impl Source {
    pub fn collect(&mut self) -> TopSnapshot {
        match self {
            Source::Live(c) => c.collect(),
            Source::Demo(start) => super::demo::snapshot(start.elapsed().as_secs()),
        }
    }
}

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

fn restore_terminal() {
    let mut out = std::io::stdout();
    let _ = terminal::disable_raw_mode();
    let _ = execute!(out, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = out.flush();
}

/// Raw mode + alternate screen for its lifetime.
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    pub fn enter() -> Result<TerminalGuard> {
        terminal::enable_raw_mode()
            .map_err(|e| PgxError::failure(format!("cannot enter raw mode: {e}")))?;
        let guard = TerminalGuard { _private: () };
        execute!(
            std::io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide
        )
        .map_err(|e| PgxError::failure(format!("cannot open the alternate screen: {e}")))?;
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            unsafe {
                libc::signal(sig, on_signal as *const () as libc::sighandler_t);
            }
        }
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

pub fn is_tty() -> bool {
    unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
}

fn map_key(k: event::KeyEvent) -> Option<Key> {
    if k.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    Some(match k.code {
        KeyCode::Char('c') | KeyCode::Char('d') if ctrl => Key::CtrlC,
        KeyCode::Char(_) if ctrl => return None,
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Tab => Key::Tab,
        _ => return None,
    })
}

enum Ctl {
    Refresh,
}

/// Run the interactive monitor until `q`, Ctrl-C or a signal.
pub fn run(
    mut source: Source,
    view: super::tree::ViewOpts,
    color: bool,
    interval: Duration,
) -> Result<()> {
    // The first snapshot before the screen opens: a broken home fails here
    // with a normal error message instead of an empty dashboard.
    let first = source.collect();
    let (snap_tx, snap_rx) = mpsc::sync_channel::<TopSnapshot>(1);
    let (ctl_tx, ctl_rx) = mpsc::channel::<Ctl>();
    std::thread::Builder::new()
        .name("pgx-top-collector".into())
        .spawn(move || loop {
            match ctl_rx.recv_timeout(interval) {
                Ok(Ctl::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            let s = source.collect();
            if snap_tx.send(s).is_err() {
                return;
            }
        })
        .map_err(|e| PgxError::failure(format!("cannot start the collector thread: {e}")))?;

    let mut app = App::new(first, view, color, interval.as_millis() as u64);
    app.received_at = crate::fsutil::now_epoch();
    let guard = TerminalGuard::enter()?;
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut term = ratatui::Terminal::new(backend)
        .map_err(|e| PgxError::failure(format!("cannot open the terminal: {e}")))?;
    let result = (|| -> Result<()> {
        let mut dirty = true;
        let mut last_draw_sec = 0;
        loop {
            if SIGNALLED.load(Ordering::SeqCst) {
                return Ok(());
            }
            let wall = crate::fsutil::now_epoch();
            if dirty || wall != last_draw_sec {
                term.draw(|f| super::ui::render(f, &app, wall))
                    .map_err(|e| PgxError::failure(format!("terminal write failed: {e}")))?;
                dirty = false;
                last_draw_sec = wall;
            }
            if event::poll(Duration::from_millis(100))
                .map_err(|e| PgxError::failure(format!("terminal read failed: {e}")))?
            {
                match event::read()
                    .map_err(|e| PgxError::failure(format!("terminal read failed: {e}")))?
                {
                    Event::Key(k) => {
                        if let Some(key) = map_key(k) {
                            let page = term.size().map(|s| s.height as usize / 2).unwrap_or(10);
                            app.on_key(key, page);
                            dirty = true;
                        }
                    }
                    Event::Resize(_, _) => {
                        let _ = term.autoresize();
                        dirty = true;
                    }
                    _ => {}
                }
            }
            if app.quit {
                return Ok(());
            }
            if app.refresh_requested {
                app.refresh_requested = false;
                let _ = ctl_tx.send(Ctl::Refresh);
            }
            while let Ok(s) = snap_rx.try_recv() {
                app.on_snapshot(s, crate::fsutil::now_epoch());
                dirty = true;
            }
        }
    })();
    drop(term);
    drop(guard);
    // Dropping the channels stops the worker at its next wakeup; a worker
    // blocked in a slow query is abandoned (the process is about to exit).
    drop(ctl_tx);
    drop(snap_rx);
    result
}
