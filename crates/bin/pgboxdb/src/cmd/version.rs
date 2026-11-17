use crate::cli::ExitCode;
use crate::errors::Result;
use crate::layout::Install;
use crate::out::Out;

pub fn run(out: &Out) -> Result<ExitCode> {
    let install = Install::discover()?;
    let d = install.dist_info().unwrap_or_default();
    let pgboxdb_version = if d.pgboxdb_version.is_empty() || d.pgboxdb_version == "unknown" {
        env!("CARGO_PKG_VERSION").to_string()
    } else {
        d.pgboxdb_version.clone()
    };
    let text = format!(
        "pgboxdb {pgboxdb_version} (engine {}, PostgreSQL {} compatible, {}/{}, built {})\ninstall: {}",
        d.engine_commit,
        d.postgres_compat,
        d.os,
        d.arch,
        d.built_at,
        install.prefix.display()
    );
    out.emit(
        &text,
        serde_json::json!({
            "pgboxdb_version": pgboxdb_version, "engine_commit": d.engine_commit, "postgres_compat": d.postgres_compat,
            "initdb_version": d.initdb_version, "os": d.os, "arch": d.arch, "built_at": d.built_at,
            "install_prefix": install.prefix,
        }),
    );
    Ok(ExitCode::Ok)
}
