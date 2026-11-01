//! Linecard relay server and admin tool. See lib.rs for the protocol.
//!
//! ```text
//! linecard-relay                               serve (also: linecard-relay serve)
//! linecard-relay account add <name> [--max-groups N]   prints the access key
//! linecard-relay account list
//! linecard-relay account remove <name> [--with-groups]
//! linecard-relay group list [--idle [DAYS]]      sizes and last activity; --idle: only quiet groups
//! linecard-relay group remove <group-id>
//! linecard-relay vacuum                        give deleted data's disk space back (safe while serving; writes pause briefly)
//! linecard-relay backup <file or folder>       copy the database while serving (checked after it is made)
//! linecard-relay restore <backup file>         replace the database with a backup (stop the relay first; the old file is kept)
//! linecard-relay stats
//! linecard-relay health                        exit 0 if the local server answers (for container health checks)
//! ```
//!
//! Environment:
//!   LINECARD_RELAY_ADDR      listen address (default 0.0.0.0:8443)
//!   LINECARD_RELAY_DB        SQLite file (default ./linecard-relay.db)
//!   LINECARD_RELAY_MAX_OPS   batches kept per sync group (default 200000)
//!   LINECARD_RELAY_OPEN      "false" = only accounts may start sync groups
//!   LINECARD_RELAY_MAX_GROUP_BYTES  storage per group, e.g. 256MiB (default; 0 = no limit)
//!   LINECARD_RELAY_IDLE_DAYS delete groups with no write or heartbeat for this many days (default 0 = never)
//!   LINECARD_RELAY_BACKUP_DIR  folder for built-in daily backups (default: none)
//!   LINECARD_RELAY_BACKUP_KEEP how many of them to keep (default 7)
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

/// "256MiB", "1GiB", "500000" or "0"/"off"/"unlimited" (= no limit) as bytes.
fn parse_size(text: &str) -> Option<i64> {
    let t = text.trim().to_ascii_lowercase();
    if matches!(t.as_str(), "off" | "none" | "unlimited") {
        return Some(0);
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    let n: i64 = t[..digits].parse().ok()?;
    let unit: i64 = match t[digits..].trim() {
        "" | "b" => 1,
        "k" | "kb" => 1000,
        "kib" => 1 << 10,
        "m" | "mb" => 1_000_000,
        "mib" => 1 << 20,
        "g" | "gb" => 1_000_000_000,
        "gib" => 1 << 30,
        _ => return None,
    };
    n.checked_mul(unit)
}

fn env_limits() -> Limits {
    let default = Limits::default();
    let number = |name: &str, fallback: i64| match std::env::var(name) {
        Ok(v) => v.parse().unwrap_or_else(|_| {
            eprintln!("{name}={v} is not a number; using {fallback}");
            fallback
        }),
        Err(_) => fallback,
    };
    Limits {
        max_ops: number("LINECARD_RELAY_MAX_OPS", default.max_ops),
        open: !matches!(
            std::env::var("LINECARD_RELAY_OPEN")
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Ok("false" | "0" | "no")
        ),
        max_group_bytes: match std::env::var("LINECARD_RELAY_MAX_GROUP_BYTES") {
            Ok(v) => parse_size(&v).unwrap_or_else(|| {
                eprintln!("LINECARD_RELAY_MAX_GROUP_BYTES={v} is not a size (try 256MiB); using the default");
                default.max_group_bytes
            }),
            Err(_) => default.max_group_bytes,
        },
        idle_days: number("LINECARD_RELAY_IDLE_DAYS", default.idle_days).max(0),
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

/// Where built-in daily backups go, and how many to keep (`LINECARD_RELAY_BACKUP_DIR`
/// and `LINECARD_RELAY_BACKUP_KEEP`, default 7). Off when no folder is set.
fn backup_settings() -> Option<(PathBuf, usize)> {
    let dir = std::env::var("LINECARD_RELAY_BACKUP_DIR")
        .ok()
        .filter(|d| !d.trim().is_empty())?;
    let keep = std::env::var("LINECARD_RELAY_BACKUP_KEEP")
        .ok()
        .and_then(|k| k.parse().ok())
        .unwrap_or(7)
        .max(1);
    Some((PathBuf::from(dir), keep))
}

fn db_path() -> PathBuf {
    PathBuf::from(std::env::var("LINECARD_RELAY_DB").unwrap_or_else(|_| "linecard-relay.db".into()))
}

/// `backup <path>`: a copy of the database, taken while the relay serves.
/// `path` may be a folder (the file is named for the time).
fn backup_command(dest: &str) -> Result<(), String> {
    let (relay, _) = open_db()?;
    let mut file = PathBuf::from(dest);
    if file.is_dir() || dest.ends_with(['/', '\\']) {
        std::fs::create_dir_all(&file).map_err(|e| format!("can't create {}: {e}", file.display()))?;
        file = file.join(format!("linecard-relay-{}.db", linecard_relay::stamp(now_ms())));
    }
    let info = relay.backup(&file)?;
    println!(
        "Backed up to {} ({}, {} groups, {} accounts). Everything in it is encrypted sync data, but it also holds hashes of the groups' tokens and accounts' keys: keep it private.",
        file.display(),
        size(info.bytes as i64),
        info.groups,
        info.accounts
    );
    Ok(())
}

/// `restore <path>`: replace the database with a backup. The relay must be stopped.
fn restore_command(from: &str) -> Result<(), String> {
    let listen = std::env::var("LINECARD_RELAY_ADDR").unwrap_or_else(|_| "0.0.0.0:8443".into());
    if local_addr(&listen).and_then(probe).is_ok() {
        return Err(format!(
            "a relay is answering on {listen}: stop it before restoring (the old database is kept aside either way)"
        ));
    }
    let db = db_path();
    let (info, aside) = linecard_relay::restore_backup(std::path::Path::new(from), &db)?;
    println!(
        "Restored {} groups and {} accounts from {from} into {}.",
        info.groups,
        info.accounts,
        db.display()
    );
    match aside {
        Some(old) => println!(
            "The database it replaced is kept at {}; delete it when you are sure.",
            old.display()
        ),
        None => println!("There was no database to replace."),
    }
    println!("Start the relay again.");
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn size(bytes: i64) -> String {
    match bytes {
        b if b < 10_000 => format!("{b} B"),
        b if b < 10_000_000 => format!("{:.0} KB", b as f64 / 1e3),
        b if b < 10_000_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b => format!("{:.1} GB", b as f64 / 1e9),
    }
}

fn admin(args: &[String]) -> Result<(), String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    // These two manage the database file itself, so they come before opening it.
    match words.as_slice() {
        ["backup", dest] => return backup_command(dest),
        ["restore", from] => return restore_command(from),
        ["backup"] | ["restore"] => return Err("usage: backup <file or folder> | restore <backup file>".into()),
        _ => {}
    }
    let (relay, _) = open_db()?;
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
        ["group", "list", rest @ ..] => {
            // `--idle [DAYS]`: only groups with no write or heartbeat for that long
            // (the daily cleanup's setting when no number is given).
            let groups = match rest {
                [] => relay.groups().map_err(err)?,
                ["--idle", days @ ..] => {
                    let days = match days {
                        [d] => d
                            .parse::<i64>()
                            .map_err(|_| "--idle needs a number of days".to_string())?,
                        [] if env_limits().idle_days > 0 => env_limits().idle_days,
                        _ => return Err("give a number of days: group list --idle 30".into()),
                    };
                    relay.idle_groups(days).map_err(err)?
                }
                _ => return Err("usage: group list [--idle [DAYS]]".into()),
            };
            if groups.is_empty() {
                println!("No sync groups.");
            }
            for g in groups {
                println!(
                    "{}  {:<16} {:>7} batches {:>9}  {} devices  last write {}, last seen {}",
                    g.id,
                    g.account.as_deref().unwrap_or("(open)"),
                    g.ops,
                    size(g.bytes),
                    g.devices,
                    when(g.last),
                    when(Some(g.active))
                );
            }
        }
        ["vacuum"] => {
            let free = relay.vacuum().map_err(err)?;
            println!(
                "Rebuilt the database; about {} of free space went back to the system.",
                size(free)
            );
        }
        ["group", "remove", id] => {
            relay.remove_group(id).map_err(err)?;
            println!("Removed sync group {id}.");
        }
        ["stats"] => {
            let groups = relay.groups().map_err(err)?;
            let ops: i64 = groups.iter().map(|g| g.ops).sum();
            let bytes: i64 = groups.iter().map(|g| g.bytes).sum();
            println!(
                "{} accounts, {} sync groups, {ops} batches stored ({})",
                relay.accounts().map_err(err)?.len(),
                groups.len(),
                size(bytes)
            );
            if let Some(big) = groups.iter().max_by_key(|g| g.bytes) {
                println!("largest group: {} ({})", big.id, size(big.bytes));
            }
            if let Some(old) = groups.iter().min_by_key(|g| g.active) {
                println!("quietest group: {} (last seen {})", old.id, when(Some(old.active)));
            }
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

/// Background care of the database: fold the write-ahead log into the file
/// every hour, and once a day delete idle groups and stale heartbeats.
async fn upkeep(relay: Arc<Relay>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
    let mut hours = 0u32;
    loop {
        tick.tick().await; // the first tick is immediate: run the daily round at start too
        let relay = relay.clone();
        let daily = hours.is_multiple_of(24);
        hours = hours.wrapping_add(1);
        let done = tokio::task::spawn_blocking(move || {
            if daily {
                match relay.maintain() {
                    Ok(m) => {
                        for g in &m.expired {
                            println!("removed idle sync group {g}");
                        }
                        if m.devices_removed > 0 {
                            println!("removed {} stale device records", m.devices_removed);
                        }
                    }
                    Err(e) => eprintln!("maintenance failed: {e:?}"),
                }
            }
            if daily {
                if let Some((dir, keep)) = backup_settings() {
                    match relay.backup_rotating(&dir, keep) {
                        Ok(Some(file)) => println!("backed up to {}", file.display()),
                        Ok(None) => {}
                        Err(e) => eprintln!("backup failed: {e}"),
                    }
                }
            }
            if let Err(e) = relay.checkpoint() {
                eprintln!("checkpoint failed: {e}");
            }
        })
        .await;
        if done.is_err() {
            eprintln!("maintenance task crashed");
        }
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
    match backup_settings() {
        Some((dir, keep)) => println!("backups: daily into {}, newest {keep} kept", dir.display()),
        None => println!("backups: not set up (LINECARD_RELAY_BACKUP_DIR)"),
    }
    let limits = env_limits();
    println!(
        "limits: {} batches and {} per group; idle groups {}",
        limits.max_ops,
        if limits.max_group_bytes > 0 {
            size(limits.max_group_bytes)
        } else {
            "no byte limit".into()
        },
        if limits.idle_days > 0 {
            format!("deleted after {} days", limits.idle_days)
        } else {
            "kept".into()
        }
    );
    let relay = Arc::new(relay);
    tokio::spawn(upkeep(relay.clone()));
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
    fn sizes_take_units_and_zero_means_unlimited() {
        assert_eq!(parse_size("256MiB"), Some(256 << 20));
        assert_eq!(parse_size(" 1 GiB "), Some(1 << 30));
        assert_eq!(parse_size("500KB"), Some(500_000));
        assert_eq!(parse_size("1000000"), Some(1_000_000));
        assert_eq!(parse_size("0"), Some(0));
        assert_eq!(parse_size("unlimited"), Some(0));
        assert_eq!(parse_size("lots"), None);
        assert_eq!(parse_size("12 parsecs"), None);
        assert_eq!(parse_size(""), None);
    }

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
                ..Default::default()
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
                ..Default::default()
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
