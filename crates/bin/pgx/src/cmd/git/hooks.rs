//! Git hook installation. Non-destructive by construction:
//!
//! - a missing hook gets a small script PGX owns entirely;
//! - an existing hook gets a marked block inserted right after its shebang
//!   (so an `exit` later in the script cannot skip it), its own lines and
//!   permissions untouched; the block never exits, never reads stdin (Git
//!   feeds `post-rewrite` on stdin) and always succeeds;
//! - a hook that Git tracks (a committed `core.hooksPath` such as `.husky/`)
//!   or a symlink is left alone, with the one line to add by hand.
//!
//! The block calls `pgx git hook <name> "$@"`: Git's arguments are passed as
//! data, nothing from the repository is evaluated, and no credential or URL
//! is ever written into a hook.
use super::env::strip_block;
use super::repo::Repo;
use super::state::HookRecord;
use crate::errors::{PgxError, Result};
use std::path::{Path, PathBuf};

pub const HOOKS: [&str; 3] = ["post-checkout", "post-merge", "post-rewrite"];
pub const BEGIN: &str = "# >>> pgx git hook >>>";
pub const END: &str = "# <<< pgx git hook <<<";

fn sq(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

pub fn block(name: &str, pgx: &Path) -> String {
    format!(
        "{BEGIN}\n# Added by `pgx git init`; removed by `pgx git uninstall`. Never blocks Git.\n\
if [ -z \"${{PGX_GIT_SKIP-}}\" ]; then\n\
  __pgx_bin={bin}\n\
  if [ ! -x \"$__pgx_bin\" ]; then __pgx_bin=\"$(command -v pgx 2>/dev/null || true)\"; fi\n\
  if [ -n \"$__pgx_bin\" ]; then \"$__pgx_bin\" git hook {name} \"$@\" </dev/null || true; fi\n\
fi\n{END}\n",
        bin = sq(&pgx.to_string_lossy())
    )
}

#[derive(Debug, Clone)]
pub struct HookReport {
    pub name: String,
    pub path: PathBuf,
    /// `created`, `inserted`, `present`, `skipped`.
    pub action: String,
    pub detail: Option<String>,
}

fn pgx_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pgx"))
}

fn write_atomic(p: &Path, text: &str, keep_mode_of: Option<&Path>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let tmp = p.with_extension(format!("pgx-tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| PgxError::io("cannot write", &tmp, &e))?;
    let mode = keep_mode_of
        .and_then(|k| std::fs::metadata(k).ok())
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0o755);
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    std::fs::rename(&tmp, p).map_err(|e| PgxError::io("cannot replace", p, &e))
}

/// Where the effective hooks directory lives, which decides whether PGX may
/// write into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HooksDir {
    /// Inside this repository's Git directory: private to the repository.
    Private(PathBuf),
    /// Inside the work tree (`core.hooksPath = .husky/_`, `.githooks`):
    /// owned by a hook manager or committed; its files get regenerated.
    InWorktree(PathBuf),
    /// Outside the repository (a global `core.hooksPath`): shared by every
    /// repository on the machine.
    Shared(PathBuf),
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn classify(repo: &Repo) -> HooksDir {
    let dir = repo.hooks_dir();
    // The directory may not exist yet: judge by its nearest existing parent.
    let mut probe = dir.clone();
    let mut tail = Vec::new();
    while !probe.exists() {
        match (probe.file_name().map(|n| n.to_os_string()), probe.parent()) {
            (Some(n), Some(p)) => {
                tail.push(n);
                probe = p.to_path_buf();
            }
            _ => break,
        }
    }
    let mut c = canon(&probe);
    for n in tail.iter().rev() {
        c.push(n);
    }
    if c.starts_with(&repo.common_dir) {
        HooksDir::Private(dir)
    } else if c.starts_with(&repo.toplevel) {
        HooksDir::InWorktree(dir)
    } else {
        HooksDir::Shared(dir)
    }
}

pub fn install(repo: &Repo) -> Result<(Vec<HookReport>, Vec<HookRecord>)> {
    let dir = match classify(repo) {
        HooksDir::Private(d) => d,
        other => {
            let (d, why) = match &other {
                HooksDir::InWorktree(d) => (d.clone(), format!(
                    "core.hooksPath points into the work tree ({}), which a hook manager or the repository owns (husky regenerates `.husky/_`); add this line to your own hook (for husky: `.husky/<hook>`)",
                    d.display()
                )),
                HooksDir::Shared(d) => (d.clone(), format!(
                    "core.hooksPath ({}) is shared by every repository on this machine, so PGX does not write there (uninstalling in one repository would remove it from all); add this line to the shared hook yourself (it is a no-op in repositories without PGX Git)",
                    d.display()
                )),
                HooksDir::Private(_) => unreachable!(),
            };
            let reports = HOOKS
                .iter()
                .map(|name| HookReport {
                    name: name.to_string(),
                    path: d.join(name),
                    action: "skipped".into(),
                    detail: Some(format!(
                        "{why}: pgx git hook {name} \"$@\" </dev/null || true"
                    )),
                })
                .collect();
            return Ok((reports, Vec::new()));
        }
    };
    std::fs::create_dir_all(&dir).map_err(|e| PgxError::io("cannot create", &dir, &e))?;
    let pgx = pgx_path();
    let mut reports = Vec::new();
    let mut records = Vec::new();
    for name in HOOKS {
        let path = dir.join(name);
        let blk = block(name, &pgx);
        let md = std::fs::symlink_metadata(&path).ok();
        let manual = format!(
            "add this line near the top of {}: pgx git hook {name} \"$@\" </dev/null || true",
            path.display()
        );
        if md.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
            reports.push(HookReport {
                name: name.into(),
                path,
                action: "skipped".into(),
                detail: Some(format!(
                    "symlinked hook (managed by another tool); {manual}"
                )),
            });
            continue;
        }
        if md.is_some() && path.starts_with(&repo.toplevel) && repo.is_tracked(&path) {
            reports.push(HookReport {
                name: name.into(),
                path,
                action: "skipped".into(),
                detail: Some(format!("the hook is committed to the repository; {manual}")),
            });
            continue;
        }
        match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                write_atomic(&path, &format!("#!/bin/sh\n{blk}"), None)?;
                records.push(HookRecord {
                    name: name.into(),
                    path: path.to_string_lossy().into_owned(),
                    mode: "file".into(),
                });
                reports.push(HookReport {
                    name: name.into(),
                    path,
                    action: "created".into(),
                    detail: None,
                });
            }
            Err(e) => return Err(PgxError::io("cannot read", &path, &e)),
            Ok(bytes) => {
                let Ok(text) = String::from_utf8(bytes) else {
                    reports.push(HookReport {
                        name: name.into(),
                        path,
                        action: "skipped".into(),
                        detail: Some(format!("not a text script; {manual}")),
                    });
                    continue;
                };
                // Only a POSIX-family script can carry a POSIX-sh block.
                let first = text.lines().next().unwrap_or("");
                let interp = first.strip_prefix("#!").map(|s| {
                    let mut w = s.split_whitespace();
                    let a = w.next().unwrap_or("");
                    let a = if a.ends_with("/env") {
                        w.next().unwrap_or("")
                    } else {
                        a
                    };
                    a.rsplit('/').next().unwrap_or("").to_string()
                });
                if let Some(i) = &interp {
                    if !matches!(i.as_str(), "sh" | "bash" | "zsh" | "dash" | "ksh" | "ash") {
                        reports.push(HookReport {
                            name: name.into(),
                            path,
                            action: "skipped".into(),
                            detail: Some(format!("a {i} script, not a shell script; {manual}")),
                        });
                        continue;
                    }
                }
                let owned_before = text.starts_with("#!/bin/sh\n# >>> pgx git hook >>>")
                    && strip_block(&text, BEGIN, END).is_some_and(|r| r.trim() == "#!/bin/sh");
                let mode = if owned_before { "file" } else { "block" };
                // Refresh our block in place (the pgx path may have moved).
                let base = strip_block(&text, BEGIN, END);
                let present = base.is_some();
                let base = base.unwrap_or_else(|| text.clone());
                let new_text = match base.split_once('\n') {
                    Some((first, rest)) if first.starts_with("#!") => {
                        format!("{first}\n{blk}{rest}")
                    }
                    _ if base.starts_with("#!") => format!("{base}\n{blk}"),
                    _ => format!("#!/bin/sh\n{blk}{base}"),
                };
                if new_text != text {
                    write_atomic(&path, &new_text, Some(&path))?;
                }
                if !owned_before {
                    // An existing hook that was not executable did not run before;
                    // making it run now would change its behaviour. Say so instead.
                    use std::os::unix::fs::PermissionsExt;
                    let exec = std::fs::metadata(&path)
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false);
                    if !exec {
                        reports.push(HookReport { name: name.into(), path: path.clone(), action: "inserted".into(), detail: Some("the existing hook is not executable, so Git does not run it (nor PGX's block); `chmod +x` it to enable both".into()) });
                        records.push(HookRecord {
                            name: name.into(),
                            path: path.to_string_lossy().into_owned(),
                            mode: mode.into(),
                        });
                        continue;
                    }
                }
                records.push(HookRecord {
                    name: name.into(),
                    path: path.to_string_lossy().into_owned(),
                    mode: mode.into(),
                });
                reports.push(HookReport {
                    name: name.into(),
                    path,
                    action: if present { "present" } else { "inserted" }.into(),
                    detail: None,
                });
            }
        }
    }
    Ok((reports, records))
}

/// Remove exactly what PGX added: its block from shared hooks, its whole
/// file only when the file is still nothing but PGX's block.
pub fn uninstall(repo: &Repo) -> Result<Vec<HookReport>> {
    // Only ever this repository's own hooks directory (see `install`).
    let HooksDir::Private(dir) = classify(repo) else {
        return Ok(Vec::new());
    };
    let mut reports = Vec::new();
    for name in HOOKS {
        let path = dir.join(name);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(rest) = strip_block(&text, BEGIN, END) else {
            continue;
        };
        if rest.trim() == "#!/bin/sh" || rest.trim().is_empty() {
            std::fs::remove_file(&path).map_err(|e| PgxError::io("cannot remove", &path, &e))?;
            reports.push(HookReport {
                name: name.into(),
                path,
                action: "removed".into(),
                detail: None,
            });
        } else {
            write_atomic(&path, &rest, Some(&path))?;
            reports.push(HookReport {
                name: name.into(),
                path,
                action: "block removed".into(),
                detail: None,
            });
        }
    }
    Ok(reports)
}

/// Which hooks currently carry PGX's block.
pub fn status(repo: &Repo) -> Vec<(String, PathBuf, bool)> {
    let dir = repo.hooks_dir();
    HOOKS
        .iter()
        .map(|n| {
            let p = dir.join(n);
            let ok = std::fs::read_to_string(&p).is_ok_and(|t| t.contains(BEGIN));
            (n.to_string(), p, ok)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_survives_sh_e_without_pgx() {
        let b = block("post-checkout", Path::new("/no/such/pgx"));
        let script = format!("{b}echo user-hook-ran\n");
        let out = std::process::Command::new("/bin/sh")
            .arg("-e")
            .arg("-c")
            .arg(&script)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "user-hook-ran\n");
    }

    #[test]
    fn the_block_is_inert_text_for_a_posix_shell() {
        let b = block("post-checkout", Path::new("/no/such/pgx"));
        assert!(b.starts_with(BEGIN) && b.trim_end().ends_with(END));
        assert!(b.contains("</dev/null || true"));
        assert!(!b.contains("://"));
        // With no pgx anywhere the block is a no-op that still exits 0.
        let script = format!("#!/bin/sh\n{b}exit 0\n");
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
