//! State written under the product's previous name: detection only.
//!
//! PGBoxDB does not accept any of these names as aliases. No previous
//! environment variable is read as configuration, no previous file name is
//! read as a project file, and no previous command name is installed. They
//! exist so that an existing installation is never silently ignored, forked
//! or overwritten, and so that `pgboxdb migrate` can adopt it on request.
//! Every previous name the program knows is in this file
//! (reports/PGBOXDB_COMPATIBILITY_MATRIX.md lists each one and why).
//!
//! The internal identifiers below are the exception: they are read in place,
//! because renaming them would detach existing data from its owner.
//! * `REPO_ID_FILE` / `WORKTREE_ID_FILE`: the Git integration's identity
//!   files inside `.git`. Read as a fallback and copied to the new name on
//!   first use (the old file is left untouched), so branch databases stay
//!   mapped to their repository.
//! * `PROFILE_CONF`: the generated settings file that an existing cluster's
//!   `postgresql.conf` includes. `start` keeps regenerating whichever file
//!   the cluster includes, so a migrated cluster keeps its port and socket.
use crate::cli::ExitCode;
use crate::errors::{PgboxdbError, Result};
use crate::layout::Home;
use std::path::{Path, PathBuf};

/// Data home under `$HOME`.
pub const HOME_DIR: &str = ".pgx";
/// Environment variable prefix. `ENV_NAMES` are the ones with a PGBOXDB_ counterpart.
pub const ENV_PREFIX: &str = "PGX_";
pub const ENV_NAMES: &[&str] = &[
    "HOME",
    "PORT",
    "DURABILITY",
    "LISTEN",
    "PASSWORD",
    "PG_BINDIR",
    "RUN_DIR",
    "SOURCE",
    "TTL_INTERVAL",
    "GIT",
    "GIT_SKIP",
];
/// Committed project file.
pub const PROJECT_FILE: &str = ".pgx.toml";
/// Git integration identity files (inside the Git directory).
pub const REPO_ID_FILE: &str = "pgx-repo-id";
pub const WORKTREE_ID_FILE: &str = "pgx-worktree-id";
/// Generated settings file included by an existing cluster's postgresql.conf.
pub const PROFILE_CONF: &str = "pgx.conf";
/// Substring identifying an agent hook command written by the previous CLI.
pub const AGENT_HOOK_MARKER: &str = "pgx agent hook";
/// Git hook block written by the previous CLI.
pub const GIT_HOOK_BEGIN: &str = "# >>> pgx git hook >>>";
pub const GIT_HOOK_END: &str = "# <<< pgx git hook <<<";
/// Shell rc block written by the previous CLI.
pub const RC_BEGIN: &str = "# >>> pgx git shell integration >>>";
pub const RC_END: &str = "# <<< pgx git shell integration <<<";

/// Where the previous CLI kept its data: its home variable if set, else `~/.pgx`.
pub fn home_from_env(legacy_home_var: Option<&str>, home: Option<&str>) -> Option<Home> {
    let root = match (legacy_home_var, home) {
        (Some(p), _) if !p.is_empty() => PathBuf::from(p),
        (_, Some(h)) if !h.is_empty() => Path::new(h).join(HOME_DIR),
        _ => return None,
    };
    Some(Home::at(root))
}

pub fn home() -> Option<Home> {
    let var = std::env::var(format!("{ENV_PREFIX}HOME")).ok();
    let home = std::env::var("HOME").ok();
    home_from_env(var.as_deref(), home.as_deref())
}

/// The previous data home, only when it holds an initialised cluster.
pub fn initialised_home() -> Option<Home> {
    home().filter(|h| h.is_initialized())
}

/// Previous environment variables that are set while their PGBOXDB_
/// counterpart is not: they are ignored, and the user should know.
pub fn ignored_env() -> Vec<(String, String)> {
    ENV_NAMES
        .iter()
        .filter(|n| {
            std::env::var_os(format!("{ENV_PREFIX}{n}")).is_some()
                && std::env::var_os(format!("PGBOXDB_{n}")).is_none()
        })
        .map(|n| (format!("{ENV_PREFIX}{n}"), format!("PGBOXDB_{n}")))
        .collect()
}

pub fn ignored_env_note() -> Option<String> {
    let v = ignored_env();
    if v.is_empty() {
        return None;
    }
    let old: Vec<&str> = v.iter().map(|(o, _)| o.as_str()).collect();
    let new: Vec<&str> = v.iter().map(|(_, n)| n.as_str()).collect();
    Some(format!(
        "pgboxdb: note: {} {} set but not read; PGBoxDB reads {}",
        old.join(", "),
        if old.len() == 1 { "is" } else { "are" },
        new.join(", ")
    ))
}

fn explicit_home() -> bool {
    std::env::var("PGBOXDB_HOME").is_ok_and(|v| !v.is_empty())
}

/// Called before a new, empty home is created. With `PGBOXDB_HOME` unset and
/// an initialised previous home present, refuse: creating `~/.pgboxdb` would
/// silently fork the user's databases. Setting PGBOXDB_HOME is the explicit
/// way to start a separate installation (or to use the old one in place).
pub fn guard_new_home(new: &Home) -> Result<()> {
    if explicit_home() {
        return Ok(());
    }
    let Some(old) = initialised_home() else {
        return Ok(());
    };
    if same_path(&old.root, &new.root) {
        return Ok(());
    }
    Err(PgboxdbError::new(
        ExitCode::Conflict,
        format!(
            "found databases from an installation made before the rename to PGBoxDB at {}; \
             PGBoxDB did not create a new, empty home at {}",
            old.root.display(),
            new.root.display()
        ),
    )
    .hint(format!(
        "`pgboxdb migrate` shows how to adopt them (`pgboxdb migrate --apply` moves {} to {}); \
         or keep them where they are with PGBOXDB_HOME={}; or start a separate, empty \
         installation with PGBOXDB_HOME={}",
        old.root.display(),
        new.root.display(),
        old.root.display(),
        new.root.display()
    )))
}

/// For "not initialised" errors: point at `pgboxdb migrate` when the reason is
/// a previous installation that has not been adopted yet.
pub fn uninitialised_hint(new: &Home) -> Option<String> {
    if explicit_home() {
        return None;
    }
    let old = initialised_home()?;
    (!same_path(&old.root, &new.root)).then(|| {
        format!(
            "databases from before the rename are at {}: `pgboxdb migrate` adopts them",
            old.root.display()
        )
    })
}

/// A project that still has only the project file written before the rename.
/// Its settings (the branch source, guards, the [git] table) would be ignored
/// and sessions would silently branch from the default source, so this is an
/// error: agent sessions fail closed and Git hooks skip.
pub fn project_file_error(dir: &Path) -> PgboxdbError {
    PgboxdbError::new(
        ExitCode::NotReady,
        format!(
            "{} is a project file from before the rename to PGBoxDB; PGBoxDB reads {}",
            dir.join(PROJECT_FILE).display(),
            crate::cmd::agent::project::CONFIG_FILE
        ),
    )
    .hint(format!(
        "run `pgboxdb migrate --apply` in {} (it copies the file to {} and leaves the original), then commit the new file",
        dir.display(),
        crate::cmd::agent::project::CONFIG_FILE
    ))
}

/// True when `dir` has the previous project file and not the current one.
pub fn only_legacy_project_file(dir: &Path) -> bool {
    dir.join(PROJECT_FILE).is_file() && !dir.join(crate::cmd::agent::project::CONFIG_FILE).is_file()
}

fn same_path(a: &Path, b: &Path) -> bool {
    let c = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    c(a) == c(b)
}

/// The settings file a cluster's postgresql.conf includes: the previous name
/// for a cluster created before the rename (its postgresql.conf is the
/// user's to edit and is never rewritten), the current name otherwise.
pub fn profile_conf_name(data: &Path) -> &'static str {
    let text = std::fs::read_to_string(data.join("postgresql.conf")).unwrap_or_default();
    let includes = |name: &str| {
        text.lines().any(|l| {
            let l = l.trim();
            !l.starts_with('#')
                && l.starts_with("include")
                && (l.contains(&format!("'{name}'")) || l.contains(&format!("\"{name}\"")))
        })
    };
    if includes(PROFILE_CONF) && !includes(crate::conf::PROFILE_CONF) {
        PROFILE_CONF
    } else {
        crate::conf::PROFILE_CONF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_home_defaults_and_override() {
        let h = home_from_env(None, Some("/Users/u")).unwrap();
        assert_eq!(h.root, Path::new("/Users/u/.pgx"));
        let h = home_from_env(Some("/srv/x"), Some("/Users/u")).unwrap();
        assert_eq!(h.root, Path::new("/srv/x"));
        assert!(home_from_env(None, None).is_none());
    }

    #[test]
    fn profile_name_follows_the_include_line() {
        let d = std::env::temp_dir().join(format!("pgboxdb-legacy-conf-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("postgresql.conf"),
            "work_mem = 8MB\ninclude 'pgx.conf'\n",
        )
        .unwrap();
        assert_eq!(profile_conf_name(&d), "pgx.conf");
        std::fs::write(d.join("postgresql.conf"), "include 'pgboxdb.conf'\n").unwrap();
        assert_eq!(profile_conf_name(&d), "pgboxdb.conf");
        std::fs::write(
            d.join("postgresql.conf"),
            "# include 'pgx.conf'\ninclude 'pgboxdb.conf'\n",
        )
        .unwrap();
        assert_eq!(profile_conf_name(&d), "pgboxdb.conf");
        std::fs::remove_file(d.join("postgresql.conf")).unwrap();
        assert_eq!(profile_conf_name(&d), "pgboxdb.conf");
        std::fs::remove_dir(&d).unwrap();
    }
}
