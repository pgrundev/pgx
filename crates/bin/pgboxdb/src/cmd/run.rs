//! `pgboxdb run`: one command to a ready database. Initialise if needed, start if
//! needed, ensure the default `app` database, print its connection URL. Every
//! step is idempotent, so repeating `pgboxdb run` is safe and never resets data.
use super::Ctx;
use crate::cli::ExitCode;
use crate::errors::{PgboxdbError, Result};
use crate::layout::Home;
use crate::names;
use crate::out::Out;
use crate::pgwire::quote_literal;
use serde_json::json;
use std::os::fd::AsRawFd;

/// The database `pgboxdb run` ensures exists.
pub const DEFAULT_DB: &str = "app";

/// Serialise the whole `pgboxdb run` sequence on one home. The start lock only
/// covers starting; init (shared `data.tmp`) and the create/ensure race are not
/// under it, so without this two concurrent first-`pgboxdb run`s on a fresh home
/// could clash initialising. Held for the whole command; released on return.
fn acquire_run_lock(home: &Home) -> Result<std::fs::File> {
    std::fs::create_dir_all(&home.root)
        .map_err(|e| PgboxdbError::io("cannot create", &home.root, &e))?;
    let path = home.root.join("run.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| PgboxdbError::io("cannot open", &path, &e))?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(PgboxdbError::failure(format!(
            "cannot lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(f)
}

pub fn run(out: &Out) -> Result<ExitCode> {
    // Sub-steps run through a quiet Out so only this command's final line
    // reaches stdout (one JSON object under --json); their human progress
    // notes still go to stderr.
    let quiet = out.quiet();

    let home = Home::resolve();
    if !home.is_initialized() {
        // Before the run lock creates the home directory.
        crate::legacy::guard_new_home(&home)?;
    }
    let _run_lock = acquire_run_lock(&home)?;

    // 1. Initialise a fresh $PGBOXDB_HOME. Reads PGBOXDB_PORT / PGBOXDB_DURABILITY for the
    //    first init, like `pgboxdb serve`; an already-initialised home is left
    //    untouched (no --force, so data is never destroyed).
    let initialized = if home.is_initialized() {
        false
    } else {
        out.note("initialising a new PGBoxDB cluster…");
        let port = std::env::var("PGBOXDB_PORT")
            .ok()
            .and_then(|p| p.parse().ok());
        let durability = std::env::var("PGBOXDB_DURABILITY").ok();
        crate::cmd::init::run(&quiet, port, durability, false)?;
        true
    };

    // 2. Start the runtime if it is not already up. ensure_running takes the
    //    start lock, so concurrent `pgboxdb run`s serialise and at most one engine
    //    starts; the rest observe it already running.
    let ctx = Ctx::load()?;
    let started = crate::cmd::start::ensure_running(out, &ctx)?;

    // 3. Ensure the default database exists. Create only when absent — never
    //    drop, reset or overwrite an existing database.
    let name = names::validate_user_target(DEFAULT_DB)?;
    let mut client = ctx.connect("postgres")?;
    let existed = {
        let r = client
            .simple_query(&format!(
                "SELECT 1 FROM pg_database WHERE datname = {}",
                quote_literal(name.as_str())
            ))
            .map_err(|e| super::db_error(e, "cannot query pg_database"))?;
        !r.rows.is_empty()
    };
    if !existed {
        out.note(&format!("creating database {name}…"));
        // A concurrent `pgboxdb run` may create it between our check and this
        // statement; treat "already exists" (42P04) as success, since the
        // post-condition — the database exists — holds either way.
        if let Err(e) = client.simple_query(&format!("CREATE DATABASE {}", name.quoted())) {
            let err = super::db_error(e, &format!("cannot create database {name}"));
            if err.code != ExitCode::Conflict {
                return Err(err);
            }
        }
    }
    client.close();

    // 4. Report. The connection URL is the headline; psql shows the exact copy.
    let url = crate::cmd::db::tcp_url(&ctx, name.as_str());
    let socket_url = crate::cmd::db::socket_url(&ctx, name.as_str());
    let runtime_state = if started.already_running {
        "already running"
    } else {
        "started"
    };
    let db_state = if existed { "existing" } else { "created" };
    let text = format!(
        "PGBoxDB is ready.\n\n  {url}\n\n  database   {name} ({db_state})\n  runtime    {runtime_state} (pid {}, port {})\n  home       {}\n\nConnect:\n  psql \"{url}\"",
        started.pid,
        ctx.config.port,
        ctx.home.root.display(),
    );
    out.emit(
        &text,
        json!({
            "ready": true,
            "initialized": initialized,
            "started": started.started,
            "already_running": started.already_running,
            "pid": started.pid,
            "port": ctx.config.port,
            "host": "127.0.0.1",
            "user": "postgres",
            "database": name.as_str(),
            "database_created": !existed,
            "url": url,
            "socket_url": socket_url,
            "home": ctx.home.root,
            "durability": ctx.config.durability.as_str(),
        }),
    );
    Ok(ExitCode::Ok)
}
