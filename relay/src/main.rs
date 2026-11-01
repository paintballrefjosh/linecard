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
//! linecard-relay health                        exit 0 if the local server answers (for container health checks)
//! ```
//!
//! Environment:
//!   LINECARD_RELAY_ADDR      listen address (default 0.0.0.0:8443)
//!   LINECARD_RELAY_DB        SQLite file (default ./linecard-relay.db)
//!   LINECARD_RELAY_MAX_OPS   batches kept per sync group (default 200000)
//!   LINECARD_RELAY_OPEN      "false" = only accounts may start sync groups
//!
//! On SIGTERM or Ctrl+C the server stops accepting connections, gives requests
//! in flight a few seconds (long-polls are released), and folds the SQLite
//! write-ahead log into the database file before exiting.
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

/// True when `response` is an HTTP reply with status 200.
fn is_ok(response: &str) -> bool {
    response.split_whitespace().nth(1) == Some("200")
}

/// Ask the server on `addr` for /health. Plain HTTP over a socket, so the
/// binary can check itself inside an image with no curl.
fn probe(addr: SocketAddr) -> Result<(), String> {
    use std::io::{Read, Write};
    let wait = std::time::Duration::from_secs(3);
    let mut s = std::net::TcpStream::connect_timeout(&addr, wait).map_err(|e| format!("can't reach {addr}: {e}"))?;
    s.set_read_timeout(Some(wait)).map_err(|e| e.to_string())?;
    s.write_all(
        b"GET /health HTTP/1.0
Host: localhost

",
    )
    .map_err(|e| e.to_string())?;
    let mut response = String::new();
    let _ = s.read_to_string(&mut response);
    if is_ok(&response) {
        Ok(())
    } else {
        Err(format!(
            "{addr} answered: {}",
            response.lines().next().unwrap_or("nothing")
        ))
    }
}

/// The address to probe: the listen address, with 127.0.0.1 for a wildcard.
fn local_addr(listen: &str) -> Result<SocketAddr, String> {
    let mut addr: SocketAddr = listen
        .parse()
        .map_err(|_| "LINECARD_RELAY_ADDR must be host:port".to_string())?;
    if addr.ip().is_unspecified() {
        addr.set_ip(if addr.is_ipv6() {
            std::net::Ipv6Addr::LOCALHOST.into()
        } else {
            std::net::Ipv4Addr::LOCALHOST.into()
        });
    }
    Ok(addr)
}

/// Resolves on SIGTERM (container stop) or Ctrl+C.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

/// How long requests in flight get after a stop signal. Docker kills at 10 s.
const GRACE: std::time::Duration = std::time::Duration::from_secs(8);

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("health") {
        let listen = std::env::var("LINECARD_RELAY_ADDR").unwrap_or_else(|_| "0.0.0.0:8443".into());
        return match local_addr(&listen).and_then(probe) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
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
    let relay = Arc::new(relay);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(listener, router(relay.clone())).with_graceful_shutdown(async {
        let _ = stop_rx.await;
    });
    let mut server = tokio::spawn(async move { server.await });
    tokio::select! {
        done = &mut server => {
            // The server ended by itself: that is an error.
            eprintln!("server stopped: {done:?}");
            return ExitCode::FAILURE;
        }
        _ = shutdown_signal() => {}
    }
    println!("linecard-relay stopping");
    let _ = stop_tx.send(());
    if tokio::time::timeout(GRACE, &mut server).await.is_err() {
        // Long-polls and slow clients: stop waiting for them.
        server.abort();
    }
    if let Err(e) = relay.checkpoint() {
        eprintln!("could not checkpoint the database: {e}");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_status_line() {
        assert!(is_ok("HTTP/1.0 200 OK\r\n\r\nok"));
        assert!(is_ok("HTTP/1.1 200 OK\r\n"));
        assert!(!is_ok("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(!is_ok(""));
    }

    #[test]
    fn probes_the_local_address_even_when_listening_on_every_interface() {
        assert_eq!(local_addr("0.0.0.0:8443").unwrap().to_string(), "127.0.0.1:8443");
        assert_eq!(local_addr("[::]:8443").unwrap().to_string(), "[::1]:8443");
        assert_eq!(local_addr("10.0.0.5:9000").unwrap().to_string(), "10.0.0.5:9000");
        assert!(local_addr("nonsense").is_err());
    }

    #[tokio::test]
    async fn health_probe_succeeds_against_the_router_and_fails_when_nothing_listens() {
        let relay = Arc::new(
            Relay::in_memory(Limits {
                max_ops: 10,
                open: true,
            })
            .unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(relay)).await });
        // `probe` blocks, so keep it off the runtime thread serving the request.
        tokio::task::spawn_blocking(move || probe(addr)).await.unwrap().unwrap();

        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        assert!(tokio::task::spawn_blocking(move || probe(closed))
            .await
            .unwrap()
            .is_err());
    }

    #[test]
    fn checkpoint_works_on_a_file_database() {
        let path = std::env::temp_dir().join(format!("linecard-relay-ckpt-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let relay = Relay::open(
            &path,
            Limits {
                max_ops: 10,
                open: true,
            },
        )
        .unwrap();
        relay
            .push(
                "0123456789abcdef0123456789abcdef",
                "token-for-the-group-xyz",
                None,
                "dev",
                "blob",
            )
            .unwrap();
        relay.checkpoint().unwrap();
        drop(relay);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }
}
