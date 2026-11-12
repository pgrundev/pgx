//! Argument parsing, help text and the stable exit codes.
use crate::errors::{PgxError, Result};

/// Exit codes are part of the agent-facing interface (docs/pgx/local-dev/cli.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Ok = 0,
    /// engine or runtime error
    Failure = 1,
    Usage = 2,
    /// not initialised, or not running
    NotReady = 3,
    /// already exists, already running, port in use
    Conflict = 4,
    NotFound = 5,
    /// sessions hold the database
    InUse = 6,
    /// corrupted or incomplete installation
    Corrupt = 7,
}

impl ExitCode {
    pub fn name(self) -> &'static str {
        match self {
            ExitCode::Ok => "ok",
            ExitCode::Failure => "failure",
            ExitCode::Usage => "usage",
            ExitCode::NotReady => "not_ready",
            ExitCode::Conflict => "conflict",
            ExitCode::NotFound => "not_found",
            ExitCode::InUse => "in_use",
            ExitCode::Corrupt => "corrupt",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Global {
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Init {
        port: Option<u16>,
        durability: Option<String>,
        force: bool,
    },
    /// One command to a ready database: init if needed, start if needed, ensure
    /// the default `app` database, print its URL. Idempotent.
    Run,
    Start,
    /// Run the engine in the foreground (init if needed); the Docker/systemd entrypoint.
    Serve,
    Stop {
        force: bool,
    },
    Restart,
    Status,
    Create {
        name: String,
        /// `--ttl <dur>`; None means the default (unlimited).
        ttl: Option<String>,
    },
    Drop {
        name: String,
        force: bool,
    },
    Clone {
        source: String,
        target: String,
        /// `--ttl <dur>`; None means unlimited (TTL is never inherited from the source).
        ttl: Option<String>,
    },
    /// `pgx branch <source> <name> [--ttl <dur>]`: a clone for one agent with a
    /// 2h default TTL; prints the connection URL last. None means the default.
    Branch {
        source: String,
        name: String,
        ttl: Option<String>,
    },
    Ls,
    /// `pgx ttl <db>` shows the TTL; `pgx ttl <db> <dur>` sets it (dur may be `unlimited`).
    Ttl {
        name: String,
        value: Option<String>,
    },
    Url {
        name: String,
        socket: bool,
    },
    /// `pgx copy --from <url> --to <db>`: import an external PostgreSQL database.
    Copy(crate::cmd::copy::Opts),
    /// `pgx agent …`: one isolated database per coding-agent session.
    Agent(crate::cmd::agent::AgentCmd),
    /// `pgx git …`: one isolated database per Git branch.
    Git(crate::cmd::git::GitCmd),
    /// `pgx top`: the live, read-only monitor.
    Top(crate::cmd::top::TopOpts),
    Doctor,
    /// Hidden: the background TTL cleanup worker, spawned by start/run/serve.
    Supervise,
    Version,
    Help,
}

pub fn parse(args: &[String]) -> Result<(Global, Command)> {
    let mut global = Global::default();
    let mut rest: Vec<&str> = Vec::new();
    // Everything after a bare `--` belongs to a child command (`pgx agent
    // exec -- codex --json`), so global flags stop there.
    let mut passthrough = false;
    let mut help = false;
    for a in args {
        if passthrough {
            rest.push(a.as_str());
            continue;
        }
        match a.as_str() {
            "--" => {
                passthrough = true;
                rest.push("--");
            }
            "--json" => global.json = true,
            "--help" | "-h" => help = true,
            "--version" | "-V" => return Ok((global, Command::Version)),
            other => rest.push(other),
        }
    }
    if help {
        // `pgx top --help` is the monitor's own page; everything else keeps
        // the global help it always printed.
        if rest.first() == Some(&"top") {
            return Ok((
                global,
                Command::Top(crate::cmd::top::TopOpts {
                    help: true,
                    ..Default::default()
                }),
            ));
        }
        return Ok((global, Command::Help));
    }
    let Some((&name, tail)) = rest.split_first() else {
        return Ok((global, Command::Help));
    };
    if name == "top" {
        return Ok((global, Command::Top(crate::cmd::top::parse(tail)?)));
    }
    if name == "agent" {
        return Ok((global, Command::Agent(crate::cmd::agent::parse(tail)?)));
    }
    if name == "git" {
        return Ok((global, Command::Git(crate::cmd::git::parse_args(tail)?)));
    }
    let mut positional: Vec<&str> = Vec::new();
    let mut flags: Vec<(&str, Option<&str>)> = Vec::new();
    let mut i = 0;
    while i < tail.len() {
        let t = tail[i];
        if let Some(flag) = t.strip_prefix("--") {
            if let Some((k, v)) = flag.split_once('=') {
                flags.push((k, Some(v)));
            } else if matches!(flag, "port" | "durability" | "ttl" | "from" | "to" | "jobs") {
                let v = tail
                    .get(i + 1)
                    .copied()
                    .ok_or_else(|| PgxError::usage(format!("--{flag} needs a value")))?;
                flags.push((flag, Some(v)));
                i += 1;
            } else {
                flags.push((flag, None));
            }
        } else {
            positional.push(t);
        }
        i += 1;
    }
    let only_flags = |allowed: &[&str]| -> Result<()> {
        for (k, _) in &flags {
            if !allowed.contains(k) {
                return Err(PgxError::usage(format!("pgx {name}: unknown option --{k}")));
            }
        }
        Ok(())
    };
    let has = |k: &str| flags.iter().any(|(f, _)| *f == k);
    let value = |k: &str| flags.iter().find(|(f, _)| *f == k).and_then(|(_, v)| *v);
    let arity = |n: usize, usage: &str| -> Result<()> {
        if positional.len() != n {
            return Err(PgxError::usage(format!("usage: pgx {usage}")));
        }
        Ok(())
    };
    let cmd = match name {
        "init" => {
            only_flags(&["port", "durability", "force"])?;
            arity(0, "init [--port N] [--durability ephemeral|full] [--force]")?;
            let port =
                match value("port") {
                    None => None,
                    Some(p) => Some(p.parse::<u16>().ok().filter(|p| *p >= 1024).ok_or_else(
                        || {
                            PgxError::usage(format!(
                                "--port must be a number between 1024 and 65535, got {p:?}"
                            ))
                        },
                    )?),
                };
            Command::Init {
                port,
                durability: value("durability").map(String::from),
                force: has("force"),
            }
        }
        "run" => {
            only_flags(&[])?;
            arity(0, "run")?;
            Command::Run
        }
        "start" => {
            only_flags(&[])?;
            arity(0, "start")?;
            Command::Start
        }
        "serve" => {
            only_flags(&[])?;
            arity(0, "serve")?;
            Command::Serve
        }
        "stop" => {
            only_flags(&["force"])?;
            arity(0, "stop [--force]")?;
            Command::Stop {
                force: has("force"),
            }
        }
        "restart" => {
            only_flags(&[])?;
            arity(0, "restart")?;
            Command::Restart
        }
        "status" => {
            only_flags(&[])?;
            arity(0, "status")?;
            Command::Status
        }
        "create" => {
            only_flags(&["ttl"])?;
            arity(1, "create <database> [--ttl <duration>]")?;
            Command::Create {
                name: positional[0].into(),
                ttl: value("ttl").map(String::from),
            }
        }
        "drop" => {
            only_flags(&["force"])?;
            arity(1, "drop <database> [--force]")?;
            Command::Drop {
                name: positional[0].into(),
                force: has("force"),
            }
        }
        "clone" => {
            only_flags(&["ttl"])?;
            arity(2, "clone <source> <target> [--ttl <duration>]")?;
            Command::Clone {
                source: positional[0].into(),
                target: positional[1].into(),
                ttl: value("ttl").map(String::from),
            }
        }
        "branch" => {
            only_flags(&["ttl"])?;
            arity(2, "branch <source> <name> [--ttl <duration>]")?;
            Command::Branch {
                source: positional[0].into(),
                name: positional[1].into(),
                ttl: value("ttl").map(String::from),
            }
        }
        "ls" | "list" => {
            only_flags(&[])?;
            arity(0, "ls")?;
            Command::Ls
        }
        "ttl" => {
            only_flags(&[])?;
            if positional.is_empty() || positional.len() > 2 {
                return Err(PgxError::usage(
                    "usage: pgx ttl <database> [<duration>|unlimited]".to_string(),
                ));
            }
            Command::Ttl {
                name: positional[0].into(),
                value: positional.get(1).map(|s| (*s).to_string()),
            }
        }
        "supervise" => {
            only_flags(&[])?;
            arity(0, "supervise")?;
            Command::Supervise
        }
        "url" => {
            only_flags(&["socket"])?;
            arity(1, "url <database> [--socket]")?;
            Command::Url {
                name: positional[0].into(),
                socket: has("socket"),
            }
        }
        "copy" => {
            only_flags(&[
                "from",
                "to",
                "schema-only",
                "jobs",
                "verbose",
                "no-optimize",
            ])?;
            arity(
                0,
                "copy --from <postgres-url> --to <database> [--schema-only] [--jobs N] [--verbose] [--no-optimize]",
            )?;
            let from = value("from")
                .filter(|v| !v.is_empty())
                .ok_or_else(|| PgxError::usage("pgx copy: --from <postgres-url> is required"))?;
            let to = value("to")
                .filter(|v| !v.is_empty())
                .ok_or_else(|| PgxError::usage("pgx copy: --to <database> is required"))?;
            let jobs = match value("jobs") {
                None => None,
                Some(j) => Some(
                    j.parse::<u32>()
                        .ok()
                        .filter(|j| (1..=64).contains(j))
                        .ok_or_else(|| {
                            PgxError::usage(format!(
                                "--jobs must be a number between 1 and 64, got {j:?}"
                            ))
                        })?,
                ),
            };
            Command::Copy(crate::cmd::copy::Opts {
                from: from.to_string(),
                to: to.to_string(),
                schema_only: has("schema-only"),
                jobs,
                verbose: has("verbose"),
                optimize: !has("no-optimize"),
            })
        }
        "doctor" => {
            only_flags(&[])?;
            arity(0, "doctor")?;
            Command::Doctor
        }
        "version" => {
            only_flags(&[])?;
            arity(0, "version")?;
            Command::Version
        }
        "help" => Command::Help,
        other => return Err(PgxError::usage(format!("unknown command {other:?}"))),
    };
    Ok((global, cmd))
}

pub fn help_text() -> String {
    format!(
        "pgx {} — a local, disposable, PostgreSQL-compatible runtime

Usage: pgx [--json] <command> [arguments]

Quick start
  pgx run                 get a ready database in one step: init if needed,
                          start if needed, ensure the `app` database, print its
                          URL. Idempotent and safe to repeat; never resets data.

Runtime
  pgx init [--port N] [--durability ephemeral|full] [--force]
                          create the cluster under $PGX_HOME (default ~/.pgx)
  pgx start               start the runtime (idempotent)
  pgx serve               run the engine in the foreground (init if needed; for Docker/systemd)
  pgx stop [--force]      stop it (fast shutdown; --force kills after the timeout)
  pgx restart
  pgx status              running?, pid, port, databases, memory

Databases
  pgx create <database> [--ttl <dur>]   --ttl makes it temporary (auto-expires)
  pgx drop <database> [--force]     --force terminates connected sessions first
  pgx clone <source> <target> [--ttl <dur>]   copy-on-write where supported; TTL not inherited
  pgx branch <source> <name> [--ttl <dur>]    a disposable database for one agent: a clone
                                    that expires after 2h unless --ttl says otherwise;
                                    prints its connection URL as the last line
  pgx ls                            name, status, TTL, expires-in
  pgx ttl <database> [<dur>]        show the TTL, or set it (<dur> or `unlimited`)
  pgx url <database> [--socket]     connection URL (TCP with password, or Unix socket)

Import from PostgreSQL
  pgx copy --from <postgres-url> --to <database> [--schema-only] [--jobs N]
                          copy an existing PostgreSQL database into PGX (pg_dump →
                          pg_restore, read-only on the source) as a template to
                          branch from: `pgx copy --from \"$DATABASE_URL\" --to myapp`,
                          then `pgx branch myapp agent-1` per agent. Needs pg_dump/pg_restore on PATH or in
                          PGX_PG_BINDIR. --schema-only skips table data (migration
                          bookkeeping rows such as schema_migrations are kept);
                          --no-optimize skips VACUUM (FREEZE, ANALYZE); --verbose
                          shows the tools' output.

  TTL durations: 30s 5m 15m 1h 6h 24h 7d (general s/m/h/d), or `unlimited`
  (the default). Temporary databases are removed automatically by a background
  worker; permanent databases are never auto-deleted.

Agents (one isolated database per coding-agent session)
  pgx agent setup [--source <db>] [--hosts claude,cursor,codex] [--scope project|local|user]
                          one-time project setup: writes .pgx.toml, installs lifecycle
                          hooks for the detected hosts, verifies them
  pgx agent doctor        check hooks, .pgx.toml, the source template, the runtime
  pgx agent exec [--id <session>] [--agent <id>] [--source <db>] [--ttl <dur>] [--keep] -- <command> [args…]
                          run any command (codex, tests, a script) with its own branch
                          in DATABASE_URL; drop the branch when it exits, or with
                          --keep leave it active for its idle TTL (the next exec with
                          the same --id attaches to it). Inside a Claude Code or Codex
                          session it attaches to that session's branch instead.
  pgx agent ls            agent branches: host, session, agent, source, expires-in
  pgx agent activity --state working|migrating|testing|completed|failed [--task <label>]
                          report this session's task phase for `pgx top` (explicit only;
                          never changes the branch, its TTL or its lifecycle)
  pgx agent up|touch|down|env|hook|install|uninstall
                          lifecycle primitives (see docs/pgx/local-dev/agents.md)

Git (one Git branch, one isolated database, automatically)
  pgx git init [--source <db|postgres-url>] [--main <branch>] [--schema-only|--with-data] [--no-hooks]
                          set up the repository: main database, first main snapshot,
                          post-checkout/post-merge/post-rewrite hooks (safe to rerun)
  pgx git status          branch, database, main snapshot vs branch baseline, sync state
  pgx git ls              Git-managed databases: branch, database, base, status
  pgx git sync [--force] [--migrate]
                          refresh the immutable main snapshot when main changed
  pgx git env [--shell bash|zsh|fish] [--reveal]
                          connection variables for the current branch (password masked unless --shell or --reveal)
  pgx git exec -- <command> [args…]
                          run a command with the current branch's database
  pgx git reset --from-main [--force]
                          recreate this branch's database from the latest main snapshot
  pgx git prune [--apply] [--grace <dur>]
                          abandoned branch databases (dry run unless --apply)
  pgx git doctor | uninstall [--shell] | shell <sh> | install-shell [<sh>]

Monitor
  pgx top [--interval-ms N] [--no-color] [--project <filter>]
                          live, read-only terminal view of the runtime, its databases
                          and agent sessions (connections, TTLs, lifecycle, task phase)
  pgx top --demo          the same view on a simulated five-agent session (no PGX needed)
  pgx [--json] top --snapshot [--demo]
                          one collection as text, or one JSON object with --json

Tools
  pgx doctor              check the installation, data directory, port, storage
  pgx version
  pgx help

Options
  --json                  machine-readable output on stdout (also for errors)

Environment
  PGX_HOME                data home (default ~/.pgx)
  PGX_PORT                port for the first `pgx run`/`pgx serve` (else auto 5432-5500)
  PGX_DURABILITY          ephemeral (default) or full, for the first init
  PGX_PASSWORD            superuser password for the first init (else random)
  PGX_PG_BINDIR           directory with pg_dump and pg_restore for `pgx copy`

Exit codes
  0 ok · 1 engine or runtime error · 2 usage · 3 not initialised / not running
  4 conflict (exists, running, port in use) · 5 not found · 6 database in use
  7 corrupted or incomplete installation
",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(ExitCode::Ok as i32, 0);
        assert_eq!(ExitCode::Failure as i32, 1);
        assert_eq!(ExitCode::Usage as i32, 2);
        assert_eq!(ExitCode::NotReady as i32, 3);
        assert_eq!(ExitCode::Conflict as i32, 4);
        assert_eq!(ExitCode::NotFound as i32, 5);
        assert_eq!(ExitCode::InUse as i32, 6);
        assert_eq!(ExitCode::Corrupt as i32, 7);
    }

    #[test]
    fn parse_global_json_flag() {
        let (g, c) = parse(&args("--json status")).unwrap();
        assert!(g.json);
        assert_eq!(c, Command::Status);
        let (g, c) = parse(&args("status --json")).unwrap();
        assert!(g.json);
        assert_eq!(c, Command::Status);
    }

    #[test]
    fn parse_subcommands_with_arguments() {
        assert_eq!(
            parse(&args("create myapp")).unwrap().1,
            Command::Create {
                name: "myapp".into(),
                ttl: None
            }
        );
        assert_eq!(
            parse(&args("create agent-1 --ttl 5m")).unwrap().1,
            Command::Create {
                name: "agent-1".into(),
                ttl: Some("5m".into())
            }
        );
        assert_eq!(
            parse(&args("drop myapp --force")).unwrap().1,
            Command::Drop {
                name: "myapp".into(),
                force: true
            }
        );
        assert_eq!(
            parse(&args("drop myapp")).unwrap().1,
            Command::Drop {
                name: "myapp".into(),
                force: false
            }
        );
        assert_eq!(
            parse(&args("clone tpl t1")).unwrap().1,
            Command::Clone {
                source: "tpl".into(),
                target: "t1".into(),
                ttl: None
            }
        );
        assert_eq!(
            parse(&args("clone tpl t1 --ttl 1h")).unwrap().1,
            Command::Clone {
                source: "tpl".into(),
                target: "t1".into(),
                ttl: Some("1h".into())
            }
        );
        assert_eq!(
            parse(&args("branch myapp agent-1")).unwrap().1,
            Command::Branch {
                source: "myapp".into(),
                name: "agent-1".into(),
                ttl: None
            }
        );
        assert_eq!(
            parse(&args("branch myapp agent-3 --ttl 30m")).unwrap().1,
            Command::Branch {
                source: "myapp".into(),
                name: "agent-3".into(),
                ttl: Some("30m".into())
            }
        );
        assert_eq!(
            parse(&args("--json branch myapp agent-1 --ttl=unlimited"))
                .unwrap()
                .1,
            Command::Branch {
                source: "myapp".into(),
                name: "agent-1".into(),
                ttl: Some("unlimited".into())
            }
        );
        for bad in [
            "branch",
            "branch myapp",
            "branch a b c",
            "branch a b --force",
            "branch a b --ttl",
        ] {
            assert_eq!(
                parse(&args(bad)).unwrap_err().code,
                ExitCode::Usage,
                "{bad}"
            );
        }
        assert_eq!(
            parse(&args("ttl agent-1")).unwrap().1,
            Command::Ttl {
                name: "agent-1".into(),
                value: None
            }
        );
        assert_eq!(
            parse(&args("ttl agent-1 30m")).unwrap().1,
            Command::Ttl {
                name: "agent-1".into(),
                value: Some("30m".into())
            }
        );
        assert_eq!(parse(&args("supervise")).unwrap().1, Command::Supervise);
        assert_eq!(parse(&args("ttl")).unwrap_err().code, ExitCode::Usage);
        assert_eq!(parse(&args("ttl a b c")).unwrap_err().code, ExitCode::Usage);
        assert_eq!(
            parse(&args("url myapp")).unwrap().1,
            Command::Url {
                name: "myapp".into(),
                socket: false
            }
        );
        assert_eq!(
            parse(&args("url myapp --socket")).unwrap().1,
            Command::Url {
                name: "myapp".into(),
                socket: true
            }
        );
        assert_eq!(
            parse(&args("init --port 5555")).unwrap().1,
            Command::Init {
                port: Some(5555),
                durability: None,
                force: false
            }
        );
        assert_eq!(
            parse(&args("init --durability full --force")).unwrap().1,
            Command::Init {
                port: None,
                durability: Some("full".into()),
                force: true
            }
        );
        assert_eq!(
            parse(&args("stop --force")).unwrap().1,
            Command::Stop { force: true }
        );
        assert_eq!(parse(&args("serve")).unwrap().1, Command::Serve);
        assert_eq!(parse(&args("ls")).unwrap().1, Command::Ls);
        assert_eq!(parse(&args("doctor")).unwrap().1, Command::Doctor);
        assert_eq!(parse(&args("version")).unwrap().1, Command::Version);
        assert_eq!(parse(&args("--version")).unwrap().1, Command::Version);
        assert_eq!(parse(&args("help")).unwrap().1, Command::Help);
        assert_eq!(parse(&args("--help")).unwrap().1, Command::Help);
        assert_eq!(parse(&args("")).unwrap().1, Command::Help);
    }

    #[test]
    fn parse_run_subcommand() {
        assert_eq!(parse(&args("run")).unwrap().1, Command::Run);
        let (g, c) = parse(&args("--json run")).unwrap();
        assert!(g.json);
        assert_eq!(c, Command::Run);
        // run takes no positionals and no flags of its own.
        assert_eq!(parse(&args("run extra")).unwrap_err().code, ExitCode::Usage);
        assert_eq!(
            parse(&args("run --bogus")).unwrap_err().code,
            ExitCode::Usage
        );
    }

    #[test]
    fn parse_copy_subcommand() {
        use crate::cmd::copy::Opts;
        assert_eq!(
            parse(&args("copy --from postgres://u:p@h/d --to tpl"))
                .unwrap()
                .1,
            Command::Copy(Opts {
                from: "postgres://u:p@h/d".into(),
                to: "tpl".into(),
                schema_only: false,
                jobs: None,
                verbose: false,
                optimize: true,
            })
        );
        let (g, c) = parse(&args(
            "--json copy --from=postgres://h/d --to=tpl --schema-only --jobs 4 --verbose --no-optimize",
        ))
        .unwrap();
        assert!(g.json);
        assert_eq!(
            c,
            Command::Copy(Opts {
                from: "postgres://h/d".into(),
                to: "tpl".into(),
                schema_only: true,
                jobs: Some(4),
                verbose: true,
                optimize: false,
            })
        );
        for bad in [
            "copy",
            "copy --from x",
            "copy --to x",
            "copy --from x --to y extra",
            "copy --from x --to y --jobs 0",
            "copy --from x --to y --jobs many",
            "copy --from x --to y --data-only",
            "copy --from",
        ] {
            assert_eq!(
                parse(&args(bad)).unwrap_err().code,
                ExitCode::Usage,
                "{bad}"
            );
        }
    }

    #[test]
    fn unknown_subcommand_is_usage_error() {
        let e = parse(&args("frobnicate")).unwrap_err();
        assert_eq!(e.code, ExitCode::Usage);
        assert!(e.message.contains("frobnicate"), "{}", e.message);
    }

    #[test]
    fn missing_and_extra_arguments_are_usage_errors() {
        assert_eq!(parse(&args("create")).unwrap_err().code, ExitCode::Usage);
        assert_eq!(
            parse(&args("create a b")).unwrap_err().code,
            ExitCode::Usage
        );
        assert_eq!(parse(&args("clone a")).unwrap_err().code, ExitCode::Usage);
        assert_eq!(
            parse(&args("init --port notanumber")).unwrap_err().code,
            ExitCode::Usage
        );
        assert_eq!(
            parse(&args("init --port 70000")).unwrap_err().code,
            ExitCode::Usage
        );
        assert_eq!(
            parse(&args("status --bogus")).unwrap_err().code,
            ExitCode::Usage
        );
    }

    #[test]
    fn help_mentions_every_command() {
        let h = help_text();
        for c in [
            "run", "init", "start", "stop", "restart", "status", "create", "drop", "clone",
            "branch", "ls", "url", "copy", "doctor", "version", "help",
        ] {
            assert!(
                h.contains(&format!("pgx {c}")) || h.contains(&format!("  {c}")),
                "help lacks {c}"
            );
        }
        assert!(h.contains("--json"));
        assert!(h.contains("Exit codes"));
    }

    #[test]
    fn agent_exec_keeps_everything_after_double_dash() {
        let (g, c) = parse(&args("agent exec --source tpl -- codex --json x")).unwrap();
        assert!(!g.json, "--json after -- belongs to the child");
        match c {
            Command::Agent(crate::cmd::agent::AgentCmd::Exec(o)) => {
                assert_eq!(o.source.as_deref(), Some("tpl"));
                assert_eq!(o.command, vec!["codex", "--json", "x"]);
                assert!(!o.keep);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn agent_requires_a_subcommand_and_rejects_unknown_ones() {
        assert_eq!(parse(&args("agent")).unwrap_err().code, ExitCode::Usage);
        assert_eq!(
            parse(&args("agent frobnicate")).unwrap_err().code,
            ExitCode::Usage
        );
        assert_eq!(
            parse(&args("agent exec")).unwrap_err().code,
            ExitCode::Usage
        );
        assert_eq!(
            parse(&args("agent up --bogus 1")).unwrap_err().code,
            ExitCode::Usage
        );
    }

    #[test]
    fn agent_subcommands_parse_their_options() {
        use crate::cmd::agent::{AgentCmd, Scope};
        let (g, c) = parse(&args(
            "--json agent up --id abc --host claude --ttl 1h --env",
        ))
        .unwrap();
        assert!(g.json);
        match c {
            Command::Agent(AgentCmd::Up(o)) => {
                assert_eq!(o.id.as_deref(), Some("abc"));
                assert_eq!(o.host.as_deref(), Some("claude"));
                assert_eq!(o.ttl.as_deref(), Some("1h"));
                assert!(o.env);
            }
            other => panic!("{other:?}"),
        }
        match parse(&args("agent down --id abc --now")).unwrap().1 {
            Command::Agent(AgentCmd::Down(o)) => assert!(o.now && o.grace.is_none()),
            other => panic!("{other:?}"),
        }
        match parse(&args(
            "agent install claude cursor --scope local --source tpl",
        ))
        .unwrap()
        .1
        {
            Command::Agent(AgentCmd::Install(o)) => {
                assert_eq!(o.hosts, vec!["claude", "cursor"]);
                assert_eq!(o.scope, Scope::Local);
                assert_eq!(o.source.as_deref(), Some("tpl"));
            }
            other => panic!("{other:?}"),
        }
        match parse(&args("agent setup --hosts claude,codex --no-hooks"))
            .unwrap()
            .1
        {
            Command::Agent(AgentCmd::Setup(o)) => {
                assert_eq!(o.hosts, vec!["claude", "codex"]);
                assert!(o.no_hooks);
                assert_eq!(o.scope, Scope::Project);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse(&args("agent ls")).unwrap().1,
            Command::Agent(AgentCmd::Ls)
        ));
        assert!(matches!(
            parse(&args("agent doctor")).unwrap().1,
            Command::Agent(AgentCmd::Doctor)
        ));
        assert!(matches!(
            parse(&args("agent hook --host cursor")).unwrap().1,
            Command::Agent(AgentCmd::Hook(_))
        ));
    }

    #[test]
    fn parse_top_and_its_help() {
        use crate::cmd::top::TopOpts;
        assert_eq!(
            parse(&args("top")).unwrap().1,
            Command::Top(TopOpts::default())
        );
        let (g, c) = parse(&args("--json top --snapshot --demo")).unwrap();
        assert!(g.json);
        match c {
            Command::Top(o) => assert!(o.snapshot && o.demo && !o.help),
            other => panic!("{other:?}"),
        }
        let (g, c) = parse(&args("top --snapshot --json")).unwrap();
        assert!(g.json, "--json is global, after the subcommand too");
        assert!(matches!(c, Command::Top(TopOpts { snapshot: true, .. })));
        for (s, ms) in [
            ("top --interval-ms 250", 250),
            ("top --interval-ms=10000", 10_000),
        ] {
            match parse(&args(s)).unwrap().1 {
                Command::Top(o) => assert_eq!(o.interval_ms, ms),
                other => panic!("{other:?}"),
            }
        }
        for bad in [
            "top --interval-ms 0",
            "top --interval-ms -1",
            "top --interval-ms 10001",
            "top --interval-ms x",
            "top --interval-ms",
            "top --bogus",
            "top extra",
        ] {
            assert_eq!(
                parse(&args(bad)).unwrap_err().code,
                ExitCode::Usage,
                "{bad}"
            );
        }
        // `pgx top --help` is the monitor's page; other commands keep the global one.
        assert!(matches!(
            parse(&args("top --help")).unwrap().1,
            Command::Top(TopOpts { help: true, .. })
        ));
        assert!(matches!(
            parse(&args("top -h --demo")).unwrap().1,
            Command::Top(TopOpts { help: true, .. })
        ));
        assert_eq!(parse(&args("status --help")).unwrap().1, Command::Help);
        assert_eq!(
            parse(&args("--help top")).unwrap().1,
            Command::Top(TopOpts {
                help: true,
                ..Default::default()
            })
        );
        assert!(help_text().contains("pgx top"));
        assert!(help_text().contains("pgx agent activity"));
    }

    #[test]
    fn parse_agent_activity() {
        use crate::cmd::agent::AgentCmd;
        match parse(&args(
            "agent activity --state testing --task OAuth --id s1 --agent a1",
        ))
        .unwrap()
        .1
        {
            Command::Agent(AgentCmd::Activity(o)) => {
                assert_eq!(o.state, "testing");
                assert_eq!(o.task.as_deref(), Some("OAuth"));
                assert_eq!(o.id.as_deref(), Some("s1"));
                assert_eq!(o.agent.as_deref(), Some("a1"));
            }
            other => panic!("{other:?}"),
        }
        for bad in [
            "agent activity",
            "agent activity --task x",
            "agent activity --state done",
            "agent activity --state testing extra",
            "agent activity --state testing --ttl 1h",
            "agent activity --state",
        ] {
            assert_eq!(
                parse(&args(bad)).unwrap_err().code,
                ExitCode::Usage,
                "{bad}"
            );
        }
        // The new value flags do not change other subcommands' contracts.
        assert_eq!(
            parse(&args("agent up --state x")).unwrap_err().code,
            ExitCode::Usage
        );
        assert_eq!(
            parse(&args("agent touch --task x")).unwrap_err().code,
            ExitCode::Usage
        );
    }

    #[test]
    fn help_mentions_agent() {
        assert!(help_text().contains("pgx agent setup"));
        assert!(help_text().contains("pgx agent exec"));
    }
}
