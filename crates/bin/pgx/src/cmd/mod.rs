pub mod agent;
pub mod copy;
pub mod db;
pub mod doctor;
pub mod git;
pub mod init;
pub mod run;
pub mod start;
pub mod status;
pub mod stop;
pub mod supervise;
pub mod top;
pub mod ttl;
pub mod version;

use crate::cli::ExitCode;
use crate::config::Config;
use crate::errors::{PgxError, Result};
use crate::layout::{Home, Install};
use std::path::PathBuf;

/// What every post-init command needs: the installation, the home, its config.
pub struct Ctx {
    pub install: Install,
    pub home: Home,
    pub config: Config,
}

impl Ctx {
    pub fn load() -> Result<Ctx> {
        let install = Install::discover()?;
        install.require_complete()?;
        let home = Home::resolve();
        let config = Config::load(&home.config_file)?;
        Ok(Ctx {
            install,
            home,
            config,
        })
    }

    pub fn sock_dir(&self) -> PathBuf {
        self.config
            .run_dir
            .clone()
            .unwrap_or_else(|| self.home.run.clone())
    }

    /// A stable identity for this cluster incarnation: the canonical data-dir
    /// path plus the init timestamp. `pgx init --force` rewrites `created_at`,
    /// so a re-initialised cluster at the same path gets a new identity and
    /// cannot match TTL metadata from the previous one.
    pub fn cluster_id(&self) -> String {
        let data =
            std::fs::canonicalize(&self.home.data).unwrap_or_else(|_| self.home.data.clone());
        format!("{}#{}", data.display(), self.config.created_at)
    }

    /// The pid of a running engine on this data directory, if any (stale pid files are ignored).
    pub fn running_pid(&self) -> Option<u32> {
        let pf = crate::process::read_pid_file(&self.home.data)?;
        if crate::process::is_our_engine(pf.pid, &self.home.data) {
            Some(pf.pid)
        } else {
            None
        }
    }

    pub fn require_running(&self) -> Result<u32> {
        self.running_pid().ok_or_else(|| {
            PgxError::new(ExitCode::NotReady, "PGX is not running").hint("run `pgx start`")
        })
    }

    pub fn connect(
        &self,
        database: &str,
    ) -> Result<crate::pgwire::Client<std::os::unix::net::UnixStream>> {
        self.require_running()?;
        crate::pgwire::Client::connect_unix(
            &self.sock_dir(),
            self.config.port,
            "postgres",
            database,
            std::time::Duration::from_secs(30),
        )
        .map_err(|e| {
            PgxError::failure(format!(
                "cannot connect to the runtime on {}: {e}",
                crate::pgwire::Client::socket_path(&self.sock_dir(), self.config.port).display()
            ))
        })
    }
}

/// Maps a database error to the CLI's exit codes.
pub fn db_error(e: crate::pgwire::WireError, context: &str) -> PgxError {
    match e {
        crate::pgwire::WireError::Db(d) => {
            let code = match d.sqlstate.as_str() {
                "3D000" => ExitCode::NotFound,
                "42P04" => ExitCode::Conflict,
                "55006" => ExitCode::InUse,
                _ => ExitCode::Failure,
            };
            let mut err = PgxError::new(code, format!("{context}: {}", d.message));
            if let Some(h) = d.hint {
                err = err.hint(h);
            }
            err
        }
        other => PgxError::failure(format!("{context}: {other}")),
    }
}
