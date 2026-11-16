//! `pgboxdb` — a local, disposable, PostgreSQL-compatible runtime.
mod cli;
mod cmd;
mod conf;
mod config;
mod conninfo;
mod errors;
mod fsdetect;
mod fsutil;
mod layout;
mod legacy;
mod names;
mod out;
mod pgtools;
mod pgwire;
mod process;
mod toc;
mod ttl;
mod ttlstore;

use cli::{Command, ExitCode};
use errors::{PgboxdbError, Result};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (global, command) = match cli::parse(&args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("pgboxdb: {e}");
            std::process::exit(e.code as i32);
        }
    };
    let code = match run(global, command) {
        Ok(code) => code,
        Err(e) => {
            if global.json {
                println!("{}", e.to_json());
            } else {
                eprintln!("pgboxdb: {e}");
            }
            e.code
        }
    };
    std::process::exit(code as i32);
}

fn run(global: cli::Global, command: Command) -> Result<ExitCode> {
    let out = out::Out::new(global.json);
    // Environment variables from before the rename are never read; say so
    // where a user configures or inspects the runtime (not in hooks).
    if matches!(
        command,
        Command::Init { .. }
            | Command::Run
            | Command::Start
            | Command::Serve
            | Command::Status
            | Command::Doctor
            | Command::Migrate(_)
    ) {
        if let Some(n) = legacy::ignored_env_note() {
            out.note(&n);
        }
    }
    match command {
        Command::Help => {
            print!("{}", cli::help_text());
            Ok(ExitCode::Ok)
        }
        Command::Version => cmd::version::run(&out),
        Command::Init {
            port,
            durability,
            force,
        } => cmd::init::run(&out, port, durability, force),
        Command::Serve => cmd::start::serve(&out),
        Command::Run => cmd::run::run(&out),
        Command::Supervise => cmd::supervise::run(&out),
        Command::Doctor => cmd::doctor::run(&out),
        Command::Migrate(opts) => cmd::migrate::run(&out, &opts),
        // Agent commands manage initialisation themselves (`setup` initialises;
        // `hook` must degrade gracefully instead of pointing at `pgboxdb init`).
        Command::Agent(cmd) => cmd::agent::run(&out, cmd),
        // PGBoxDB Git initialises (`init`) or degrades (`hook`) on its own.
        Command::Git(cmd) => cmd::git::run(&out, cmd),
        // `pgboxdb top` never initialises anything; `--demo` needs no home at all.
        Command::Top(opts) => cmd::top::run(&out, opts),
        other => {
            let home = layout::Home::resolve();
            if !home.is_initialized() {
                return Err(PgboxdbError::new(
                    ExitCode::NotReady,
                    format!("PGBoxDB is not initialised at {}", home.root.display()),
                )
                .hint(
                    legacy::uninitialised_hint(&home)
                        .unwrap_or_else(|| "run `pgboxdb init` first".to_string()),
                ));
            }
            match other {
                Command::Start => cmd::start::run(&out),
                Command::Stop { force } => cmd::stop::run(&out, force),
                Command::Restart => cmd::start::restart(&out),
                Command::Status => cmd::status::run(&out),
                Command::Create { name, ttl } => cmd::db::create(&out, &name, ttl.as_deref()),
                Command::Drop { name, force } => cmd::db::drop(&out, &name, force),
                Command::Clone {
                    source,
                    target,
                    ttl,
                } => cmd::db::clone(&out, &source, &target, ttl.as_deref()),
                Command::Branch { source, name, ttl } => {
                    cmd::db::branch(&out, &source, &name, ttl.as_deref())
                }
                Command::Ls => cmd::db::ls(&out),
                Command::Ttl { name, value } => cmd::ttl::run(&out, &name, value.as_deref()),
                Command::Url { name, socket } => cmd::db::url(&out, &name, socket),
                Command::Copy(opts) => cmd::copy::run(&out, opts),
                Command::Help
                | Command::Version
                | Command::Init { .. }
                | Command::Run
                | Command::Doctor
                | Command::Migrate(_)
                | Command::Agent(_)
                | Command::Git(_)
                | Command::Top(_)
                | Command::Serve
                | Command::Supervise => unreachable!(),
            }
        }
    }
}
