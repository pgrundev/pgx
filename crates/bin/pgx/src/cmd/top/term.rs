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

    /// One collection that survives a panic in the collector: the state is
    /// reset (a fresh connection next time) and the panic becomes an ISSUE
    /// in a snapshot whose catalog is unknown — so it can never read as
    /// "every database dropped".
    pub fn collect_guarded(&mut self) -> TopSnapshot {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.collect())) {
            Ok(s) => s,
            Err(p) => {
                if let Source::Live(c) = self {
                    **c = super::collect::LiveCollector::new();
                }
                crash_snapshot(panic_message(&*p))
            }
        }
    }
}

pub const COLLECTOR_THREAD: &str = "pgx-top-collector";

fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

pub fn crash_snapshot(msg: String) -> TopSnapshot {
    use super::model::*;
    TopSnapshot {
        mode: Mode::Live,
        collected_at: crate::fsutil::now_epoch(),
        runtime: Runtime::empty(RuntimeStatus::Error),
        databases: None,
        warnings: vec![Warning::issue(
            "collector_panicked",
            format!(
                "the collector failed and was restarted: {}",
                super::text::sanitize(&msg)
            ),
        )],
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
            // A collector panic is caught on its thread and shown as an
            // ISSUE; the UI keeps the terminal. Anything else restores it.
            if std::thread::current().name() == Some(COLLECTOR_THREAD) {
                return;
            }
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
        .name(COLLECTOR_THREAD.into())
        .spawn(move || loop {
            match ctl_rx.recv_timeout(interval) {
                Ok(Ctl::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            let s = source.collect_guarded();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_is_an_issue_with_an_unknown_catalog() {
        let s = crash_snapshot("index out of bounds \x1b[2J".into());
        assert_eq!(s.databases, None, "unknown, never 'all dropped'");
        assert_eq!(s.summary().issues, 1);
        assert!(!s.warnings[0].message.contains('\x1b'));
        let mut log = super::super::events::EventLog::default();
        let mut ok =
            super::super::model::fixtures::snap(vec![super::super::model::fixtures::standalone(
                5, "postgres",
            )]);
        log.observe(&ok);
        log.observe(&s);
        ok.collected_at += 2;
        log.observe(&ok);
        let t: Vec<String> = log.recent().map(|e| e.text.clone()).collect();
        assert!(t.iter().all(|e| !e.contains("dropped")), "{t:?}");
    }

    #[test]
    fn panic_messages_are_extracted() {
        let r = std::panic::catch_unwind(|| panic!("boom {}", 1));
        assert_eq!(panic_message(&*r.unwrap_err()), "boom 1");
        let r = std::panic::catch_unwind(|| panic!("static"));
        assert_eq!(panic_message(&*r.unwrap_err()), "static");
    }
}
