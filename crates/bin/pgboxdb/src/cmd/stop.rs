//! `pgboxdb stop [--force]`.
use super::Ctx;
use crate::cli::ExitCode;
use crate::errors::{PgboxdbError, Result};
use crate::out::Out;
use crate::process;
use std::time::Duration;

pub fn run(out: &Out, force: bool) -> Result<ExitCode> {
    let ctx = Ctx::load()?;
    // Stop the TTL supervisor first: it is tied to the runtime's lifetime, and
    // ending it here (rather than letting it notice on its next poll) frees the
    // single-instance lock so a restart or relocation can spawn a fresh one.
    super::start::stop_supervisor(&ctx.home);
    let pid_file = ctx.home.data.join("postmaster.pid");
    let Some(pid) = ctx.running_pid() else {
        if pid_file.exists() {
            out.note("removing a stale postmaster.pid");
            let _ = std::fs::remove_file(&pid_file);
        }
        out.emit(
            "PGBoxDB is not running",
            serde_json::json!({"stopped": false, "was_running": false}),
        );
        return Ok(ExitCode::Ok);
    };
    let mode = if force {
        process::StopMode::Force
    } else {
        process::StopMode::Fast
    };
    process::stop(pid, mode, Duration::from_secs(60), None)?;
    if pid_file.exists() {
        // A killed engine cannot remove its own pid file.
        std::fs::remove_file(&pid_file)
            .map_err(|e| PgboxdbError::io("cannot remove", &pid_file, &e))?;
    }
    out.emit(
        &format!("stopped PGBoxDB (pid {pid})"),
        serde_json::json!({"stopped": true, "was_running": true, "pid": pid, "forced": force}),
    );
    Ok(ExitCode::Ok)
}
