//! Where things are: the installation prefix (relative to the `pgboxdb` binary)
//! and the data home (`$PGBOXDB_HOME`, default `~/.pgboxdb`).
use crate::cli::ExitCode;
use crate::errors::{PgboxdbError, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub prefix: PathBuf,
    pub engine: PathBuf,
    pub share: PathBuf,
    pub base_cluster: PathBuf,
    pub dist_json: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// prefix-relative path or short name of the thing that is wrong
    pub what: String,
    pub detail: String,
    pub fatal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DistInfo {
    pub pgboxdb_version: String,
    pub engine_commit: String,
    pub postgres_compat: String,
    pub initdb_version: String,
    pub os: String,
    pub arch: String,
    pub built_at: String,
}

impl Install {
    pub fn from_prefix(prefix: &Path) -> Install {
        Install {
            prefix: prefix.to_path_buf(),
            engine: prefix.join("bin/postgres"),
            share: prefix.join("share"),
            base_cluster: prefix.join("share/pgboxdb/base-cluster"),
            dist_json: prefix.join("share/pgboxdb/dist.json"),
        }
    }

    /// The prefix is the parent of the directory holding the (symlink-resolved) `pgboxdb` binary.
    pub fn discover() -> Result<Install> {
        let exe = std::env::current_exe().map_err(|e| {
            PgboxdbError::new(
                ExitCode::Corrupt,
                format!("cannot locate the pgboxdb executable: {e}"),
            )
        })?;
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        let bindir = exe.parent().ok_or_else(|| {
            PgboxdbError::new(
                ExitCode::Corrupt,
                "pgboxdb executable has no parent directory",
            )
        })?;
        let prefix = bindir.parent().ok_or_else(|| {
            PgboxdbError::new(
                ExitCode::Corrupt,
                "pgboxdb executable is not inside a <prefix>/bin directory",
            )
        })?;
        Ok(Install::from_prefix(prefix))
    }

    pub fn verify(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        let mut need = |rel: &str, kind: &str| {
            let p = self.prefix.join(rel);
            let ok = match kind {
                "dir" => p.is_dir(),
                _ => p.is_file(),
            };
            if !ok {
                out.push(Problem {
                    what: rel.to_string(),
                    detail: format!("missing {kind} {}", p.display()),
                    fatal: true,
                });
            }
        };
        need("bin/postgres", "file");
        need("share/timezone", "dir");
        need("share/timezonesets", "dir");
        need("share/tsearch_data", "dir");
        need("share/extension", "dir");
        need("share/pgboxdb/base-cluster/PG_VERSION", "file");
        need("share/pgboxdb/base-cluster/global/pg_control", "file");
        need("share/pgboxdb/base-cluster/base", "dir");
        need("share/pgboxdb/dist.json", "file");
        out
    }

    /// Fails with exit code 7 when the installation is incomplete.
    pub fn require_complete(&self) -> Result<()> {
        let problems = self.verify();
        if problems.is_empty() {
            return Ok(());
        }
        let list: Vec<String> = problems.iter().map(|p| p.detail.clone()).collect();
        Err(PgboxdbError::new(
            ExitCode::Corrupt,
            format!(
                "incomplete PGBoxDB installation at {}:\n  {}",
                self.prefix.display(),
                list.join("\n  ")
            ),
        )
        .hint("reinstall PGBoxDB (curl -fsSL https://pgboxdb.com/install | sh) or run `pgboxdb doctor`"))
    }

    pub fn dist_info(&self) -> Result<DistInfo> {
        let text = std::fs::read_to_string(&self.dist_json).map_err(|e| {
            PgboxdbError::new(
                ExitCode::Corrupt,
                format!("cannot read {}: {e}", self.dist_json.display()),
            )
        })?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            PgboxdbError::new(
                ExitCode::Corrupt,
                format!("{} is not valid JSON: {e}", self.dist_json.display()),
            )
        })?;
        let s = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or("unknown")
                .to_string()
        };
        Ok(DistInfo {
            pgboxdb_version: s("pgboxdb_version"),
            engine_commit: s("engine_commit"),
            postgres_compat: s("postgres_compat"),
            initdb_version: s("initdb_version"),
            os: s("os"),
            arch: s("arch"),
            built_at: s("built_at"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    pub root: PathBuf,
    pub data: PathBuf,
    pub run: PathBuf,
    pub log_file: PathBuf,
    pub config_file: PathBuf,
}

impl Home {
    pub fn from_env(pgboxdb_home: Option<&str>, home: Option<&str>) -> Home {
        let root = match (pgboxdb_home, home) {
            (Some(p), _) if !p.is_empty() => PathBuf::from(p),
            (_, Some(h)) if !h.is_empty() => Path::new(h).join(".pgboxdb"),
            _ => PathBuf::from(".pgboxdb"),
        };
        Home::at(root)
    }

    pub fn at(root: PathBuf) -> Home {
        Home {
            data: root.join("data"),
            run: root.join("run"),
            log_file: root.join("log/pgboxdb.log"),
            config_file: root.join("config.json"),
            root,
        }
    }

    pub fn resolve() -> Home {
        let pgboxdb_home = std::env::var("PGBOXDB_HOME").ok();
        let home = std::env::var("HOME").ok();
        Home::from_env(pgboxdb_home.as_deref(), home.as_deref())
    }

    pub fn is_initialized(&self) -> bool {
        self.data.join("PG_VERSION").is_file() && self.config_file.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn home_defaults_to_dot_pgboxdb() {
        let h = Home::from_env(None, Some("/Users/someone"));
        assert_eq!(h.root, Path::new("/Users/someone/.pgboxdb"));
        assert_eq!(h.data, Path::new("/Users/someone/.pgboxdb/data"));
        assert_eq!(h.run, Path::new("/Users/someone/.pgboxdb/run"));
        assert_eq!(
            h.log_file,
            Path::new("/Users/someone/.pgboxdb/log/pgboxdb.log")
        );
        assert_eq!(
            h.config_file,
            Path::new("/Users/someone/.pgboxdb/config.json")
        );
    }

    #[test]
    fn home_env_override() {
        let h = Home::from_env(Some("/tmp/my pgboxdb"), Some("/Users/someone"));
        assert_eq!(h.root, Path::new("/tmp/my pgboxdb"));
        assert_eq!(h.data, Path::new("/tmp/my pgboxdb/data"));
    }

    #[test]
    fn install_from_prefix_names_every_file() {
        let i = Install::from_prefix(Path::new("/opt/pgboxdb dist"));
        assert_eq!(i.engine, Path::new("/opt/pgboxdb dist/bin/postgres"));
        assert_eq!(i.share, Path::new("/opt/pgboxdb dist/share"));
        assert_eq!(
            i.base_cluster,
            Path::new("/opt/pgboxdb dist/share/pgboxdb/base-cluster")
        );
        assert_eq!(
            i.dist_json,
            Path::new("/opt/pgboxdb dist/share/pgboxdb/dist.json")
        );
    }

    #[test]
    fn install_verify_reports_each_missing_file() {
        let tmp = std::env::temp_dir().join(format!("pgboxdb-layout-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("bin")).unwrap();
        std::fs::create_dir_all(tmp.join("share/timezone")).unwrap();
        std::fs::write(tmp.join("bin/postgres"), b"").unwrap();
        let i = Install::from_prefix(&tmp);
        let problems = i.verify();
        let names: Vec<&str> = problems.iter().map(|p| p.what.as_str()).collect();
        assert!(names.contains(&"share/timezonesets"), "{names:?}");
        assert!(names.contains(&"share/tsearch_data"), "{names:?}");
        assert!(names.contains(&"share/extension"), "{names:?}");
        assert!(
            names.contains(&"share/pgboxdb/base-cluster/PG_VERSION"),
            "{names:?}"
        );
        assert!(names.contains(&"share/pgboxdb/dist.json"), "{names:?}");
        assert!(!names.contains(&"bin/postgres"), "{names:?}");
        assert!(!names.contains(&"share/timezone"), "{names:?}");
        assert!(problems.iter().all(|p| p.fatal));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn install_verify_clean_layout_has_no_problems() {
        let tmp = std::env::temp_dir().join(format!("pgboxdb-layout-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for d in [
            "bin",
            "share/timezone",
            "share/timezonesets",
            "share/tsearch_data",
            "share/extension",
            "share/pgboxdb/base-cluster/global",
            "share/pgboxdb/base-cluster/base",
        ] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
        }
        std::fs::write(tmp.join("bin/postgres"), b"").unwrap();
        std::fs::write(tmp.join("share/pgboxdb/base-cluster/PG_VERSION"), b"18\n").unwrap();
        std::fs::write(
            tmp.join("share/pgboxdb/base-cluster/global/pg_control"),
            b"",
        )
        .unwrap();
        std::fs::write(tmp.join("share/pgboxdb/dist.json"), br#"{"pgboxdb_version":"0.1.0","engine_commit":"abc","postgres_compat":"18.6","initdb_version":"18.6","os":"darwin","arch":"arm64","built_at":"now"}"#).unwrap();
        let i = Install::from_prefix(&tmp);
        assert!(i.verify().is_empty());
        let d = i.dist_info().unwrap();
        assert_eq!(d.pgboxdb_version, "0.1.0");
        assert_eq!(d.engine_commit, "abc");
        assert_eq!(d.postgres_compat, "18.6");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
