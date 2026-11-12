//! Rendering for `pgx top`: one dense screen, htop-style.
//!
//! ```text
//! header (runtime, memory, counters)       1 line when compact, 4 otherwise
//! table  (tree or flat, scrolls to the selection)
//! inspector (Enter)                        optional
//! recent events                            when there is room
//! footer (keys / search line)
//! ```
//!
//! Every cell is built with `text::fit`, so a label can never be wider than
//! its column; the layout degrades by dropping columns and panes, never by
//! overflowing. Labels were sanitized by the model; `fit` sanitizes again.
use super::app::App;
use super::model::*;
use super::text::{
    bytes, clip, clock_secs, compact_secs, fit, fit_right, local_hms, sanitize, width,
};
use super::tree::{db_label, host_label, GroupKind, Row, RowKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

#[derive(Clone, Copy)]
pub struct Theme {
    pub color: bool,
}

impl Theme {
    fn fg(self, c: Color) -> Style {
        if self.color {
            Style::default().fg(c)
        } else {
            Style::default()
        }
    }
    fn dim(self) -> Style {
        if self.color {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        }
    }
    fn bold(self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }
    fn badge(self, c: Color) -> Style {
        if self.color {
            Style::default()
                .fg(Color::Black)
                .bg(c)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }
    fn db_state(self, s: DbState) -> Style {
        match s {
            DbState::Connected => self.fg(Color::Green),
            DbState::Idle | DbState::Unknown => self.dim(),
            DbState::Expiring => self.fg(Color::Yellow),
            DbState::DeleteRetry | DbState::Invalid => self.fg(Color::Red),
        }
    }
    fn phase(self, p: Phase) -> Style {
        match p {
            Phase::Working => self.fg(Color::Cyan),
            Phase::Migrating => self.fg(Color::Magenta),
            Phase::Testing => self.fg(Color::Yellow),
            Phase::Completed => self.fg(Color::Green),
            Phase::Failed => self.fg(Color::Red),
        }
    }
    fn lifecycle(self, l: Lifecycle) -> Style {
        match l {
            Lifecycle::Active => self.fg(Color::Green),
            Lifecycle::Retired => self.dim(),
            Lifecycle::Expired => self.fg(Color::Yellow),
        }
    }
}

/// The time the screen shows: the snapshot's clock advanced by the wall time
/// since it arrived, so TTLs count down smoothly between refreshes (and the
/// simulator's clock works the same way).
pub fn display_now(app: &App, wall: u64) -> u64 {
    app.snapshot
        .collected_at
        .saturating_add(wall.saturating_sub(app.received_at))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Density {
    /// Below 60 columns or 16 lines: one header line, three columns, no events.
    Compact,
    Normal,
    /// 120 columns and more: lifecycle, host, size and last-touch columns.
    Wide,
}

pub fn density(area: Rect) -> Density {
    if area.width < 60 || area.height < 16 {
        Density::Compact
    } else if area.width >= 120 {
        Density::Wide
    } else {
        Density::Normal
    }
}

struct Col {
    title: &'static str,
    width: usize,
    right: bool,
}

fn columns(d: Density, total: usize) -> (usize, Vec<Col>) {
    let c = |title, width, right| Col {
        title,
        width,
        right,
    };
    let cols = match d {
        Density::Compact => vec![c("STATE", 9, false), c("TTL", 6, true)],
        Density::Normal => vec![
            c("DB STATE", 9, false),
            c("TASK", 9, false),
            c("CONN", 4, true),
            c("AGE", 5, true),
            c("TTL", 6, true),
        ],
        Density::Wide => vec![
            c("DB STATE", 9, false),
            c("LIFECYCLE", 9, false),
            c("TASK", 9, false),
            c("HOST", 11, false),
            c("CONN", 4, true),
            c("SIZE", 9, true),
            c("AGE", 5, true),
            c("TOUCH", 6, true),
            c("TTL", 6, true),
        ],
    };
    let fixed: usize = cols.iter().map(|c| c.width + 1).sum();
    (total.saturating_sub(fixed + 1).max(8), cols)
}

fn ttl_cell(d: &Database, now: u64) -> String {
    match d.remaining(now) {
        None => "∞".into(),
        Some(0) => "due".into(),
        Some(r) => compact_secs(r),
    }
}

fn age(t: Option<u64>, now: u64) -> String {
    t.map(|t| compact_secs(now.saturating_sub(t)))
        .unwrap_or_else(|| "—".into())
}

/// Spans for one table row, exactly `total` columns wide.
fn row_spans(
    app: &App,
    row: &Row,
    d: Density,
    total: usize,
    now: u64,
    th: Theme,
) -> Vec<Span<'static>> {
    let (name_w, cols) = columns(d, total);
    let indent = |depth: usize| "  ".repeat(depth);
    let mut spans = vec![Span::raw(" ")];
    match &row.kind {
        RowKind::Group {
            kind,
            label,
            dbs,
            conns,
            collapsed,
            has_session_db,
        } => {
            let arrow = if *collapsed { "▸ " } else { "▾ " };
            let mut l = format!("{}{arrow}{}", indent(row.depth), sanitize(label));
            if *kind == GroupKind::Session && !has_session_db {
                l.push_str("  (no session db)");
            }
            spans.push(Span::styled(fit(&l, name_w), th.bold()));
            for c in &cols {
                spans.push(Span::raw(" "));
                let v = match c.title {
                    "STATE" | "DB STATE" => format!("{dbs} DB"),
                    "CONN" => conns.map(|c| c.to_string()).unwrap_or_else(|| "n/a".into()),
                    _ => String::new(),
                };
                let cell = if c.right {
                    fit_right(&v, c.width)
                } else {
                    fit(&v, c.width)
                };
                spans.push(Span::styled(cell, th.dim()));
            }
        }
        RowKind::Db(i) => {
            let db = &app.databases()[*i];
            let prefix = if row.depth == 0 {
                String::new()
            } else {
                format!(
                    "{}{}",
                    indent(row.depth),
                    if row.last { "└─ " } else { "├─ " }
                )
            };
            let label = if app.view.tree {
                db_label(db)
            } else {
                sanitize(&db.name)
            };
            spans.push(Span::raw(fit(&format!("{prefix}{label}"), name_w)));
            for c in &cols {
                spans.push(Span::raw(" "));
                let (v, style) = match c.title {
                    "STATE" | "DB STATE" => {
                        (db.db_state.label().to_string(), th.db_state(db.db_state))
                    }
                    "LIFECYCLE" => match db.lifecycle {
                        Some(l) => (l.label().to_string(), th.lifecycle(l)),
                        None => ("—".into(), th.dim()),
                    },
                    "TASK" => match &db.phase {
                        Some(p) => (p.phase.label().to_string(), th.phase(p.phase)),
                        None => ("—".into(), th.dim()),
                    },
                    "HOST" => (
                        db.agent
                            .as_ref()
                            .map(|a| host_label(&a.host))
                            .unwrap_or_else(|| "—".into()),
                        th.dim(),
                    ),
                    "CONN" => (
                        db.connections
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "n/a".into()),
                        Style::default(),
                    ),
                    "SIZE" => (
                        db.size_bytes.map(bytes).unwrap_or_else(|| "n/a".into()),
                        th.dim(),
                    ),
                    "AGE" => (age(db.created_at, now), Style::default()),
                    "TOUCH" => (
                        age(db.agent.as_ref().and_then(|a| a.last_touch), now),
                        th.dim(),
                    ),
                    "TTL" => {
                        let s = if db.deadline_kind == Some(DeadlineKind::Grace) {
                            th.dim()
                        } else {
                            Style::default()
                        };
                        let s = if db.remaining(now) == Some(0) {
                            th.fg(Color::Yellow)
                        } else {
                            s
                        };
                        (ttl_cell(db, now), s)
                    }
                    _ => (String::new(), Style::default()),
                };
                let cell = if c.right {
                    fit_right(&v, c.width)
                } else {
                    fit(&v, c.width)
                };
                spans.push(Span::styled(cell, style));
            }
        }
    }
    spans
}

fn line_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| width(&s.content)).sum()
}

fn pad_line(mut spans: Vec<Span<'static>>, total: usize) -> Vec<Span<'static>> {
    let w = line_width(&spans);
    if w < total {
        spans.push(Span::raw(" ".repeat(total - w)));
    }
    spans
}

fn status_badge(app: &App, wall: u64, th: Theme) -> (String, Style) {
    let rt = app.snapshot.runtime.status;
    if app.paused {
        return ("❚❚ PAUSED".into(), th.fg(Color::Yellow));
    }
    match rt {
        RuntimeStatus::Running => {
            let age = wall.saturating_sub(app.received_at);
            let iv = (app.interval_ms as f64 / 1000.0).max(0.25);
            let stale = age as f64 > iv * 3.0 + 2.0;
            if stale {
                (
                    format!("◌ STALE {}", compact_secs(age)),
                    th.fg(Color::Yellow),
                )
            } else {
                let i = if app.interval_ms.is_multiple_of(1000) {
                    format!("{}s", app.interval_ms / 1000)
                } else {
                    format!("{:.2}s", app.interval_ms as f64 / 1000.0)
                };
                (format!("● LIVE {i}"), th.fg(Color::Green))
            }
        }
        other => (format!("○ {}", other.label()), th.fg(Color::Red)),
    }
}

fn header(app: &App, area: Rect, d: Density, wall: u64, th: Theme) -> Vec<Line<'static>> {
    let w = area.width as usize;
    let s = &app.snapshot;
    let sum = s.summary();
    let rt = &s.runtime;
    let demo = s.mode == Mode::Demo;
    let (badge, badge_style) = status_badge(app, wall, th);
    let opt = |v: Option<usize>| v.map(|v| v.to_string()).unwrap_or_else(|| "n/a".into());
    if d == Density::Compact {
        let mut spans = vec![Span::styled(" PGX TOP", th.bold())];
        if demo {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("DEMO", th.badge(Color::Yellow)));
        }
        spans.push(Span::raw(" "));
        spans.push(Span::styled(badge, badge_style));
        let tail = format!(
            " DB {} C {} !{}",
            opt(sum.databases),
            sum.client_connections
                .map(|c| c.to_string())
                .unwrap_or_else(|| "n/a".into()),
            sum.issues
        );
        let room = w.saturating_sub(line_width(&spans));
        spans.push(Span::raw(clip(&tail, room)));
        return vec![Line::from(pad_line(spans, w))];
    }
    let mut l1 = vec![Span::styled(" PGX TOP", th.bold())];
    if demo {
        l1.push(Span::raw("  "));
        l1.push(Span::styled(" DEMO / SIMULATED ", th.badge(Color::Yellow)));
    }
    let right = format!("{badge}   PGX {} ", env!("CARGO_PKG_VERSION"));
    let gap = w.saturating_sub(line_width(&l1) + width(&right));
    l1.push(Span::raw(" ".repeat(gap)));
    l1.push(Span::styled(
        clip(&right, w.saturating_sub(line_width(&l1))),
        badge_style,
    ));
    let na = || "n/a".to_string();
    let l2 = format!(
        " Runtime: {}  PID {}  Port {}   Uptime {}",
        rt.status.label(),
        rt.pid.map(|p| p.to_string()).unwrap_or_else(na),
        rt.port.map(|p| p.to_string()).unwrap_or_else(na),
        rt.uptime_seconds.map(clock_secs).unwrap_or_else(na),
    );
    let bar_w = (w / 4).clamp(10, 30);
    let filled = match (rt.rss_bytes, rt.phys_mem_bytes) {
        (Some(r), Some(p)) if p > 0 => ((r as f64 / p as f64) * bar_w as f64).ceil() as usize,
        _ => 0,
    }
    .min(bar_w);
    let mem = rt.rss_bytes.map(bytes).unwrap_or_else(na);
    let l3 = vec![
        Span::raw(" MEM ["),
        Span::styled("█".repeat(filled), th.fg(Color::Cyan)),
        Span::styled("░".repeat(bar_w - filled), th.dim()),
        Span::raw(format!("] {mem} RSS (whole runtime)   CPU n/a")),
    ];
    let view = format!(
        "sort:{} {}{}{}",
        app.view.sort.label(),
        if app.view.tree { "tree" } else { "flat" },
        if app.view.agents_only {
            " agents-only"
        } else {
            ""
        },
        if app.view.query.is_empty() {
            String::new()
        } else {
            format!(" /{}", sanitize(&app.view.query))
        },
    );
    let mut l4 = vec![Span::raw(format!(
        " DATABASES {}   AGENT DBS {}   SESSIONS {}   CONNS {}   ",
        opt(sum.databases),
        opt(sum.agent_databases),
        opt(sum.sessions),
        sum.client_connections
            .map(|c| c.to_string())
            .unwrap_or_else(na),
    ))];
    l4.push(Span::styled(
        format!("ISSUES {}", sum.issues),
        if sum.issues > 0 {
            th.fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    ));
    let room = w.saturating_sub(line_width(&l4) + 2);
    if room > 6 {
        let v = clip(&view, room);
        l4.push(Span::raw(" ".repeat(room - width(&v) + 1)));
        l4.push(Span::styled(v, th.dim()));
    }
    let fitline = |spans: Vec<Span<'static>>| -> Line<'static> {
        // Clip span by span so a narrow terminal never wraps a header line.
        let mut out = Vec::new();
        let mut left = w;
        for s in spans {
            if left == 0 {
                break;
            }
            let c = clip(&s.content, left);
            left -= width(&c);
            out.push(Span::styled(c, s.style));
        }
        Line::from(pad_line(out, w))
    };
    vec![
        fitline(l1),
        fitline(vec![Span::raw(l2)]),
        fitline(l3),
        fitline(l4),
        Line::styled("─".repeat(w), th.dim()),
    ]
}

fn table_header(d: Density, w: usize, th: Theme) -> Line<'static> {
    let (name_w, cols) = columns(d, w);
    let mut s = format!(" {}", fit("PROJECT / SESSION / DATABASE", name_w));
    for c in &cols {
        s.push(' ');
        s.push_str(&if c.right {
            fit_right(c.title, c.width)
        } else {
            fit(c.title, c.width)
        });
    }
    Line::styled(fit(&s, w), th.dim().add_modifier(Modifier::BOLD))
}

fn empty_state(app: &App) -> Vec<String> {
    let s = &app.snapshot;
    let first_issue = s
        .warnings
        .iter()
        .find(|w| w.severity == Severity::Issue)
        .map(|w| sanitize(&w.message));
    match (&s.databases, s.runtime.status) {
        (None, RuntimeStatus::NotInitialized) => vec![
            "PGX is not initialised here.".into(),
            "Run `pgx init` (or `pgx run`); this view updates on its own.".into(),
        ],
        (None, RuntimeStatus::Stopped) => vec![
            "The PGX runtime is not running.".into(),
            "Start it with `pgx start`; this view reconnects on its own.".into(),
        ],
        (None, _) => {
            let mut v = vec!["Runtime unreachable — retrying every refresh.".to_string()];
            v.extend(first_issue);
            v
        }
        (Some(d), _) if d.is_empty() => vec!["No databases yet.".into()],
        (Some(_), _) => vec![
            format!(
                "No databases match{}{}.",
                if app.view.query.is_empty() {
                    String::new()
                } else {
                    format!(" /{}", sanitize(&app.view.query))
                },
                if app.view.agents_only {
                    " (agents only)"
                } else {
                    ""
                }
            ),
            "Esc clears the search; `a` shows all databases.".into(),
        ],
    }
}

fn kv(k: &str, v: String) -> (String, String) {
    (k.to_string(), v)
}

fn inspector_fields(app: &App, row: &Row, now: u64) -> Vec<(String, String)> {
    let dash = || "—".to_string();
    match &row.kind {
        RowKind::Group {
            kind,
            label,
            dbs,
            conns,
            has_session_db,
            ..
        } => {
            let mut v = vec![
                kv("group", sanitize(label)),
                kv("kind", format!("{kind:?}").to_lowercase()),
                kv("databases", dbs.to_string()),
                kv(
                    "connections",
                    conns.map(|c| c.to_string()).unwrap_or_else(|| "n/a".into()),
                ),
                kv("id", sanitize(&row.id)),
            ];
            if *kind == GroupKind::Session && !has_session_db {
                v.push(kv(
                    "session db",
                    "none (expired or dropped); subagents shown under a synthetic group".into(),
                ));
            }
            v
        }
        RowKind::Db(i) => {
            let d = &app.databases()[*i];
            let a = d.agent.as_ref();
            let rfc = |t: u64| crate::fsutil::rfc3339(t);
            let mut v = vec![
                kv("database", format!("{} (oid {})", sanitize(&d.name), d.oid)),
                kv("role", d.role.as_str().to_string()),
            ];
            if let Some(a) = a {
                v.push(kv(
                    "project",
                    format!(
                        "{} ({})",
                        sanitize(&a.project_name),
                        super::tree::short(&a.project_id, 12)
                    ),
                ));
                v.push(kv(
                    "session",
                    format!("{} · {}", host_label(&a.host), sanitize(&a.session_id)),
                ));
                v.push(kv(
                    "agent",
                    a.agent_id
                        .as_deref()
                        .map(sanitize)
                        .unwrap_or_else(|| "— (the session itself)".into()),
                ));
                v.push(kv(
                    "git",
                    match (&a.git_branch, &a.worktree) {
                        (Some(b), Some(w)) => format!("{} @ {}", sanitize(b), sanitize(w)),
                        (Some(b), None) => sanitize(b),
                        _ => dash(),
                    },
                ));
                v.push(kv("source", sanitize(&a.source)));
                v.push(kv(
                    "last touch",
                    a.last_touch
                        .map(|t| {
                            format!(
                                "{} ago (hook heartbeat, not SQL activity)",
                                compact_secs(now.saturating_sub(t))
                            )
                        })
                        .unwrap_or_else(dash),
                ));
            } else if let Some(g) = &d.git {
                v.push(kv(
                    "git",
                    format!(
                        "{} · {} @ {}",
                        sanitize(&g.repo_name),
                        sanitize(&g.branch),
                        sanitize(&g.worktree_path)
                    ),
                ));
            }
            v.push(kv(
                "created",
                d.created_at
                    .map(|c| format!("{} ago ({})", compact_secs(now.saturating_sub(c)), rfc(c)))
                    .unwrap_or_else(dash),
            ));
            v.push(kv(
                "ttl",
                match d.expires_at {
                    None => format!(
                        "{} (no deadline)",
                        d.ttl_label.clone().unwrap_or_else(|| "unlimited".into())
                    ),
                    Some(e) => format!(
                        "{} left · {} · until {}",
                        ttl_cell(d, now),
                        match d.deadline_kind {
                            Some(DeadlineKind::Grace) => "grace (session ended)",
                            Some(DeadlineKind::IdleTtl) => "idle TTL after last touch",
                            _ => "fixed deadline",
                        },
                        local_hms(e)
                    ),
                },
            ));
            v.push(kv(
                "lifecycle",
                d.lifecycle
                    .map(|l| l.label().to_string())
                    .unwrap_or_else(dash),
            ));
            v.push(kv(
                "db state",
                format!(
                    "{} · {} client connection(s)",
                    d.db_state.label(),
                    d.connections
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "n/a".into())
                ),
            ));
            v.push(kv(
                "size",
                d.size_bytes
                    .map(|b| format!("{} logical (not COW disk usage)", bytes(b)))
                    .unwrap_or_else(|| "n/a".into()),
            ));
            v.push(kv(
                "task",
                match &d.phase {
                    Some(p) => format!(
                        "{}{} · {} ago · {}",
                        p.phase.label(),
                        p.label
                            .as_deref()
                            .map(|l| format!(" \"{}\"", sanitize(l)))
                            .unwrap_or_default(),
                        compact_secs(now.saturating_sub(p.reported_at)),
                        sanitize(&p.source)
                    ),
                    None => "— (no explicit report)".into(),
                },
            ));
            v.push(kv(
                "last error",
                d.last_error.as_deref().map(sanitize).unwrap_or_else(dash),
            ));
            v
        }
    }
}

fn inspector(app: &App, area: Rect, now: u64, th: Theme) -> Vec<Line<'static>> {
    let w = area.width as usize;
    let h = area.height as usize;
    let mut lines = vec![Line::styled(fit(" DETAILS  (Esc closes)", w), th.bold())];
    let Some(row) = app.selected_row() else {
        lines.push(Line::raw(fit("  nothing selected", w)));
        return lines;
    };
    let f = inspector_fields(app, row, now);
    let two = w >= 80;
    // The right column carries the long values (deadline, task): give it more.
    let left_w = if two { w * 9 / 20 } else { w };
    let key_w = 11;
    let cell = |(k, v): &(String, String), col_w: usize| -> Vec<Span<'static>> {
        vec![
            Span::styled(fit(&format!("  {k}"), key_w + 2), th.dim()),
            Span::raw(fit(v, col_w.saturating_sub(key_w + 2))),
        ]
    };
    let body = h.saturating_sub(1);
    if two {
        let per = f.len().div_ceil(2);
        for i in 0..per.min(body) {
            let mut spans = cell(&f[i], left_w);
            if let Some(r) = f.get(i + per) {
                spans.extend(cell(r, w - left_w));
            }
            lines.push(Line::from(pad_line(spans, w)));
        }
    } else {
        for item in f.iter().take(body) {
            lines.push(Line::from(pad_line(cell(item, w), w)));
        }
    }
    lines
}

fn events(app: &App, area: Rect, th: Theme) -> Vec<Line<'static>> {
    let w = area.width as usize;
    let mut lines = vec![
        Line::styled("─".repeat(w), th.dim()),
        Line::styled(fit(" RECENT EVENTS", w), th.bold()),
    ];
    let n = (area.height as usize).saturating_sub(2);
    if app.events.is_empty() {
        lines.push(Line::styled(
            fit("  (none yet: changes appear here as they happen)", w),
            th.dim(),
        ));
    }
    for e in app.events.recent().take(n) {
        let style = match e.glyph {
            "+" | "●" | "▲" => th.fg(Color::Green),
            "-" | "!" | "○" => th.fg(Color::Red),
            _ => Style::default(),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", local_hms(e.at)), th.dim()),
            Span::styled(format!("{} ", e.glyph), style),
            Span::raw(fit(&e.text, w.saturating_sub(12))),
        ]));
    }
    lines
}

fn footer(app: &App, w: usize, d: Density, th: Theme) -> Line<'static> {
    if app.editing.is_some() {
        let q = sanitize(&app.view.query);
        let hint = "  Enter apply  Esc cancel";
        let room = w.saturating_sub(4 + width(hint));
        // Show the end of a long query, where the cursor is.
        let shown: String = {
            let chars: Vec<char> = q.chars().collect();
            let mut out = String::new();
            let mut used = 0;
            for c in chars.iter().rev() {
                let cw = width(&c.to_string());
                if used + cw > room {
                    break;
                }
                out.insert(0, *c);
                used += cw;
            }
            out
        };
        return Line::from(pad_line(
            vec![
                Span::styled(" /", th.bold()),
                Span::raw(shown),
                Span::styled("█", th.fg(Color::Cyan)),
                Span::styled(clip(hint, w.saturating_sub(3 + room)), th.dim()),
            ],
            w,
        ));
    }
    let keys = match d {
        Density::Compact => " jk ⏎ t a / s p ? q",
        Density::Normal => " ↑↓ move  ⏎ info  t tree  a agents  / find  s sort  p pause  ? help  q quit",
        Density::Wide => " ↑↓/jk move  ←→ fold  Enter details  t tree/flat  a agents  / search  s sort  p pause  r refresh  ? help  q quit",
    };
    Line::styled(fit(keys, w), th.dim().add_modifier(Modifier::REVERSED))
}

pub const HELP: &[(&str, &str)] = &[
    ("↑ ↓  j k", "move the selection"),
    ("PgUp PgDn g G", "page / first / last"),
    (
        "← →  - +",
        "collapse / expand (← on a row selects its group)",
    ),
    ("Space Tab", "toggle the selected group"),
    ("Enter", "details of the selection (Esc closes)"),
    ("t", "tree ⇄ flat list"),
    ("a", "all databases ⇄ agent-owned only"),
    ("/", "search (Enter keeps, Esc cancels)"),
    ("s", "sort: name → ttl → age → conn"),
    ("p", "pause the display (PGX keeps running)"),
    ("r", "refresh now"),
    ("q  Ctrl-C", "quit"),
    ("", ""),
    ("read-only", "no key changes a database, a TTL or an agent"),
    ("DB STATE", "connections now; IDLE does not mean finished"),
    ("TASK", "only what agents reported (pgx agent activity)"),
];

fn help_overlay(f: &mut Frame, area: Rect, th: Theme) {
    let w = area.width.saturating_sub(4).min(76);
    let h = (HELP.len() as u16 + 2).min(area.height.saturating_sub(2));
    if w < 20 || h < 4 {
        return;
    }
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    let inner = w as usize - 2;
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(fit(&format!(" {k}"), 16.min(inner)), th.bold()),
                Span::raw(fit(v, inner.saturating_sub(16))),
            ])
        })
        .collect();
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" pgx top — keys (any key closes) "),
        ),
        r,
    );
}

/// Draw one frame. `wall` is the current epoch second.
pub fn render(f: &mut Frame, app: &App, wall: u64) {
    let area = f.area();
    let th = Theme { color: app.color };
    let w = area.width as usize;
    if area.width < 20 || area.height < 4 {
        f.render_widget(Paragraph::new(clip("pgx top: terminal too small", w)), area);
        return;
    }
    let now = display_now(app, wall);
    let d = density(area);
    let head = header(app, area, d, wall, th);
    let head_h = head.len() as u16;
    let foot_h = 1u16;
    // The inspector takes the events pane's room on short terminals.
    let events_h: u16 =
        if d == Density::Compact || area.height < 20 || (app.inspector && area.height < 32) {
            0
        } else {
            (3 + (area.height.saturating_sub(24)) / 4).min(10) + 2
        };
    let insp_h: u16 = match (app.inspector, d) {
        (false, _) => 0,
        (true, Density::Compact) => 5,
        (true, _) if w >= 80 => 9,
        (true, _) => 16,
    };
    let mut y = area.y;
    let mut rect = |h: u16| {
        let r = Rect {
            x: area.x,
            y,
            width: area.width,
            height: h,
        };
        y += h;
        r
    };
    let head_r = rect(head_h);
    let avail = area.height.saturating_sub(head_h + foot_h + events_h);
    let insp_h = insp_h.min(avail.saturating_sub(3));
    let table_r = rect(avail.saturating_sub(insp_h));
    let insp_r = rect(insp_h);
    let events_r = rect(events_h);
    let foot_r = rect(foot_h);
    f.render_widget(Paragraph::new(head), head_r);

    // Table.
    let mut lines = Vec::new();
    let mut body_h = table_r.height as usize;
    if d != Density::Compact && body_h > 1 {
        lines.push(table_header(d, w, th));
        body_h -= 1;
    }
    let rows = app.rows();
    if rows.is_empty() {
        for m in empty_state(app) {
            lines.push(Line::styled(fit(&format!("  {m}"), w), th.dim()));
        }
    } else {
        let sel = app.selected_index().unwrap_or(0);
        let offset = if sel >= body_h { sel + 1 - body_h } else { 0 };
        for (i, row) in rows.iter().enumerate().skip(offset).take(body_h) {
            let spans = pad_line(row_spans(app, row, d, w, now, th), w);
            let mut line = Line::from(spans);
            if i == sel {
                line = line.patch_style(Style::default().add_modifier(Modifier::REVERSED));
            }
            lines.push(line);
        }
    }
    f.render_widget(Paragraph::new(lines), table_r);
    if insp_h > 0 {
        f.render_widget(Paragraph::new(inspector(app, insp_r, now, th)), insp_r);
    }
    if events_h > 0 {
        f.render_widget(Paragraph::new(events(app, events_r, th)), events_r);
    }
    f.render_widget(Paragraph::new(footer(app, w, d, th)), foot_r);
    if app.help {
        help_overlay(f, area, th);
    }
}

/// The non-interactive text form of a snapshot (`pgx top --snapshot` without
/// `--json`): the same tree, plain text, no escape sequences.
pub fn plain(s: &TopSnapshot) -> String {
    let app = App::new(s.clone(), Default::default(), false, 1000);
    let now = s.collected_at;
    let sum = s.summary();
    let opt = |v: Option<usize>| v.map(|v| v.to_string()).unwrap_or_else(|| "n/a".into());
    let mut out = format!(
        "PGX TOP{}  runtime {}  pid {}  port {}  rss {}\nDATABASES {}  AGENT DBS {}  SESSIONS {}  CONNS {}  ISSUES {}\n",
        if s.mode == Mode::Demo { "  [DEMO / SIMULATED]" } else { "" },
        s.runtime.status.label(),
        s.runtime.pid.map(|p| p.to_string()).unwrap_or_else(|| "n/a".into()),
        s.runtime.port.map(|p| p.to_string()).unwrap_or_else(|| "n/a".into()),
        s.runtime.rss_bytes.map(bytes).unwrap_or_else(|| "n/a".into()),
        opt(sum.databases),
        opt(sum.agent_databases),
        opt(sum.sessions),
        sum.client_connections.map(|c| c.to_string()).unwrap_or_else(|| "n/a".into()),
        sum.issues
    );
    let th = Theme { color: false };
    for row in app.rows() {
        let spans = row_spans(&app, row, Density::Wide, 140, now, th);
        let line: String = spans.iter().map(|s| s.content.as_ref()).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if app.rows().is_empty() {
        for m in empty_state(&app) {
            out.push_str(&m);
            out.push('\n');
        }
    }
    for w in &s.warnings {
        out.push_str(&format!(
            "{} {}\n",
            if w.severity == Severity::Issue {
                "ISSUE"
            } else {
                "note "
            },
            sanitize(&w.message)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::app::Key;
    use super::super::model::fixtures::*;
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn screen(app: &App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| render(f, app, app.received_at + 1)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    fn rich() -> Vec<Database> {
        let mut v = vec![agent_db(
            100,
            "agent-claude-72ac4f1e",
            "remotely",
            "claude",
            "72ac4f1e-0000",
            None,
        )];
        for (i, a) in ["agent-a", "agent-b", "agent-c", "agent-d", "agent-e"]
            .iter()
            .enumerate()
        {
            let mut d = agent_db(
                101 + i as u64,
                &format!("agent-claude-72ac4f1e-{a}"),
                "remotely",
                "claude",
                "72ac4f1e-0000",
                Some(a),
            );
            d.phase = Some(TaskPhase {
                phase: Phase::ALL[i],
                label: Some(["JWT", "OAuth", "API", "Schema", "Billing"][i].into()),
                reported_at: NOW - 10,
                source: "pgx agent activity".into(),
            });
            v.push(d);
        }
        let mut long = standalone(
            7,
            &format!(
                "{}{}",
                "x".repeat(200),
                "\x1b[2J\x1b]0;pwn\x07漢字e\u{301}🚀"
            ),
        );
        long.connections = None;
        long.size_bytes = None;
        v.push(long);
        let mut unl = standalone(8, "permanent");
        unl.ttl_label = Some("unlimited".into());
        v.push(unl);
        let mut due = standalone(9, "overdue");
        due.expires_at = Some(NOW - 100);
        v.push(due);
        v
    }

    #[test]
    fn renders_at_every_size_without_overflow_or_escape_leaks() {
        let mut s = snap(rich());
        s.warnings
            .push(Warning::issue("x", "something \x1b[31mbad"));
        for (w, h) in [
            (40u16, 12u16),
            (80, 24),
            (120, 40),
            (160, 50),
            (20, 4),
            (19, 3),
            (61, 16),
            (59, 30),
            (200, 8),
        ] {
            for inspector in [false, true] {
                for help in [false, true] {
                    let mut app = App::new(s.clone(), Default::default(), true, 1000);
                    app.inspector = inspector;
                    app.help = help;
                    for _ in 0..4 {
                        app.on_key(Key::Down, 10);
                    }
                    let lines = screen(&app, w, h);
                    assert_eq!(lines.len(), h as usize);
                    for l in &lines {
                        assert!(
                            !l.contains('\x1b') && !l.contains('\x07'),
                            "{w}x{h}: escape leaked: {l:?}"
                        );
                        assert!(super::super::text::is_clean(l), "{w}x{h}: {l:?}");
                    }
                    if w >= 20 && h >= 4 {
                        assert!(lines[0].contains("PGX TOP"), "{w}x{h}: {:?}", lines[0]);
                    }
                }
            }
        }
    }

    #[test]
    fn core_dashboard_fits_80x24_and_wide_adds_columns() {
        let app = App::new(snap(rich()), Default::default(), false, 1000);
        let l = screen(&app, 80, 24);
        let all = l.join("\n");
        for want in [
            "PGX TOP",
            "Runtime: RUNNING",
            "PID 4242",
            "MEM [",
            "ISSUES 0",
            "DB STATE",
            "TASK",
            "CONN",
            "TTL",
            "remotely",
            "Claude Code · session 72ac4f1e",
            "agent-a · JWT",
            "RECENT EVENTS",
            "q quit",
        ] {
            assert!(all.contains(want), "80x24 lacks {want:?}:\n{all}");
        }
        assert!(!all.contains("LIFECYCLE"));
        let wide = screen(&app, 120, 40).join("\n");
        for want in [
            "LIFECYCLE",
            "HOST",
            "SIZE",
            "TOUCH",
            "TESTING",
            "MIGRATING",
            "FAILED",
            "COMPLETED",
        ] {
            assert!(wide.contains(want), "120x40 lacks {want}:\n{wide}");
        }
        let compact = screen(&app, 40, 12);
        assert!(
            compact[0].contains("PGX TOP") && compact[0].contains("DB 9"),
            "{:?}",
            compact[0]
        );
        assert!(compact.iter().any(|l| l.contains("agent-a")), "{compact:?}");
        assert!(compact[11].contains("q"), "{:?}", compact[11]);
    }

    #[test]
    fn missing_metrics_show_na_not_zero_and_ttl_marks() {
        let app = App::new(
            snap(rich()),
            ViewOpts {
                tree: false,
                ..Default::default()
            },
            false,
            1000,
        );
        let all = screen(&app, 160, 50).join("\n");
        let long_line = all.lines().find(|l| l.contains("xxxxxxxx")).unwrap();
        assert!(long_line.contains("n/a"), "{long_line}");
        assert!(long_line.contains('…'), "long label clipped: {long_line}");
        let perm = all.lines().find(|l| l.contains("permanent")).unwrap();
        assert!(perm.contains('∞'), "{perm}");
        let due = all.lines().find(|l| l.contains("overdue")).unwrap();
        assert!(due.contains("due"), "{due}");
    }

    #[test]
    fn inspector_shows_identity_ttl_and_phase_without_secrets() {
        let mut app = App::new(snap(rich()), Default::default(), false, 1000);
        for _ in 0..4 {
            app.on_key(Key::Down, 10);
        }
        assert_eq!(app.selected.as_deref(), Some("db:102"));
        app.on_key(Key::Enter, 10);
        let all = screen(&app, 120, 40).join("\n");
        for want in [
            "DETAILS",
            "oid 102",
            "session",
            "72ac4f1e-0000",
            "agent-b",
            "idle TTL after last touch",
            "MIGRATING \"OAuth\"",
            "pgx agent activity",
            "hook heartbeat",
            "logical",
        ] {
            assert!(all.contains(want), "inspector lacks {want:?}:\n{all}");
        }
        for secret in ["password", "postgres://", "DATABASE_URL"] {
            assert!(!all.contains(secret));
        }
        let mut app = App::new(
            snap(vec![standalone(5, "postgres")]),
            Default::default(),
            false,
            1000,
        );
        app.on_key(Key::Down, 10);
        app.on_key(Key::Enter, 10);
        let all = screen(&app, 80, 30).join("\n");
        assert!(all.contains("— (no explicit report)"), "{all}");
    }

    #[test]
    fn demo_badge_and_disconnected_states() {
        let app = App::new(
            super::super::demo::snapshot(40),
            Default::default(),
            true,
            1000,
        );
        let top = screen(&app, 80, 24);
        assert!(top[0].contains("DEMO / SIMULATED"), "{:?}", top[0]);
        let small = screen(&app, 40, 12);
        assert!(small[0].contains("DEMO"), "{:?}", small[0]);
        let mut s = snap(vec![]);
        s.databases = None;
        s.runtime = Runtime::empty(RuntimeStatus::Stopped);
        let app = App::new(s, Default::default(), true, 1000);
        let all = screen(&app, 80, 24).join("\n");
        assert!(
            all.contains("STOPPED") && all.contains("pgx start"),
            "{all}"
        );
        assert!(all.contains("DATABASES n/a"), "unknown is not zero: {all}");
    }

    #[test]
    fn search_line_and_paused_badge() {
        let mut app = App::new(snap(rich()), Default::default(), false, 1000);
        app.on_key(Key::Char('/'), 10);
        for c in "oauth".chars() {
            app.on_key(Key::Char(c), 10);
        }
        let l = screen(&app, 80, 24);
        assert!(l[23].contains("/oauth█"), "{:?}", l[23]);
        app.on_key(Key::Enter, 10);
        app.on_key(Key::Char('p'), 10);
        let l = screen(&app, 80, 24);
        assert!(l[0].contains("PAUSED"), "{:?}", l[0]);
        let mut empty = App::new(snap(rich()), Default::default(), false, 1000);
        empty.view.query = "zzz-none".into();
        empty.rebuild();
        assert!(screen(&empty, 80, 24)
            .join("\n")
            .contains("No databases match /zzz-none"));
    }

    #[test]
    fn plain_snapshot_text_is_escape_free() {
        let p = plain(&snap(rich()));
        assert!(p.starts_with("PGX TOP  runtime RUNNING"));
        assert!(!p.contains('\x1b'));
        assert!(p.contains("agent-a · JWT"));
        let d = plain(&super::super::demo::snapshot(40));
        assert!(d.contains("[DEMO / SIMULATED]"));
    }

    #[test]
    fn countdown_advances_between_refreshes() {
        let app = App::new(snap(rich()), Default::default(), false, 1000);
        assert_eq!(display_now(&app, app.received_at + 30), NOW + 30);
        assert_eq!(
            display_now(&app, app.received_at - 30),
            NOW,
            "clock skew never rewinds"
        );
    }

    use super::super::tree::ViewOpts;
}
