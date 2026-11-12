//! `pgx top`: a live, read-only terminal monitor of the PGX runtime, its
//! databases and the coding-agent sessions that own them.
//!
//! ```text
//! pgx top                           interactive (needs a terminal)
//! pgx top --demo                    deterministic simulation, no PGX needed
//! pgx top --snapshot                one collection, plain text
//! pgx --json top --snapshot         one collection, one JSON object
//! pgx --json top --demo --snapshot  the simulator's fixed fixture
//! pgx top --interval-ms N           refresh interval, 250..=10000 (default 1000)
//! pgx top --no-color                monochrome (NO_COLOR is honoured too)
//! pgx top --project <filter>        start filtered to matching projects
//! ```
//!
//! Layers: `collect` (live inputs → `model::TopSnapshot`) or `demo`
//! (simulated snapshot); `tree` (hierarchy, filter, sort); `events`
//! (transitions); `app` (selection and keys); `ui` (Ratatui); `term`
//! (terminal guard and loop). `phase` is the sidecar `pgx agent activity`
//! writes.
use crate::cli::ExitCode;
use crate::errors::{PgxError, Result};
use crate::out::Out;
use std::time::Duration;

pub mod app;
pub mod collect;
pub mod demo;
pub mod events;
pub mod model;
pub mod phase;
pub mod term;
pub mod text;
pub mod tree;
pub mod ui;

pub const MIN_INTERVAL_MS: u64 = 250;
pub const MAX_INTERVAL_MS: u64 = 10_000;
pub const DEFAULT_INTERVAL_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopOpts {
    pub demo: bool,
    pub snapshot: bool,
    pub interval_ms: u64,
    pub no_color: bool,
    pub project: Option<String>,
    pub help: bool,
}

impl Default for TopOpts {
    fn default() -> Self {
        TopOpts {
            demo: false,
            snapshot: false,
            interval_ms: DEFAULT_INTERVAL_MS,
            no_color: false,
            project: None,
            help: false,
        }
    }
}

const USAGE: &str =
    "usage: pgx top [--demo] [--snapshot] [--interval-ms N] [--no-color] [--project <filter>]";

pub fn help_text() -> String {
    format!(
        "pgx top — live, read-only monitor of the PGX runtime, its databases and agent sessions

{USAGE}

  pgx top                         interactive terminal UI (needs a TTY)
  pgx top --demo                  a simulated session with five agents; no PGX needed, nothing real touched
  pgx top --snapshot              one collection as plain text
  pgx --json top --snapshot       one collection as one JSON object (schema_version 1)
  pgx --json top --demo --snapshot  the simulator's fixed fixture (for tests)

Options
  --interval-ms N     refresh interval in milliseconds, {MIN_INTERVAL_MS}..={MAX_INTERVAL_MS} (default {DEFAULT_INTERVAL_MS})
  --no-color          monochrome (also when NO_COLOR is set)
  --project <filter>  start with only projects whose name, id prefix or path matches

Keys
  ↑↓ jk move · ←→ fold · Enter details · t tree/flat · a agents only · / search
  s sort (name, ttl, age, conn) · p pause · r refresh · ? help · q quit

What the columns mean
  DB STATE   CONNECTED (client connections now) or IDLE (none) — idle is not \"finished\"
  LIFECYCLE  ACTIVE / RETIRED (the owning session ended — not a task result) / EXPIRED
  TASK       only what an agent reported with `pgx agent activity --state …`; — otherwise
  MEM        resident memory of the whole runtime process, not per database
  SIZE       logical database size (pg_database_size), not copy-on-write disk usage

Read-only: no key drops, resets or extends anything. See docs/pgx/local-dev/top.md.
"
    )
}

/// Parse everything after `pgx top`.
pub fn parse(tail: &[&str]) -> Result<TopOpts> {
    let mut o = TopOpts::default();
    let mut i = 0;
    let bad = |m: String| PgxError::usage(format!("pgx top: {m}\n{USAGE}"));
    while i < tail.len() {
        let t = tail[i];
        let (flag, inline) = match t.strip_prefix("--") {
            Some(f) => match f.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (f, None),
            },
            None => return Err(bad(format!("unexpected argument {t:?}"))),
        };
        let mut value = |name: &str| -> Result<String> {
            if let Some(v) = inline {
                return Ok(v.to_string());
            }
            i += 1;
            tail.get(i)
                .map(|s| s.to_string())
                .ok_or_else(|| bad(format!("--{name} needs a value")))
        };
        match flag {
            "demo" if inline.is_none() => o.demo = true,
            "snapshot" if inline.is_none() => o.snapshot = true,
            "no-color" | "no-colour" if inline.is_none() => o.no_color = true,
            "help" if inline.is_none() => o.help = true,
            "interval-ms" => {
                let v = value("interval-ms")?;
                o.interval_ms = v
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(n))
                    .ok_or_else(|| {
                        bad(format!(
                            "--interval-ms must be a whole number of milliseconds between {MIN_INTERVAL_MS} and {MAX_INTERVAL_MS}, got {v:?}"
                        ))
                    })?;
            }
            "project" => {
                let v = value("project")?;
                let v = v.trim().to_string();
                if v.is_empty() || v.chars().count() > 128 || !text::is_clean(&v) {
                    return Err(bad(
                        "--project must be 1-128 printable characters".to_string()
                    ));
                }
                o.project = Some(v);
            }
            other => return Err(bad(format!("unknown option --{other}"))),
        }
        i += 1;
    }
    Ok(o)
}

/// Collect once from the chosen source, without a terminal.
pub fn snapshot_once(demo: bool) -> model::TopSnapshot {
    if demo {
        demo::snapshot(demo::SNAPSHOT_T)
    } else {
        collect::LiveCollector::new().collect()
    }
}

pub fn run(out: &Out, opts: TopOpts) -> Result<ExitCode> {
    if opts.help {
        print!("{}", help_text());
        return Ok(ExitCode::Ok);
    }
    if out.json && !opts.snapshot {
        return Err(PgxError::usage(
            "pgx --json top needs --snapshot: the interactive view is not machine-readable",
        )
        .hint("`pgx --json top --snapshot` (or `--demo --snapshot` for the fixed fixture)"));
    }
    if opts.snapshot {
        let s = snapshot_once(opts.demo);
        let s = match &opts.project {
            Some(p) => filter_project(s, p),
            None => s,
        };
        out.emit(ui::plain(&s).trim_end(), s.to_json());
        return Ok(match (s.mode, s.runtime.status) {
            (model::Mode::Demo, _) | (_, model::RuntimeStatus::Running) => ExitCode::Ok,
            _ => ExitCode::NotReady,
        });
    }
    if !term::is_tty() {
        return Err(PgxError::usage(
            "pgx top is interactive and needs a terminal (stdin and stdout must be a TTY)",
        )
        .hint("for scripts and CI use `pgx --json top --snapshot`, or `pgx --json top --demo --snapshot` for the fixture"));
    }
    let source = if opts.demo {
        term::Source::Demo(std::time::Instant::now())
    } else {
        let home = crate::layout::Home::resolve();
        if !home.is_initialized() {
            return Err(PgxError::new(
                ExitCode::NotReady,
                format!("PGX is not initialised at {}", home.root.display()),
            )
            .hint("run `pgx init` (or `pgx run`) first, or try `pgx top --demo`"));
        }
        crate::cmd::Ctx::load()?;
        term::Source::Live(Box::default())
    };
    let color = !opts.no_color && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
    let view = tree::ViewOpts {
        project: opts.project.clone(),
        ..Default::default()
    };
    term::run(source, view, color, Duration::from_millis(opts.interval_ms))?;
    Ok(ExitCode::Ok)
}

/// `--project` for the snapshot form: keep only matching databases.
fn filter_project(mut s: model::TopSnapshot, p: &str) -> model::TopSnapshot {
    if let Some(dbs) = s.databases.take() {
        let view = tree::ViewOpts {
            project: Some(p.to_string()),
            ..Default::default()
        };
        let keep: std::collections::HashSet<usize> =
            tree::filtered(&dbs, &view).into_iter().collect();
        let kept: Vec<model::Database> = dbs
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i))
            .map(|(_, d)| d)
            .collect();
        // Warnings about a database outside the filter go with it.
        let names: std::collections::HashSet<&str> = kept.iter().map(|d| d.name.as_str()).collect();
        s.warnings
            .retain(|w| w.database.as_deref().is_none_or(|n| names.contains(n)));
        s.databases = Some(kept);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<TopOpts> {
        let v: Vec<&str> = s.split_whitespace().collect();
        parse(&v)
    }

    #[test]
    fn parses_flags_and_interval_bounds() {
        assert_eq!(p("").unwrap(), TopOpts::default());
        let o = p("--demo --snapshot --no-color --project remotely --interval-ms 250").unwrap();
        assert!(o.demo && o.snapshot && o.no_color);
        assert_eq!(o.project.as_deref(), Some("remotely"));
        assert_eq!(o.interval_ms, 250);
        assert_eq!(p("--interval-ms=10000").unwrap().interval_ms, 10_000);
        assert_eq!(p("--project=a").unwrap().project.as_deref(), Some("a"));
        for bad in [
            "--interval-ms 0",
            "--interval-ms -5",
            "--interval-ms 249",
            "--interval-ms 10001",
            "--interval-ms 1e3",
            "--interval-ms 1000ms",
            "--interval-ms",
            "--interval-ms=",
            "--bogus",
            "extra",
            "--demo=yes",
            "--project",
            "--project=",
        ] {
            assert_eq!(p(bad).unwrap_err().code, ExitCode::Usage, "{bad}");
        }
        let long = format!("--project={}", "x".repeat(129));
        assert!(parse(&[long.as_str()]).is_err());
        assert!(parse(&["--project", "\x1b[31m"]).is_err());
        assert!(p("--help").unwrap().help);
    }

    #[test]
    fn json_without_snapshot_is_a_usage_error() {
        let out = Out::new(true);
        let e = run(
            &out,
            TopOpts {
                demo: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(e.code, ExitCode::Usage);
        assert!(e.message.contains("--snapshot"));
    }

    #[test]
    fn help_describes_semantics() {
        let h = help_text();
        for want in [
            "--demo",
            "--snapshot",
            "--interval-ms",
            "--no-color",
            "--project",
            "Read-only",
            "not a task result",
            "logical",
        ] {
            assert!(h.contains(want), "{want}");
        }
    }

    #[test]
    fn project_filter_applies_to_snapshots() {
        let s = demo::snapshot(demo::SNAPSHOT_T);
        let all = s.databases.as_ref().unwrap().len();
        let f = filter_project(s.clone(), "remotely");
        assert_eq!(
            f.databases.as_ref().unwrap().len(),
            all - 1,
            "the standalone template has no project"
        );
        assert_eq!(filter_project(s, "nothing").databases.unwrap().len(), 0);
    }
}
