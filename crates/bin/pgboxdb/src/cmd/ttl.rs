//! `pgboxdb ttl <db>` shows a database's TTL; `pgboxdb ttl <db> <dur>` sets it.
//! Setting `unlimited` makes the database permanent (removes its TTL record);
//! a finite duration sets `expires_at = now + duration`.
use super::Ctx;
use crate::cli::ExitCode;
use crate::errors::{PgboxdbError, Result};
use crate::names;
use crate::out::Out;
use crate::ttlstore::{MetaStore, State, TtlRecord};
use serde_json::json;

pub fn run(out: &Out, name: &str, value: Option<&str>) -> Result<ExitCode> {
    let name = names::validate_user_target(name)?;
    let ctx = Ctx::load()?;
    let store = MetaStore::new(&ctx.home.root);
    let now = crate::fsutil::now_epoch();

    let mut client = ctx.connect("postgres")?;
    let oid = crate::cmd::db::database_oid(&mut client, name.as_str())?;
    let Some(oid) = oid else {
        return Err(PgboxdbError::new(
            ExitCode::NotFound,
            format!("database {name} does not exist"),
        )
        .hint("`pgboxdb ls` lists databases"));
    };

    match value {
        // Show the current TTL.
        None => {
            client.close();
            let rec = store.load(name.as_str())?;
            let (label, expires_at) = match &rec {
                Some(r) => (r.ttl_label.clone(), r.expires_at),
                None => ("unlimited".to_string(), None),
            };
            out.emit(
                &format!(
                    "{name}: {label} ({})",
                    if expires_at.is_some() {
                        format!(
                            "expires in {}",
                            crate::ttl::humanize_remaining(expires_at, now)
                        )
                    } else {
                        "never expires".to_string()
                    }
                ),
                json!({
                    "name": name.as_str(),
                    "ttl": label,
                    "expires_at": expires_at.map(crate::fsutil::rfc3339),
                    "expires_in_seconds": expires_at.map(|e| e.saturating_sub(now)),
                }),
            );
            Ok(ExitCode::Ok)
        }
        // Set / extend / disable the TTL.
        Some(arg) => {
            let ttl = crate::ttl::parse_ttl(arg)?;
            client.close();
            match ttl {
                crate::ttl::Ttl::Unlimited => {
                    store.remove(name.as_str())?;
                    out.emit(
                        &format!("{name}: TTL removed (now permanent)"),
                        json!({"name": name.as_str(), "ttl": "unlimited", "expires_at": null, "expires_in_seconds": null}),
                    );
                }
                crate::ttl::Ttl::Seconds(_) => {
                    let expires_at = ttl.expires_at(now)?;
                    // An agent branch keeps its session ownership across a manual
                    // TTL change, so `pgboxdb agent down` still recognises it.
                    let agent = store.load(name.as_str())?.and_then(|r| r.agent);
                    let rec = TtlRecord {
                        name: name.as_str().to_string(),
                        oid,
                        cluster_id: ctx.cluster_id(),
                        created_at: now,
                        ttl_label: ttl.label(),
                        ttl_seconds: ttl.as_seconds(),
                        expires_at,
                        state: State::Active,
                        last_attempt: None,
                        last_error: None,
                        agent,
                    };
                    store.save(&rec)?;
                    out.emit(
                        &format!(
                            "{name}: TTL {} (expires in {})",
                            ttl.label(),
                            crate::ttl::humanize_remaining(expires_at, now)
                        ),
                        json!({
                            "name": name.as_str(),
                            "ttl": ttl.label(),
                            "ttl_seconds": ttl.as_seconds(),
                            "expires_at": expires_at.map(crate::fsutil::rfc3339),
                            "expires_in_seconds": expires_at.map(|e| e.saturating_sub(now)),
                        }),
                    );
                }
            }
            Ok(ExitCode::Ok)
        }
    }
}
