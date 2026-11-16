//! `$PGBOXDB_HOME/config.json`: what `pgboxdb init` decided and every later command reads.
use crate::cli::ExitCode;
use crate::errors::{PgboxdbError, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// The PGBoxDB profile: fsync off, synchronous_commit off, full_page_writes off.
    /// Survives a server crash; not an OS crash or power loss.
    #[default]
    Ephemeral,
    /// PostgreSQL defaults: fsync on, synchronous_commit on, full_page_writes on.
    Full,
}

impl Durability {
    pub fn parse(s: &str) -> Result<Durability> {
        match s {
            "ephemeral" => Ok(Durability::Ephemeral),
            "full" => Ok(Durability::Full),
            other => Err(PgboxdbError::usage(format!(
                "--durability must be `ephemeral` or `full`, got {other:?}"
            ))),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Durability::Ephemeral => "ephemeral",
            Durability::Full => "full",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub port: u16,
    pub password: String,
    pub durability: Durability,
    pub listen: Vec<String>,
    /// Unix socket directory when it cannot live under the home (path too long).
    pub run_dir: Option<PathBuf>,
    pub created_at: String,
}

impl Config {
    pub fn to_json(&self) -> String {
        let v = serde_json::json!({
            "port": self.port,
            "password": self.password,
            "durability": self.durability.as_str(),
            "listen": self.listen,
            "run_dir": self.run_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "created_at": self.created_at,
        });
        serde_json::to_string_pretty(&v).expect("config serialises")
    }

    pub fn from_json(text: &str) -> Result<Config> {
        let bad = |m: String| {
            PgboxdbError::new(ExitCode::Corrupt, format!("config.json: {m}"))
                .hint("re-run `pgboxdb init --force` to recreate it (this deletes the data directory)")
        };
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| bad(e.to_string()))?;
        let port = v
            .get("port")
            .and_then(|p| p.as_u64())
            .filter(|p| (1..=65535).contains(p))
            .ok_or_else(|| bad("port missing or invalid".into()))? as u16;
        let password = v
            .get("password")
            .and_then(|p| p.as_str())
            .ok_or_else(|| bad("password missing".into()))?
            .to_string();
        let durability = Durability::parse(
            v.get("durability")
                .and_then(|d| d.as_str())
                .unwrap_or("ephemeral"),
        )
        .map_err(|e| bad(e.message))?;
        let listen = v
            .get("listen")
            .and_then(|l| l.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| vec!["127.0.0.1".to_string(), "::1".to_string()]);
        let run_dir = v.get("run_dir").and_then(|r| r.as_str()).map(PathBuf::from);
        let created_at = v
            .get("created_at")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        Ok(Config {
            port,
            password,
            durability,
            listen,
            run_dir,
            created_at,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| PgboxdbError::io("cannot create", parent, &e))?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(&tmp)
                .map_err(|e| PgboxdbError::io("cannot write", &tmp, &e))?;
            f.write_all(self.to_json().as_bytes())
                .map_err(|e| PgboxdbError::io("cannot write", &tmp, &e))?;
            f.sync_all().ok();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| PgboxdbError::io("cannot chmod", &tmp, &e))?;
        }
        std::fs::rename(&tmp, path).map_err(|e| PgboxdbError::io("cannot rename into place", path, &e))
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                PgboxdbError::new(
                    ExitCode::NotReady,
                    format!("PGBoxDB is not initialised ({} is missing)", path.display()),
                )
                .hint("run `pgboxdb init`")
            } else {
                PgboxdbError::io("cannot read", path, &e)
            }
        })?;
        Config::from_json(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip() {
        let c = Config {
            port: 5433,
            password: "s3cret".into(),
            durability: Durability::Full,
            listen: vec!["127.0.0.1".into(), "::1".into()],
            run_dir: None,
            created_at: "2026-10-17T00:00:00Z".into(),
        };
        let json = c.to_json();
        let back = Config::from_json(&json).unwrap();
        assert_eq!(back, c);
        assert_eq!(
            Config::from_json(&json).unwrap().durability,
            Durability::Full
        );
    }

    #[test]
    fn durability_names() {
        assert_eq!(
            Durability::parse("ephemeral").unwrap(),
            Durability::Ephemeral
        );
        assert_eq!(Durability::parse("full").unwrap(), Durability::Full);
        assert!(Durability::parse("sorta").is_err());
        assert_eq!(Durability::Ephemeral.as_str(), "ephemeral");
        assert_eq!(Durability::default(), Durability::Ephemeral);
    }

    #[test]
    fn config_rejects_garbage() {
        assert!(Config::from_json("{").is_err());
        assert!(Config::from_json(r#"{"port":"five"}"#).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn config_file_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("pgboxdb-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let c = Config {
            port: 5432,
            password: "p".into(),
            durability: Durability::Ephemeral,
            listen: vec!["127.0.0.1".into()],
            run_dir: Some(dir.clone()),
            created_at: "t".into(),
        };
        c.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode {mode:o}");
        assert_eq!(Config::load(&path).unwrap(), c);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
