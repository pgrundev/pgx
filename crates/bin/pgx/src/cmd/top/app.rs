//! `pgx top` interaction state: what is shown, what is selected, which keys
//! do what. No terminal here — the event loop feeds [`Key`]s and snapshots
//! in, the renderer reads the state out. No key can change PGX: the only
//! "actions" are view changes and a refresh request to the read-only
//! collector.
use super::events::EventLog;
use super::model::TopSnapshot;
#[cfg(test)]
use super::tree::SortKey;
use super::tree::{self, Node, Row, RowKind, ViewOpts};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    Backspace,
    Tab,
    Char(char),
    CtrlC,
}

#[derive(Debug, Clone)]
pub struct App {
    /// The snapshot on screen.
    pub snapshot: TopSnapshot,
    /// The newest snapshot received while paused.
    pub pending: Option<TopSnapshot>,
    pub events: EventLog,
    pub view: ViewOpts,
    pub collapsed: HashSet<String>,
    /// The selected node's stable id.
    pub selected: Option<String>,
    /// Where the selection was, to land nearby when its node disappears.
    pub selected_pos: usize,
    pub inspector: bool,
    pub help: bool,
    /// `Some(previous query)` while the search line is being edited.
    pub editing: Option<String>,
    pub paused: bool,
    pub color: bool,
    pub interval_ms: u64,
    pub refresh_requested: bool,
    pub quit: bool,
    /// When the shown snapshot arrived (epoch seconds), for the LIVE badge.
    pub received_at: u64,
    roots: Vec<Node>,
    rows: Vec<Row>,
}

impl App {
    pub fn new(first: TopSnapshot, view: ViewOpts, color: bool, interval_ms: u64) -> App {
        let mut events = EventLog::default();
        events.observe(&first);
        let received_at = first.collected_at;
        let mut a = App {
            snapshot: first,
            pending: None,
            events,
            view,
            collapsed: HashSet::new(),
            selected: None,
            selected_pos: 0,
            inspector: false,
            help: false,
            editing: None,
            paused: false,
            color,
            interval_ms,
            refresh_requested: false,
            quit: false,
            received_at,
            roots: Vec::new(),
            rows: Vec::new(),
        };
        a.rebuild();
        a
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn databases(&self) -> &[super::model::Database] {
        self.snapshot.databases.as_deref().unwrap_or(&[])
    }

    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_ref()?;
        self.rows.iter().position(|r| &r.id == id)
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.selected_index().map(|i| &self.rows[i])
    }

    /// A new collector result. Events are folded in even while paused (they
    /// are history); the screen keeps the paused snapshot.
    pub fn on_snapshot(&mut self, s: TopSnapshot, received_at: u64) {
        self.events.observe(&s);
        // Keep showing the last good catalog while disconnected, flagged.
        if self.paused {
            self.pending = Some(s);
            return;
        }
        self.show(s, received_at);
    }

    fn show(&mut self, s: TopSnapshot, received_at: u64) {
        self.snapshot = s;
        self.received_at = received_at;
        self.rebuild();
    }

    /// Recompute the tree and rows, keeping the selection by id.
    pub fn rebuild(&mut self) {
        let dbs = self.snapshot.databases.as_deref().unwrap_or(&[]);
        self.roots = tree::build(dbs, &self.view);
        self.rows = tree::flatten(&self.roots, dbs, &self.collapsed);
        match self
            .selected
            .as_ref()
            .and_then(|id| self.rows.iter().position(|r| &r.id == id))
        {
            Some(i) => self.selected_pos = i,
            None if self.rows.is_empty() => self.selected = None,
            None => {
                // Its node is gone (dropped, filtered, collapsed away): the
                // ancestor if it is visible, else the row now at its place.
                let i = self.selected_pos.min(self.rows.len() - 1);
                self.selected = Some(self.rows[i].id.clone());
                self.selected_pos = i;
            }
        }
    }

    fn select(&mut self, i: usize) {
        if let Some(r) = self.rows.get(i) {
            self.selected = Some(r.id.clone());
            self.selected_pos = i;
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.selected_index().unwrap_or(0) as isize;
        let max = self.rows.len() as isize - 1;
        self.select((cur + delta).clamp(0, max) as usize);
    }

    fn collapse_or_parent(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        match row.kind {
            RowKind::Group {
                collapsed: false, ..
            } => {
                self.collapsed.insert(row.id);
                self.rebuild();
            }
            _ => {
                let dbs = self.snapshot.databases.as_deref().unwrap_or(&[]);
                if let Some(p) = tree::parent_of(&self.roots, dbs, &row.id) {
                    self.selected = Some(p);
                    self.rebuild();
                }
            }
        }
    }

    fn expand(&mut self) {
        if let Some(Row {
            id,
            kind: RowKind::Group {
                collapsed: true, ..
            },
            ..
        }) = self.selected_row().cloned()
        {
            self.collapsed.remove(&id);
            self.rebuild();
        }
    }

    fn toggle_group(&mut self) {
        if let Some(Row {
            id,
            kind: RowKind::Group { collapsed, .. },
            ..
        }) = self.selected_row().cloned()
        {
            if collapsed {
                self.collapsed.remove(&id);
            } else {
                self.collapsed.insert(id);
            }
            self.rebuild();
        }
    }

    /// One key press. `page` is the table height, for PageUp/PageDown.
    pub fn on_key(&mut self, k: Key, page: usize) {
        if k == Key::CtrlC {
            self.quit = true;
            return;
        }
        if let Some(prev) = self.editing.clone() {
            match k {
                Key::Esc => {
                    self.view.query = prev;
                    self.editing = None;
                }
                Key::Enter => self.editing = None,
                Key::Backspace => {
                    self.view.query.pop();
                }
                Key::Char(c) if !c.is_control() && self.view.query.chars().count() < 80 => {
                    self.view.query.push(c)
                }
                Key::Up => self.move_by(-1),
                Key::Down => self.move_by(1),
                _ => return,
            }
            self.rebuild();
            return;
        }
        if self.help {
            self.help = false;
            if !matches!(k, Key::Char('q')) {
                return;
            }
        }
        match k {
            Key::Char('q') => self.quit = true,
            Key::Up | Key::Char('k') => self.move_by(-1),
            Key::Down | Key::Char('j') => self.move_by(1),
            Key::PageUp => self.move_by(-(page.max(1) as isize)),
            Key::PageDown => self.move_by(page.max(1) as isize),
            Key::Home | Key::Char('g') => self.select(0),
            Key::End | Key::Char('G') => self.select(self.rows.len().saturating_sub(1)),
            Key::Left | Key::Char('-') => self.collapse_or_parent(),
            Key::Right | Key::Char('+') => self.expand(),
            Key::Char(' ') | Key::Tab => self.toggle_group(),
            Key::Enter => self.inspector = !self.inspector,
            Key::Esc => {
                if self.inspector {
                    self.inspector = false;
                } else if !self.view.query.is_empty() {
                    self.view.query.clear();
                    self.rebuild();
                }
            }
            Key::Char('t') => {
                self.view.tree = !self.view.tree;
                self.rebuild();
            }
            Key::Char('a') => {
                self.view.agents_only = !self.view.agents_only;
                self.rebuild();
            }
            Key::Char('s') => {
                self.view.sort = self.view.sort.next();
                self.rebuild();
            }
            Key::Char('/') => self.editing = Some(self.view.query.clone()),
            Key::Char('p') => {
                self.paused = !self.paused;
                if !self.paused {
                    if let Some(s) = self.pending.take() {
                        let at = s.collected_at;
                        self.show(s, at);
                    }
                }
            }
            Key::Char('r') => self.refresh_requested = true,
            Key::Char('h') | Key::Char('?') => self.help = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::fixtures::*;
    use super::super::model::*;
    use super::*;

    fn dbs() -> Vec<Database> {
        let mut v = vec![agent_db(
            100,
            "agent-claude-s1",
            "remotely",
            "claude",
            "s1",
            None,
        )];
        for (i, a) in ["agent-a", "agent-b", "agent-c"].iter().enumerate() {
            v.push(agent_db(
                101 + i as u64,
                &format!("agent-claude-s1-{a}"),
                "remotely",
                "claude",
                "s1",
                Some(a),
            ));
        }
        v.push(standalone(5, "postgres"));
        v
    }

    fn app() -> App {
        App::new(snap(dbs()), ViewOpts::default(), false, 1000)
    }

    fn sel(a: &App) -> String {
        a.selected.clone().unwrap()
    }

    fn press(a: &mut App, keys: &[Key]) {
        for k in keys {
            a.on_key(*k, 10);
        }
    }

    #[test]
    fn navigation_moves_by_visible_row_and_clamps() {
        let mut a = app();
        assert_eq!(sel(&a), "p:remotely-id");
        press(&mut a, &[Key::Char('j'), Key::Down]);
        assert_eq!(sel(&a), "db:100");
        press(&mut a, &[Key::Char('k')]);
        assert_eq!(sel(&a), "p:remotely-id/s:claude:s1");
        press(&mut a, &[Key::Up, Key::Up, Key::Up]);
        assert_eq!(sel(&a), "p:remotely-id");
        press(&mut a, &[Key::End]);
        assert_eq!(sel(&a), "db:5");
        press(&mut a, &[Key::Down]);
        assert_eq!(sel(&a), "db:5");
        press(&mut a, &[Key::Home, Key::PageDown]);
        assert_eq!(sel(&a), "db:5");
    }

    #[test]
    fn selection_survives_sibling_creation_resorting_and_deletion() {
        let mut a = app();
        press(&mut a, &[Key::Down, Key::Down, Key::Down, Key::Down]);
        assert_eq!(sel(&a), "db:102");
        // A sibling appears before it.
        let mut more = dbs();
        more.push(agent_db(
            99,
            "agent-claude-s1-0",
            "remotely",
            "claude",
            "s1",
            Some("agent-0"),
        ));
        a.on_snapshot(snap(more.clone()), NOW);
        assert_eq!(sel(&a), "db:102");
        // Re-sorting moves it; the selection follows the id.
        more[2].connections = Some(50);
        a.on_snapshot(snap(more.clone()), NOW);
        a.on_key(Key::Char('s'), 10);
        a.on_key(Key::Char('s'), 10);
        a.on_key(Key::Char('s'), 10);
        assert_eq!(a.view.sort, SortKey::Connections);
        assert_eq!(sel(&a), "db:102");
        let pos = a.selected_index().unwrap();
        // It is dropped: the selection lands on the row now at its place.
        more.retain(|d| d.oid != 102);
        a.on_snapshot(snap(more), NOW);
        assert_ne!(sel(&a), "db:102");
        assert_eq!(a.selected_index(), Some(pos.min(a.rows().len() - 1)));
    }

    #[test]
    fn collapse_state_persists_across_refreshes_and_left_goes_to_parent() {
        let mut a = app();
        press(&mut a, &[Key::Down, Key::Down, Key::Down]); // agent-a
        assert_eq!(sel(&a), "db:101");
        a.on_key(Key::Left, 10);
        assert_eq!(sel(&a), "p:remotely-id/s:claude:s1");
        a.on_key(Key::Left, 10);
        let n = a.rows().len();
        assert!(a.collapsed.contains("p:remotely-id/s:claude:s1"));
        a.on_snapshot(snap(dbs()), NOW + 1);
        assert_eq!(a.rows().len(), n, "still collapsed after refresh");
        a.on_key(Key::Right, 10);
        assert_eq!(a.rows().len(), n + 4);
        a.on_key(Key::Char(' '), 10);
        assert_eq!(a.rows().len(), n);
    }

    #[test]
    fn search_edit_backspace_enter_esc() {
        let mut a = app();
        a.on_key(Key::Char('/'), 10);
        assert!(a.editing.is_some());
        for c in "AGENT-B".chars() {
            a.on_key(Key::Char(c), 10);
        }
        assert_eq!(a.view.query, "AGENT-B");
        let db_rows = |a: &App| {
            a.rows()
                .iter()
                .filter(|r| matches!(r.kind, RowKind::Db(_)))
                .count()
        };
        assert_eq!(db_rows(&a), 1);
        a.on_key(Key::Char('q'), 10);
        assert!(!a.quit, "q is text while editing");
        a.on_key(Key::Backspace, 10);
        assert_eq!(a.view.query, "AGENT-B");
        a.on_key(Key::Enter, 10);
        assert!(a.editing.is_none());
        assert_eq!(a.view.query, "AGENT-B", "Enter keeps the filter");
        a.on_key(Key::Char('/'), 10);
        a.on_key(Key::Char('z'), 10);
        a.on_key(Key::Esc, 10);
        assert_eq!(a.view.query, "AGENT-B", "Esc cancels the edit");
        a.on_key(Key::Esc, 10);
        assert_eq!(a.view.query, "", "Esc outside editing clears the filter");
        a.on_key(Key::Char('/'), 10);
        a.on_key(Key::Char('n'), 10);
        a.on_key(Key::Char('o'), 10);
        a.on_key(Key::Char('p'), 10);
        a.on_key(Key::Char('e'), 10);
        assert!(a.rows().is_empty() && a.selected.is_none(), "zero matches");
        a.on_key(Key::Esc, 10);
        assert!(!a.rows().is_empty());
        assert!(a.selected.is_some());
    }

    #[test]
    fn view_toggles_inspector_help_and_quit() {
        let mut a = app();
        let tree_rows = a.rows().len();
        a.on_key(Key::Char('t'), 10);
        assert!(!a.view.tree);
        assert_eq!(a.rows().len(), 5);
        a.on_key(Key::Char('t'), 10);
        assert_eq!(a.rows().len(), tree_rows);
        a.on_key(Key::Char('a'), 10);
        assert!(a
            .rows()
            .iter()
            .all(|r| r.id != "standalone" && r.id != "db:5"));
        a.on_key(Key::Enter, 10);
        assert!(a.inspector);
        a.on_key(Key::Esc, 10);
        assert!(!a.inspector);
        a.on_key(Key::Char('?'), 10);
        assert!(a.help);
        a.on_key(Key::Char('j'), 10);
        assert!(!a.help, "any key closes help");
        a.on_key(Key::Char('r'), 10);
        assert!(a.refresh_requested);
        a.on_key(Key::Char('h'), 10);
        a.on_key(Key::Char('q'), 10);
        assert!(a.quit);
        let mut b = app();
        b.on_key(Key::CtrlC, 10);
        assert!(b.quit);
    }

    #[test]
    fn pause_freezes_the_screen_and_resume_shows_the_latest() {
        let mut a = app();
        a.on_key(Key::Char('p'), 10);
        assert!(a.paused);
        let before = a.snapshot.clone();
        let mut newer = dbs();
        newer.push(standalone(6, "brand_new"));
        a.on_snapshot(snap(newer.clone()), NOW + 1);
        assert_eq!(a.snapshot, before, "display frozen");
        assert_eq!(a.events.len(), 1, "history still recorded");
        let mut newest = newer;
        newest.push(standalone(7, "newest"));
        a.on_snapshot(snap(newest), NOW + 2);
        a.on_key(Key::Char('p'), 10);
        assert!(!a.paused);
        assert_eq!(
            a.databases().len(),
            7,
            "resume jumps to the latest snapshot"
        );
    }

    #[test]
    fn thousand_database_refresh_and_input_stay_fast() {
        let mut big = Vec::new();
        for i in 0..1000u64 {
            if i < 100 {
                big.push(agent_db(
                    20_000 + i,
                    &format!("a{i:04}"),
                    &format!("p{}", i % 5),
                    "claude",
                    &format!("s{}", i % 25),
                    Some(&format!("agent-{i}")),
                ));
            } else {
                big.push(standalone(20_000 + i, &format!("d{i:04}")));
            }
        }
        let mut a = App::new(snap(big.clone()), ViewOpts::default(), true, 1000);
        let t0 = std::time::Instant::now();
        for k in 0..20u64 {
            a.on_snapshot(snap(big.clone()), NOW + k);
            a.on_key(Key::Down, 30);
        }
        let per = t0.elapsed() / 20;
        assert!(
            per < std::time::Duration::from_millis(100),
            "refresh+key took {per:?}"
        );
        assert!(a.events.len() <= super::super::events::DEFAULT_CAPACITY);
    }
}
