//! `pgx agent activity --state <phase> [--task <label>]`: an agent reports
//! what it is doing, for `pgx top`'s TASK column.
//!
//! The report attaches only to the database this session (or subagent)
//! verifiably owns: the identity resolves exactly as for `touch`/`down`, the
//! TTL record must exist and be unexpired, and the live database must still
//! have the record's OID in this cluster incarnation. It is written to
//! `$PGX_HOME/top/phases/` and never touches the TTL record — no deadline,
//! lifecycle or ownership changes. Nothing ever writes a phase implicitly: a
//! hook's `SessionEnd`/`SubagentStop` retires the branch but does not report
//! `completed`.
use super::session::{self, ResolveInput};
use crate::cli::ExitCode;
use crate::cmd::top::model::Phase;
use crate::cmd::top::phase::{self, PhaseRecord};
use crate::errors::{PgxError, Result};
use crate::out::Out;
use crate::ttlstore::MetaStore;
use serde_json::json;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityOpts {
    pub id: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    pub cwd: Option<PathBuf>,
    pub state: String,
    pub task: Option<String>,
}

pub const USAGE: &str = "usage: pgx agent activity --state working|migrating|testing|completed|failed [--task <label>] [--id <session>] [--host <h>] [--agent <id>] [--cwd <dir>]";

pub fn parse_state(s: &str) -> Result<Phase> {
    Phase::parse(s).ok_or_else(|| {
        PgxError::usage(format!(
            "--state must be one of working, migrating, testing, completed, failed; got {s:?}"
        ))
    })
}

/// Inside a host session the host's own session id wins: `--id` naming a
/// *different* session is refused, so one agent cannot report for another.
fn check_spoofing(o: &ActivityOpts) -> Result<()> {
    let Some(id) = &o.id else { return Ok(()) };
    for var in ["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"] {
        if let Ok(own) = std::env::var(var) {
            if !own.is_empty() && &own != id {
                return Err(PgxError::new(
                    ExitCode::Conflict,
                    format!("--id {id} is not this session ({var} is set to another id); refusing to report for another session"),
                ));
            }
        }
    }
    Ok(())
}

pub fn run(out: &Out, o: ActivityOpts) -> Result<ExitCode> {
    let state = parse_state(&o.state)?;
    let label = match &o.task {
        Some(t) => phase::validate_label(t)?,
        None => None,
    };
    check_spoofing(&o)?;
    let ctx = session::load_ctx()?;
    let r = session::resolve_by_id(&ResolveInput {
        id: o.id.clone(),
        host: o.host.clone(),
        agent_id: o.agent.clone(),
        cwd: o.cwd.clone(),
        ..Default::default()
    })?;
    let store = MetaStore::new(&ctx.home.root);
    let Some(rec) = session::find_record(&store, &r.key)? else {
        return Err(PgxError::new(
            ExitCode::NotFound,
            format!("no agent database for {}", r.key.describe()),
        )
        .hint(
            "`pgx agent up` (or the session-start hook) creates one; `pgx agent ls` lists them",
        ));
    };
    let now = crate::fsutil::now_epoch();
    if rec.is_expired(now) {
        return Err(PgxError::new(
            ExitCode::NotFound,
            format!("{} has expired; nothing to report on", rec.name),
        ));
    }
    let Some(meta) = rec.agent.clone() else {
        return Err(PgxError::new(
            ExitCode::NotFound,
            "record has no agent owner",
        ));
    };
    let cluster = ctx.cluster_id();
    if rec.cluster_id != cluster {
        return Err(PgxError::new(
            ExitCode::Conflict,
            format!(
                "{} was recorded for another cluster incarnation; refusing to report",
                rec.name
            ),
        ));
    }
    // The live database must be the incarnation the record names.
    let mut client = ctx.connect("postgres")?;
    let oid = crate::cmd::db::database_oid(&mut client, &rec.name)?;
    client.close();
    if oid != Some(rec.oid) {
        return Err(PgxError::new(
            ExitCode::Conflict,
            format!(
                "{} is not the database this session's record names (oid {} recorded, {} live); refusing to report",
                rec.name,
                rec.oid,
                oid.map(|o| o.to_string()).unwrap_or_else(|| "none".into())
            ),
        ));
    }
    let p = PhaseRecord {
        cluster_id: cluster,
        database: rec.name.clone(),
        database_oid: rec.oid,
        project_id: meta.project_id.clone(),
        host: meta.host.clone(),
        session_id: meta.session_id.clone(),
        agent_id: meta.agent_id.clone(),
        git_scope: meta.git_scope.clone(),
        phase: state,
        label: label.clone(),
        reported_at: now,
    };
    phase::save(&ctx.home.root, &r.key.material(), &p)?;
    out.emit(
        &format!(
            "{}: {}{}",
            rec.name,
            state.label(),
            label
                .as_deref()
                .map(|l| format!(" ({l})"))
                .unwrap_or_default()
        ),
        json!({
            "database": rec.name,
            "state": state.as_str(),
            "task": label,
            "reported_at": crate::fsutil::rfc3339(now),
            "session_id": meta.session_id,
            "agent_id": meta.agent_id,
        }),
    );
    Ok(ExitCode::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_strict() {
        for s in ["working", "migrating", "testing", "completed", "failed"] {
            assert!(parse_state(s).is_ok());
        }
        for s in ["", "WORKING", "done", "success", "testing\n"] {
            assert_eq!(parse_state(s).unwrap_err().code, ExitCode::Usage, "{s:?}");
        }
    }
}
