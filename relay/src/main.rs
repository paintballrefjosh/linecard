//! Linecard relay server and admin tool. See lib.rs for the protocol.
//!
//! ```text
//! linecard-relay                               serve (also: linecard-relay serve)
//! linecard-relay account add <name> [--max-groups N]   prints the access key
//! linecard-relay account list
//! linecard-relay account remove <name> [--with-groups]
//! linecard-relay group list
//! linecard-relay group remove <group-id>
//! linecard-relay stats
//! ```
//!
//! Environment:
//!   LINECARD_RELAY_ADDR      listen address (default 0.0.0.0:8443)
//!   LINECARD_RELAY_DB        SQLite file (default ./linecard-relay.db)
//!   LINECARD_RELAY_MAX_OPS   batches kept per sync group (default 200000)
//!   LINECARD_RELAY_OPEN      "false" = only accounts may start sync groups
//!
//! Serve it behind HTTPS (a reverse proxy such as Caddy or Traefik); the
//! data is end-to-end encrypted, but the token travels in a header. The admin
//! commands work on the database directly (run them where it lives, e.g.
//! `docker exec <container> linecard-relay account add alice`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use linecard_relay::{router, Limits, Relay};

fn env_limits() -> Limits {
    Limits {
        max_ops: std::env::var("LINECARD_RELAY_MAX_OPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200_000),
        open: !matches!(
            std::env::var("LINECARD_RELAY_OPEN")
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Ok("false" | "0" | "no")
        ),
    }
}

fn open_db() -> Result<(Relay, PathBuf), String> {
    let db = PathBuf::from(std::env::var("LINECARD_RELAY_DB").unwrap_or_else(|_| "linecard-relay.db".into()));
    let relay = Relay::open(&db, env_limits()).map_err(|e| format!("can't open {}: {e}", db.display()))?;
    Ok((relay, db))
}

fn when(ms: Option<i64>) -> String {
    match ms {
        None => "never".into(),
        Some(ms) => {
            let ago = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
                - ms)
                / 1000;
            match ago {
                s if s < 120 => format!("{s}s ago"),
                s if s < 7200 => format!("{}m ago", s / 60),
                s if s < 172_800 => format!("{}h ago", s / 3600),
                s => format!("{}d ago", s / 86_400),
            }
        }
    }
}

fn admin(args: &[String]) -> Result<(), String> {
    let (relay, _) = open_db()?;
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let flag = |name: &str| words.iter().position(|w| *w == name);
    let err = |e: linecard_relay::RelayError| format!("{e:?}");
    match words.as_slice() {
        ["account", "add", name, ..] => {
            let max = flag("--max-groups")
                .and_then(|i| words.get(i + 1))
                .map(|v| v.parse::<i64>().map_err(|_| "--max-groups needs a number".to_string()))
                .transpose()?;
            let key = relay.add_account(name, max).map_err(err)?;
            println!("Account {name} created. Its access key (shown once; enter it in Linecard → Settings → Sync):");
            println!("{key}");
        }
        ["account", "list"] => {
            let accounts = relay.accounts().map_err(err)?;
            if accounts.is_empty() {
                println!("No accounts.");
            }
            for a in accounts {
                let max = a.max_groups.map_or("no limit".into(), |m| format!("max {m}"));
                println!(
                    "{:<24} {} groups ({max}), created {}",
                    a.name,
                    a.groups,
                    when(Some(a.created))
                );
            }
        }
        ["account", "remove", name, ..] => {
            let n = relay
                .remove_account(name, flag("--with-groups").is_some())
                .map_err(err)?;
            println!(
                "Removed {name}{}.",
                if n > 0 {
                    format!(" and {n} sync groups")
                } else {
                    String::new()
                }
            );
        }
        ["group", "list"] => {
            let groups = relay.groups().map_err(err)?;
            if groups.is_empty() {
                println!("No sync groups.");
            }
            for g in groups {
                println!(
                    "{}  {:<16} {:>7} batches  {} devices  last write {}",
                    g.id,
                    g.account.as_deref().unwrap_or("(open)"),
                    g.ops,
                    g.devices,
                    when(g.last)
                );
            }
        }
        ["group", "remove", id] => {
            relay.remove_group(id).map_err(err)?;
            println!("Removed sync group {id}.");
        }
        ["stats"] => {
            let groups = relay.groups().map_err(err)?;
            let ops: i64 = groups.iter().map(|g| g.ops).sum();
            println!(
                "{} accounts, {} sync groups, {ops} batches stored",
                relay.accounts().map_err(err)?.len(),
                groups.len()
            );
        }
        _ => return Err("unknown command; see the top of src/main.rs or the README".into()),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() && args[0] != "serve" {
        return match admin(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    let addr: SocketAddr = std::env::var("LINECARD_RELAY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8443".into())
        .parse()
        .expect("LINECARD_RELAY_ADDR must be host:port");
    let (relay, db) = match open_db() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let open = env_limits().open;
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    println!(
        "linecard-relay listening on {addr}, data in {}, {}",
        db.display(),
        if open {
            "open to any sync phrase"
        } else {
            "private (accounts only)"
        }
    );
    axum::serve(listener, router(Arc::new(relay))).await.expect("server");
    ExitCode::SUCCESS
}
