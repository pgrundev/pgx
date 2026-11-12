//! `pgx top --demo`: a deterministic simulation of one Claude Code session
//! with five subagents, each on its own database, in one project and one Git
//! branch. Pure: `snapshot(t)` depends only on the elapsed second `t`, reads
//! no PGX home and opens no connection. Every value here is fictional and the
//! snapshot is marked `Mode::Demo`, which the UI shows as `DEMO / SIMULATED`.
use super::model::*;

/// Simulated wall clock origin: 2026-11-11T20:00:00Z.
pub const EPOCH: u64 = 1_794_427_200;
/// The timeline repeats after this many seconds.
pub const PERIOD: u64 = 150;
/// The second `--demo --snapshot` captures: all five agents at work.
pub const SNAPSHOT_T: u64 = 40;

const SESSION: &str = "72ac4f1e-5b7d-4c1a-9e0f-3d2a1b0c9d8e";
const PROJECT_ID: &str = "d3m0d3m0d3m0d3m0";
const SCOPE: &str = "5c09e5c09e5c09e5";

struct Agent {
    id: &'static str,
    task: &'static str,
    created: u64,
    /// (from second, phase) explicit reports, in order.
    phases: &'static [(u64, Phase)],
    /// (from, to) seconds with connections, and how many.
    busy: &'static [(u64, u64, u64)],
    retired_at: Option<u64>,
}

const AGENTS: [Agent; 5] = [
    Agent {
        id: "agent-a",
        task: "JWT",
        created: 2,
        phases: &[
            (6, Phase::Working),
            (32, Phase::Testing),
            (62, Phase::Completed),
        ],
        busy: &[(4, 30, 1), (32, 60, 2)],
        retired_at: Some(66),
    },
    Agent {
        id: "agent-b",
        task: "OAuth",
        created: 5,
        phases: &[
            (9, Phase::Working),
            (20, Phase::Migrating),
            (36, Phase::Testing),
            (78, Phase::Failed),
        ],
        busy: &[(7, 20, 1), (20, 36, 1), (36, 76, 3)],
        retired_at: None,
    },
    Agent {
        id: "agent-c",
        task: "API",
        created: 8,
        phases: &[(12, Phase::Working)],
        busy: &[(10, 90, 1)],
        retired_at: None,
    },
    Agent {
        id: "agent-d",
        task: "Schema",
        created: 11,
        phases: &[
            (15, Phase::Working),
            (44, Phase::Migrating),
            (58, Phase::Working),
        ],
        busy: &[(44, 58, 1)],
        retired_at: None,
    },
    Agent {
        id: "agent-e",
        task: "Billing",
        created: 14,
        phases: &[
            (18, Phase::Working),
            (30, Phase::Testing),
            (50, Phase::Completed),
        ],
        busy: &[(16, 48, 1)],
        retired_at: Some(54),
    },
];

/// The session ends here: every remaining subagent retires.
const SESSION_END: u64 = 100;
const IDLE_TTL: u64 = 7200;
const GRACE: u64 = 600;

/// The simulated state `t` seconds into the timeline (wrapped by [`PERIOD`]).
pub fn snapshot(t: u64) -> TopSnapshot {
    let t = t % PERIOD;
    let now = EPOCH + t;
    let mut dbs = vec![Database {
        oid: 16_384,
        name: "remotely_dev".into(),
        is_template: false,
        invalid: false,
        connections: Some(0),
        size_bytes: Some(48 << 20),
        created_at: Some(EPOCH - 86_400),
        ttl_label: None,
        ttl_seconds: None,
        expires_at: None,
        ttl_state: None,
        last_error: None,
        deadline_kind: None,
        agent: None,
        git: None,
        phase: None,
        db_state: DbState::Idle,
        lifecycle: None,
        role: Role::Standalone,
    }];
    for (i, a) in AGENTS.iter().enumerate() {
        if t < a.created {
            continue;
        }
        let retired_at = a.retired_at.or(Some(SESSION_END)).filter(|r| t >= *r);
        let conns: u64 = if retired_at.is_some() {
            0
        } else {
            a.busy
                .iter()
                .filter(|(from, to, _)| t >= *from && t < *to)
                .map(|(_, _, n)| *n)
                .sum()
        };
        // Hooks touch every 30 s while the subagent is alive.
        let last_touch = match retired_at {
            Some(r) => r,
            None => a.created + ((t - a.created) / 30) * 30,
        };
        let expires_at = match retired_at {
            Some(r) => r + GRACE,
            None => last_touch + IDLE_TTL,
        };
        let phase = a
            .phases
            .iter()
            .rev()
            .find(|(from, _)| t >= *from)
            .map(|(from, p)| TaskPhase {
                phase: *p,
                label: Some(a.task.to_string()),
                reported_at: EPOCH + from,
                source: "demo".into(),
            });
        dbs.push(Database {
            oid: 16_400 + i as u64,
            name: format!("agent-claude-72ac4f1e-{}", &a.id[6..]),
            is_template: false,
            invalid: false,
            connections: Some(conns),
            size_bytes: Some((48 << 20) + (t - a.created) * 61_440 * (i as u64 + 1)),
            created_at: Some(EPOCH + a.created),
            ttl_label: Some("2h".into()),
            ttl_seconds: Some(IDLE_TTL),
            expires_at: Some(EPOCH + expires_at),
            ttl_state: Some("active".into()),
            last_error: None,
            deadline_kind: Some(if retired_at.is_some() {
                DeadlineKind::Grace
            } else {
                DeadlineKind::IdleTtl
            }),
            agent: Some(super::model::Agent {
                host: "claude".into(),
                session_id: SESSION.into(),
                agent_id: Some(a.id.into()),
                project_id: PROJECT_ID.into(),
                project_root: "/demo/remotely".into(),
                project_name: "remotely".into(),
                source: "remotely_dev".into(),
                last_touch: Some(EPOCH + last_touch),
                retired: retired_at.is_some(),
                git_scope: Some(SCOPE.into()),
                git_branch: Some("feature-auth".into()),
                worktree: Some("/demo/remotely".into()),
            }),
            git: None,
            phase,
            db_state: if conns > 0 {
                DbState::Connected
            } else {
                DbState::Idle
            },
            lifecycle: Some(if retired_at.is_some() {
                Lifecycle::Retired
            } else {
                Lifecycle::Active
            }),
            role: Role::Subagent,
        });
    }
    dbs.sort_by(|a, b| a.name.cmp(&b.name));
    TopSnapshot {
        mode: Mode::Demo,
        collected_at: now,
        runtime: Runtime {
            status: RuntimeStatus::Running,
            pid: Some(73_136),
            port: Some(5433),
            uptime_seconds: Some(2_538 + t),
            rss_bytes: Some((176 << 20) + (dbs.len() as u64) * (3 << 20)),
            phys_mem_bytes: Some(16 << 30),
            runtime_status_available: true,
        },
        databases: Some(dbs),
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents(s: &TopSnapshot) -> Vec<&Database> {
        s.databases
            .as_ref()
            .unwrap()
            .iter()
            .filter(|d| d.agent.is_some())
            .collect()
    }

    #[test]
    fn deterministic_and_marked_simulated() {
        assert_eq!(snapshot(SNAPSHOT_T), snapshot(SNAPSHOT_T));
        assert_eq!(snapshot(SNAPSHOT_T + PERIOD), snapshot(SNAPSHOT_T));
        let s = snapshot(SNAPSHOT_T);
        assert_eq!(s.mode, Mode::Demo);
        assert_eq!(s.to_json()["simulated"], true);
        assert_eq!(s.to_json()["mode"], "demo");
    }

    #[test]
    fn timeline_has_five_agents_and_the_promised_story() {
        assert_eq!(agents(&snapshot(0)).len(), 0);
        assert_eq!(agents(&snapshot(SNAPSHOT_T)).len(), 5);
        let one_session: std::collections::HashSet<_> = agents(&snapshot(SNAPSHOT_T))
            .iter()
            .map(|d| d.agent.as_ref().unwrap().session_id.clone())
            .collect();
        assert_eq!(one_session.len(), 1);
        let mut seen = std::collections::HashSet::new();
        let mut conn_changes = 0;
        let mut prev: Option<Vec<Option<u64>>> = None;
        for t in 0..PERIOD {
            let s = snapshot(t);
            for d in agents(&s) {
                if let Some(p) = &d.phase {
                    seen.insert(p.phase);
                }
                assert!(
                    d.remaining(s.collected_at).unwrap() > 0,
                    "TTL counts down, never negative"
                );
            }
            let c: Vec<Option<u64>> = agents(&s).iter().map(|d| d.connections).collect();
            if prev.as_ref().is_some_and(|p| *p != c) {
                conn_changes += 1;
            }
            prev = Some(c);
        }
        for p in Phase::ALL {
            assert!(seen.contains(&p), "{p:?} never shown");
        }
        assert!(conn_changes > 5);
        let end = snapshot(SESSION_END + 1);
        assert!(agents(&end)
            .iter()
            .all(|d| d.lifecycle == Some(Lifecycle::Retired)));
        // agent-c retired with the session but never reported COMPLETED.
        let c = agents(&end)
            .into_iter()
            .find(|d| d.name.ends_with("-c"))
            .unwrap();
        assert_eq!(c.phase.as_ref().unwrap().phase, Phase::Working);
    }
}
