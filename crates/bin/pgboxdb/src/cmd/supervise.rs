//! `pgboxdb supervise`: the background TTL cleanup worker. Spawned detached by
//! `pgboxdb start`/`run`/`serve`, it polls the TTL metadata, and for each expired
//! database verifies identity (name present, OID matches the recorded
//! incarnation, same cluster) before a forced drop. It is single-instance (an
//! exclusive flock), tied to the runtime's lifetime (it exits when the engine
//! is gone, so `pgboxdb start` owns respawning), and idempotent across crashes.
use super::Ctx;
use crate::cli::ExitCode;
use crate::errors::{PgboxdbError, Result};
use crate::layout::Home;
use crate::out::Out;
use crate::ttlstore::{MetaStore, State, TtlRecord};
use std::os::fd::AsRawFd;
use std::time::Duration;

const DEFAULT_INTERVAL_SECS: u64 = 5;
/// A failed cleanup is retried, but not before this backoff, so a persistently
/// failing drop cannot busy-loop.
const RETRY_BACKOFF_SECS: u64 = 30;

pub fn run(out: &Out) -> Result<ExitCode> {
    let home = Home::resolve();
    if !home.is_initialized() {
        return Ok(ExitCode::Ok); // nothing to supervise
    }

    // Single instance: hold an exclusive, non-blocking lock for our whole life.
    // A second supervisor simply exits, so start/run/serve may spawn freely.
    let lock_path = home.root.join("supervisor.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| PgboxdbError::io("cannot open", &lock_path, &e))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        out.note("pgboxdb supervise: another supervisor is already running; exiting");
        return Ok(ExitCode::Ok);
    }
    let pid_path = home.root.join("supervisor.pid");
    let _ = std::fs::write(&pid_path, std::process::id().to_string());

    let interval = std::env::var("PGBOXDB_TTL_INTERVAL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(1);
    out.note(&format!(
        "pgboxdb supervise: pid {} interval {interval}s",
        std::process::id()
    ));

    let mut down_cycles = 0u32;
    loop {
        // Home removed or re-initialised out from under us (e.g. a test tore it
        // down, or `pgboxdb init --force`): stop immediately.
        if !home.is_initialized() {
            out.note("pgboxdb supervise: home no longer initialised; exiting");
            let _ = std::fs::remove_file(&pid_path);
            return Ok(ExitCode::Ok);
        }
        match Ctx::load() {
            Ok(ctx) if ctx.running_pid().is_some() => {
                down_cycles = 0;
                run_cycle(&ctx);
            }
            // Runtime down (or config temporarily unreadable). We are tied to the
            // runtime's lifetime: exit so we never linger past `pgboxdb stop`, and a
            // later `pgboxdb start` owns respawning.
            _ => {
                down_cycles += 1;
                if down_cycles >= 2 {
                    out.note("pgboxdb supervise: runtime not running; exiting");
                    let _ = std::fs::remove_file(&pid_path);
                    return Ok(ExitCode::Ok);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(interval));
    }
}

/// One cleanup sweep. Reuses a single connection for all drops in the cycle.
fn run_cycle(ctx: &Ctx) {
    let store = MetaStore::new(&ctx.home.root);
    let (records, _bad) = store.list();
    let now = crate::fsutil::now_epoch();
    let due: Vec<TtlRecord> = records
        .into_iter()
        .filter(|r| r.is_expired(now) && should_attempt(r, now))
        .collect();
    if due.is_empty() {
        return;
    }
    let mut client = match ctx.connect("postgres") {
        Ok(c) => c,
        Err(_) => return, // runtime busy/starting; try again next cycle
    };
    for rec in due {
        if let Err(e) = expire_one(ctx, &store, &mut client, &rec) {
            // Observable failure; back off and retry on a later cycle.
            let mut r = rec.clone();
            r.state = State::RetryPending;
            r.last_attempt = Some(crate::fsutil::now_epoch());
            r.last_error = Some(e.message.clone());
            let _ = store.save(&r);
        }
    }
    client.close();
}

fn should_attempt(rec: &TtlRecord, now: u64) -> bool {
    match rec.state {
        State::RetryPending => rec
            .last_attempt
            .map(|t| now.saturating_sub(t) >= RETRY_BACKOFF_SECS)
            .unwrap_or(true),
        _ => true,
    }
}

/// Expire one database under the full safety gate. Returns Ok when the database
/// is gone and its metadata finalized, or when cleanup is safely declined
/// (protected, wrong cluster, identity mismatch, deadline cancelled).
fn expire_one(
    ctx: &Ctx,
    store: &MetaStore,
    client: &mut crate::pgwire::Client<std::os::unix::net::UnixStream>,
    rec: &TtlRecord,
) -> Result<()> {
    // Gate 1: never auto-delete a protected system database.
    if crate::names::RESERVED.contains(&rec.name.as_str()) {
        return Ok(());
    }
    // Gate 2: only act on the cluster this metadata was written for.
    if rec.cluster_id != ctx.cluster_id() {
        return Ok(());
    }
    // Gate 3: the database must still exist with the recorded OID. Gone → finalize.
    // Different OID → the name was reused by a new database; drop the stale record,
    // never the replacement.
    let mut rec = rec.clone();
    match crate::cmd::db::database_oid(client, &rec.name)? {
        None => return store.remove(&rec.name),
        Some(oid) if oid != rec.oid => {
            // An agent branch lives in a name space only its session uses; a
            // new incarnation under that name was recreated by the session
            // (a framework purge) and is still its disposable branch. Any
            // other database keeps the rule: a reused name is never touched.
            // Only a live session can have recreated it; a retired session
            // is gone, so a same-name newcomer is foreign and the stale
            // record is simply forgotten.
            if rec.agent.as_ref().is_some_and(|m| !m.retired) {
                rec.oid = oid;
                store.save(&rec)?;
            } else {
                return store.remove(&rec.name);
            }
        }
        Some(_) => {}
    }
    let rec = &rec;
    // Gate 4: re-read the live record and the clock immediately before dropping.
    // A concurrent `pgboxdb ttl … unlimited` (record removed) or extension (deadline
    // moved) cancels the deletion.
    let now = crate::fsutil::now_epoch();
    match store.load(&rec.name)? {
        None => return Ok(()),
        Some(cur) => {
            if cur.oid != rec.oid || cur.cluster_id != ctx.cluster_id() || !cur.is_expired(now) {
                return Ok(());
            }
        }
    }

    // Commit to deletion: mark DELETING (crash here → next run re-verifies and
    // either finishes the drop or finds it gone and finalizes).
    let mut deleting = rec.clone();
    deleting.state = State::Deleting;
    deleting.last_attempt = Some(now);
    deleting.last_error = None;
    store.save(&deleting)?;

    // Terminate sessions on THIS database only (never other databases, never the
    // shared runtime), then drop with FORCE.
    let _ = client.simple_query(&format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = {} AND pid <> pg_backend_pid()",
        crate::pgwire::quote_literal(&rec.name)
    ));
    let name = crate::names::validate(&rec.name)?; // safe identifier quoting
    client
        .simple_query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            name.quoted()
        ))
        .map_err(|e| {
            crate::cmd::db_error(e, &format!("cannot drop expired database {}", rec.name))
        })?;

    // Verify the drop, then finalize metadata.
    if crate::cmd::db::database_oid(client, &rec.name)?.is_some() {
        return Err(PgboxdbError::failure(format!(
            "database {} still present after a forced drop",
            rec.name
        )));
    }
    store.remove(&rec.name)
}
