//! `pgboxdb status`: exit 0 when running, 3 when not; the state is printed either way.
use super::Ctx;
use crate::cli::ExitCode;
use crate::errors::Result;
use crate::out::Out;
use serde_json::{json, Value};

fn rss_bytes_via_ps(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .ok()
        .map(|kb| kb * 1024)
}

pub fn run(out: &Out) -> Result<ExitCode> {
    let ctx = Ctx::load()?;
    let sock_dir = ctx.sock_dir();
    let mut v = json!({
        "running": false,
        "port": ctx.config.port,
        "socket_dir": sock_dir,
        "data_dir": ctx.home.data,
        "home": ctx.home.root,
        "durability": ctx.config.durability.as_str(),
        "log": ctx.home.log_file,
    });
    let Some(pid) = ctx.running_pid() else {
        out.emit(
            &format!(
                "PGBoxDB is not running\n  home        {}\n  port        {}\n  durability  {}",
                ctx.home.root.display(),
                ctx.config.port,
                ctx.config.durability.as_str()
            ),
            v,
        );
        return Ok(ExitCode::NotReady);
    };
    v["running"] = json!(true);
    v["pid"] = json!(pid);
    let mut client = ctx.connect("postgres")?;
    let q = |c: &mut crate::pgwire::Client<std::os::unix::net::UnixStream>,
             sql: &str|
     -> Option<String> {
        c.simple_query(sql)
            .ok()
            .and_then(|r| r.scalar().map(String::from))
    };
    let databases: u64 = q(
        &mut client,
        "SELECT count(*) FROM pg_database WHERE NOT datistemplate",
    )
    .and_then(|s| s.parse().ok())
    .unwrap_or(0);
    let connections: u64 = q(&mut client, "SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend' AND pid <> pg_backend_pid()").and_then(|s| s.parse().ok()).unwrap_or(0);
    let uptime: u64 = q(
        &mut client,
        "SELECT extract(epoch FROM now() - pg_postmaster_start_time())::bigint",
    )
    .and_then(|s| s.parse().ok())
    .unwrap_or(0);
    let version = q(&mut client, "SELECT version()").unwrap_or_default();
    let runtime: Value = q(&mut client, "SELECT pgrust_runtime_status()")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    client.close();
    let rss = runtime
        .pointer("/memory/rss_bytes")
        .and_then(|x| x.as_u64())
        .or_else(|| rss_bytes_via_ps(pid));
    v["databases"] = json!(databases);
    v["connections"] = json!(connections);
    v["uptime_s"] = json!(uptime);
    v["memory_rss_bytes"] = json!(rss);
    v["version"] = json!(version);
    v["libraries"] = runtime.get("libraries").cloned().unwrap_or(Value::Null);
    v["runtime"] = runtime;
    let mb = rss
        .map(|b| format!("{:.0} MB", b as f64 / 1_048_576.0))
        .unwrap_or_else(|| "n/a".into());
    let text = format!(
        "PGBoxDB is running (pid {pid})\n  port         {}\n  socket       {}\n  databases    {databases}\n  connections  {connections}\n  uptime       {uptime}s\n  memory       {mb} RSS\n  durability   {}\n  data         {}\n  engine       {version}",
        ctx.config.port,
        sock_dir.display(),
        ctx.config.durability.as_str(),
        ctx.home.data.display()
    );
    out.emit(&text, v);
    Ok(ExitCode::Ok)
}
