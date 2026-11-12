//! Recent events: transitions between successive snapshots, in a bounded ring.
//!
//! Only a *complete* catalog read can prove a database is gone. A snapshot
//! without a catalog (runtime stopped, connection lost, query failed) yields
//! one "runtime disconnected" event and leaves the last good baseline in
//! place; the next complete snapshot is compared with that baseline, so a
//! transient failure never turns into a storm of "dropped" events. The first
//! snapshot only sets the baseline.
use super::model::*;
use super::text::sanitize;
use std::collections::{BTreeMap, HashSet, VecDeque};

pub const DEFAULT_CAPACITY: usize = 100;
/// A deadline that moved less than this is a heartbeat, not news.
const DEADLINE_NOISE_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub at: u64,
    /// `+` created, `-` gone, `~` changed, `✓` retired, `!` problem, `▲`/`▼` connections, `●`/`○` runtime.
    pub glyph: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    name: String,
    lifecycle: Option<Lifecycle>,
    retired: bool,
    connections: Option<u64>,
    expires_at: Option<u64>,
    ttl_seconds: Option<u64>,
    ttl_state: Option<String>,
    phase: Option<(Phase, Option<String>, u64)>,
    session: Option<String>,
    session_key: Option<String>,
    subagent: bool,
}

fn seen(d: &Database) -> Seen {
    Seen {
        name: sanitize(&d.name),
        lifecycle: d.lifecycle,
        retired: d.agent.as_ref().is_some_and(|a| a.retired),
        connections: d.connections,
        expires_at: d.expires_at,
        ttl_seconds: d.ttl_seconds,
        ttl_state: d.ttl_state.clone(),
        phase: d
            .phase
            .as_ref()
            .map(|p| (p.phase, p.label.clone(), p.reported_at)),
        session: d.agent.as_ref().map(|a| {
            format!(
                "{} session {}",
                super::tree::host_label(&a.host),
                super::tree::short(&a.session_id, 8)
            )
        }),
        session_key: d.agent.as_ref().map(session_key),
        subagent: d.role == Role::Subagent,
    }
}

fn session_key(a: &Agent) -> String {
    format!("{}|{}|{}", a.project_id, a.host, a.session_id)
}

#[derive(Debug, Clone)]
pub struct EventLog {
    cap: usize,
    events: VecDeque<Event>,
    baseline: Option<BTreeMap<u64, Seen>>,
    sessions: HashSet<String>,
    connected: Option<bool>,
    pid: Option<u32>,
    warnings: HashSet<String>,
}

impl Default for EventLog {
    fn default() -> Self {
        EventLog::new(DEFAULT_CAPACITY)
    }
}

fn session_keys(dbs: &[Database]) -> HashSet<String> {
    dbs.iter()
        .filter_map(|d| d.agent.as_ref())
        .map(session_key)
        .collect()
}

impl EventLog {
    pub fn new(cap: usize) -> EventLog {
        EventLog {
            cap: cap.max(1),
            events: VecDeque::new(),
            baseline: None,
            sessions: HashSet::new(),
            connected: None,
            pid: None,
            warnings: HashSet::new(),
        }
    }

    /// Newest first.
    pub fn recent(&self) -> impl Iterator<Item = &Event> {
        self.events.iter().rev()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn push(&mut self, at: u64, glyph: &'static str, text: String) {
        // Deduplicate: the same text within the last few entries and seconds.
        if self
            .events
            .iter()
            .rev()
            .take(8)
            .any(|e| e.text == text && at.saturating_sub(e.at) < 10)
        {
            return;
        }
        self.events.push_back(Event { at, glyph, text });
        while self.events.len() > self.cap {
            self.events.pop_front();
        }
    }

    /// Fold one snapshot in.
    pub fn observe(&mut self, s: &TopSnapshot) {
        let at = s.collected_at;
        let first = self.connected.is_none();
        let Some(dbs) = &s.databases else {
            if self.connected == Some(true) {
                self.push(
                    at,
                    "○",
                    format!("runtime {}", s.runtime.status.label().to_lowercase()),
                );
            }
            self.connected = Some(false);
            return;
        };
        if self.connected == Some(false) && self.baseline.is_some() {
            self.push(at, "●", "runtime reconnected".into());
        }
        if let (Some(old), Some(new)) = (self.pid, s.runtime.pid) {
            if old != new {
                self.push(at, "●", format!("runtime restarted (pid {old} → {new})"));
            }
        }
        if s.runtime.pid.is_some() {
            self.pid = s.runtime.pid;
        }
        self.connected = Some(true);
        let now: BTreeMap<u64, Seen> = dbs.iter().map(|d| (d.oid, seen(d))).collect();
        let sessions = session_keys(dbs);
        let warn_keys: HashSet<String> = s
            .warnings
            .iter()
            .filter(|w| w.severity == Severity::Issue)
            .map(|w| format!("{}|{}", w.code, w.message))
            .collect();
        if let (false, Some(base)) = (first, self.baseline.take()) {
            self.diff(at, &base, &now, &sessions);
            for w in &s.warnings {
                let k = format!("{}|{}", w.code, w.message);
                if w.severity == Severity::Issue && !self.warnings.contains(&k) {
                    self.push(at, "!", sanitize(&w.message));
                }
            }
        }
        self.baseline = Some(now);
        self.sessions = sessions;
        self.warnings = warn_keys;
    }

    fn diff(
        &mut self,
        at: u64,
        base: &BTreeMap<u64, Seen>,
        now: &BTreeMap<u64, Seen>,
        sessions: &HashSet<String>,
    ) {
        let new_sessions: HashSet<String> = sessions.difference(&self.sessions).cloned().collect();
        let mut announced = HashSet::new();
        for (oid, n) in now {
            let Some(o) = base.get(oid) else {
                if let (Some(label), Some(key)) = (&n.session, &n.session_key) {
                    if new_sessions.contains(key) && announced.insert(key.clone()) {
                        self.push(at, "+", format!("{label} started"));
                    }
                }
                let what = if n.subagent {
                    "subagent database"
                } else {
                    "database"
                };
                self.push(at, "+", format!("{} {what} created", n.name));
                continue;
            };
            if !o.retired && n.retired {
                let who = if n.subagent { "subagent" } else { "session" };
                self.push(
                    at,
                    "✓",
                    format!("{} {who} retired (owner ended; not a task result)", n.name),
                );
            } else if o.retired && !n.retired {
                self.push(at, "~", format!("{} owner resumed", n.name));
            }
            if o.lifecycle != Some(Lifecycle::Expired) && n.lifecycle == Some(Lifecycle::Expired) {
                self.push(at, "!", format!("{} passed its deadline", n.name));
            }
            match (o.connections, n.connections) {
                (Some(0), Some(c)) if c > 0 => {
                    self.push(at, "▲", format!("{} receiving connections ({c})", n.name))
                }
                (Some(c), Some(0)) if c > 0 => {
                    self.push(at, "▼", format!("{} no client connections", n.name))
                }
                _ => {}
            }
            let moved = match (o.expires_at, n.expires_at) {
                (Some(a), Some(b)) => a.abs_diff(b) >= DEADLINE_NOISE_SECS,
                (None, None) => false,
                _ => true,
            };
            if o.ttl_seconds != n.ttl_seconds || (moved && o.retired == n.retired) {
                let to = match n.expires_at {
                    Some(e) => format!("now {}", super::text::compact_secs(e.saturating_sub(at))),
                    None => "now unlimited".into(),
                };
                self.push(at, "~", format!("{} TTL deadline changed ({to})", n.name));
            }
            if o.ttl_state != n.ttl_state && n.ttl_state.as_deref() == Some("retry_pending") {
                self.push(
                    at,
                    "!",
                    format!("{} TTL deletion failed; retry pending", n.name),
                );
            }
            if o.phase != n.phase {
                if let Some((p, label, _)) = &n.phase {
                    let l = label
                        .as_deref()
                        .map(|l| format!(" ({})", sanitize(l)))
                        .unwrap_or_default();
                    self.push(at, "~", format!("{} reported {}{l}", n.name, p.label()));
                }
            }
        }
        for (oid, o) in base {
            if !now.contains_key(oid) {
                let what = if o.subagent {
                    "subagent database"
                } else {
                    "database"
                };
                self.push(at, "-", format!("{} {what} dropped", o.name));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::fixtures::*;
    use super::*;

    fn texts(l: &EventLog) -> Vec<String> {
        let mut v: Vec<String> = l
            .recent()
            .map(|e| format!("{} {}", e.glyph, e.text))
            .collect();
        v.reverse();
        v
    }

    fn at(mut s: TopSnapshot, t: u64) -> TopSnapshot {
        s.collected_at = NOW + t;
        s
    }

    fn down(t: u64) -> TopSnapshot {
        let mut s = at(snap(vec![]), t);
        s.databases = None;
        s.runtime = Runtime::empty(RuntimeStatus::Disconnected);
        s
    }

    #[test]
    fn first_snapshot_is_only_a_baseline() {
        let mut l = EventLog::default();
        l.observe(&snap(vec![
            standalone(5, "postgres"),
            agent_db(9, "a", "p", "claude", "s", None),
        ]));
        assert!(l.is_empty(), "{:?}", texts(&l));
    }

    #[test]
    fn one_create_and_one_drop_make_exactly_one_event_each() {
        let mut l = EventLog::default();
        l.observe(&snap(vec![standalone(5, "postgres")]));
        l.observe(&at(
            snap(vec![standalone(5, "postgres"), standalone(6, "new")]),
            1,
        ));
        assert_eq!(texts(&l), ["+ new database created"]);
        l.observe(&at(snap(vec![standalone(5, "postgres")]), 2));
        assert_eq!(
            texts(&l),
            ["+ new database created", "- new database dropped"]
        );
        l.observe(&at(snap(vec![standalone(5, "postgres")]), 3));
        assert_eq!(l.len(), 2, "no change, no event");
    }

    #[test]
    fn disconnect_never_becomes_a_drop_storm() {
        let mut l = EventLog::default();
        let full = || {
            snap(
                (0..50)
                    .map(|i| standalone(100 + i, &format!("d{i}")))
                    .collect(),
            )
        };
        l.observe(&full());
        l.observe(&down(1));
        l.observe(&down(2));
        l.observe(&down(3));
        l.observe(&at(full(), 4));
        assert_eq!(
            texts(&l),
            ["○ runtime disconnected", "● runtime reconnected"]
        );
    }

    #[test]
    fn a_drop_while_disconnected_is_reported_once_after_reconnect() {
        let mut l = EventLog::default();
        l.observe(&snap(vec![standalone(5, "postgres"), standalone(6, "x")]));
        l.observe(&down(1));
        let mut back = at(snap(vec![standalone(5, "postgres")]), 2);
        back.runtime.pid = Some(9999);
        l.observe(&back);
        assert_eq!(
            texts(&l),
            [
                "○ runtime disconnected",
                "● runtime reconnected",
                "● runtime restarted (pid 4242 → 9999)",
                "- x database dropped"
            ]
        );
    }

    #[test]
    fn lifecycle_connection_ttl_phase_and_error_transitions() {
        let mut l = EventLog::default();
        let a0 = agent_db(10, "agent-a", "p", "claude", "s", Some("agent-a"));
        l.observe(&snap(vec![a0.clone()]));
        let mut a1 = a0.clone();
        a1.connections = Some(2);
        a1.db_state = DbState::Connected;
        l.observe(&at(snap(vec![a1.clone()]), 1));
        let mut a2 = a1.clone();
        a2.phase = Some(TaskPhase {
            phase: Phase::Testing,
            label: Some("OAuth".into()),
            reported_at: NOW + 2,
            source: "pgx agent activity".into(),
        });
        a2.expires_at = Some(a1.expires_at.unwrap() + 30); // heartbeat: not news
        l.observe(&at(snap(vec![a2.clone()]), 2));
        let mut a3 = a2.clone();
        a3.connections = Some(0);
        a3.agent.as_mut().unwrap().retired = true;
        a3.lifecycle = Some(Lifecycle::Retired);
        a3.expires_at = Some(NOW + 600);
        l.observe(&at(snap(vec![a3.clone()]), 3));
        let mut a4 = a3.clone();
        a4.ttl_seconds = Some(60);
        a4.ttl_state = Some("retry_pending".into());
        let mut s4 = at(snap(vec![a4]), 4);
        s4.warnings.push(Warning::issue(
            "ttl_delete_retry",
            "agent-a: TTL deletion is being retried",
        ));
        l.observe(&s4);
        assert_eq!(
            texts(&l),
            [
                "▲ agent-a receiving connections (2)",
                "~ agent-a reported TESTING (OAuth)",
                "✓ agent-a subagent retired (owner ended; not a task result)",
                "▼ agent-a no client connections",
                "~ agent-a TTL deadline changed (now 9m56s)"
                    .replace("9m56s", &super::super::text::compact_secs(596))
                    .as_str(),
                "! agent-a TTL deletion failed; retry pending",
                "! agent-a: TTL deletion is being retried",
            ]
        );
        assert!(
            texts(&l).iter().all(|t| !t.contains("COMPLETED")),
            "retirement is never success"
        );
    }

    #[test]
    fn new_session_is_announced_once() {
        let mut l = EventLog::default();
        l.observe(&snap(vec![]));
        l.observe(&at(
            snap(vec![
                agent_db(10, "agent-claude-s1", "p", "claude", "s1", None),
                agent_db(11, "agent-claude-s1-x", "p", "claude", "s1", Some("x")),
            ]),
            1,
        ));
        assert_eq!(
            texts(&l),
            [
                "+ Claude Code session s1 started",
                "+ agent-claude-s1 database created",
                "+ agent-claude-s1-x subagent database created"
            ]
        );
    }

    #[test]
    fn ring_is_capped_and_order_is_stable() {
        let mut l = EventLog::new(10);
        l.observe(&snap(vec![]));
        let mut dbs = Vec::new();
        for i in 0..50u64 {
            dbs.push(standalone(1000 + i, &format!("d{i}")));
            l.observe(&at(snap(dbs.clone()), 100 * (i + 1)));
        }
        assert_eq!(l.len(), 10);
        let newest = l.recent().next().unwrap();
        assert_eq!(newest.text, "d49 database created");
        let ats: Vec<u64> = l.recent().map(|e| e.at).collect();
        assert!(ats.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn event_text_is_sanitized() {
        let mut l = EventLog::default();
        l.observe(&snap(vec![]));
        l.observe(&at(snap(vec![standalone(7, "\x1b[2Jevil")]), 1));
        assert!(l.recent().all(|e| !e.text.contains('\x1b')));
    }
}
