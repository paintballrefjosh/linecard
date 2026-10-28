//! OpenSSH client config (`~/.ssh/config`) import.
//!
//! Each concrete `Host` alias becomes an SSH connection with its HostName,
//! Port, User and ProxyJump. Wildcard blocks (`Host *`, `Host 10.*`) set
//! defaults in OpenSSH; they're applied to the aliases they match, in file
//! order, first value wins, as `ssh` does.

use std::path::Path;

use linecard_core_model::{ConnectionKind, Endpoint};

use crate::{Import, ImportedConnection};

struct Block {
    patterns: Vec<String>,
    options: Vec<(String, String)>,
}

/// `Key value` or `Key=value`, keys case-insensitive, `#` comments.
fn parse_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..split].to_ascii_lowercase();
    let value = line[split..]
        .trim_start_matches(|c: char| c.is_whitespace() || c == '=')
        .trim();
    Some((key, value.trim_matches('"').to_string()))
}

/// OpenSSH glob: `*` any run, `?` one character; `!` negations are skipped.
fn glob(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

fn is_pattern(s: &str) -> bool {
    s.contains(['*', '?', '!'])
}

pub fn parse(text: &str) -> Import {
    let mut import = Import {
        source: "ssh_config".into(),
        ..Default::default()
    };
    let mut blocks: Vec<Block> = vec![Block {
        patterns: vec!["*".into()], // options before the first Host apply to all
        options: vec![],
    }];
    for line in text.lines() {
        let Some((key, value)) = parse_line(line) else { continue };
        match key.as_str() {
            "host" => blocks.push(Block {
                patterns: value.split_whitespace().map(str::to_string).collect(),
                options: vec![],
            }),
            "match" => {
                import.warnings.push(format!(
                    "Skipped a 'Match {value}' block: conditional blocks aren't imported"
                ));
                blocks.push(Block {
                    patterns: vec![],
                    options: vec![],
                });
            }
            "include" => import
                .warnings
                .push(format!("Not followed: 'Include {value}' (import that file separately)")),
            _ => blocks.last_mut().unwrap().options.push((key, value)),
        }
    }

    let aliases: Vec<String> = blocks
        .iter()
        .flat_map(|b| b.patterns.iter())
        .filter(|p| !is_pattern(p))
        .cloned()
        .collect();
    for alias in aliases {
        // First value for each option across all matching blocks wins.
        let get = |key: &str| {
            blocks
                .iter()
                .filter(|b| {
                    b.patterns.iter().any(|p| glob(p, &alias))
                        && !b
                            .patterns
                            .iter()
                            .any(|p| p.strip_prefix('!').is_some_and(|n| glob(n, &alias)))
                })
                .flat_map(|b| b.options.iter())
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        if import.connections.iter().any(|c| c.name == alias) {
            continue;
        }
        let host = get("hostname").unwrap_or_else(|| alias.clone()).replace("%h", &alias);
        let port = get("port").and_then(|p| p.parse().ok()).unwrap_or(22);
        // ProxyJump may list several hops; the one nearest the target is last.
        let jump = get("proxyjump")
            .filter(|j| !j.eq_ignore_ascii_case("none"))
            .and_then(|j| j.split(',').next_back().map(|h| h.trim().to_string()))
            .map(|h| h.rsplit_once('@').map_or(h.clone(), |(_, host)| host.to_string()));
        if get("proxycommand").is_some() {
            import.warnings.push(format!(
                "'{alias}': ProxyCommand isn't supported (the connection is imported without it)"
            ));
        }
        import.connections.push(ImportedConnection {
            folder_path: vec![],
            name: alias.clone(),
            kind: ConnectionKind::Ssh,
            endpoint: Endpoint::Network { host, port },
            username: get("user"),
            jump_host: jump,
            notes: String::new(),
        });
    }
    import
}

pub fn import_file(path: &Path) -> std::io::Result<Import> {
    Ok(parse(&std::fs::read_to_string(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "
# lab
Host bastion
    HostName 203.0.113.10
    User jump

Host core1 core2
  HostName %h.dc1.example.net
  ProxyJump jump@bastion

Host sw-*
  User netadmin
  Port 2222

Host sw-edge1
  HostName=10.0.0.5

Host legacy
  HostName 10.9.9.9
  ProxyCommand nc -X 5 -x proxy:1080 %h %p

Match host *.example.net
  User someone

Host *
  User fallback
";

    #[test]
    fn hosts_options_wildcards_and_jumps() {
        let imp = parse(CONFIG);
        let names: Vec<_> = imp.connections.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["bastion", "core1", "core2", "sw-edge1", "legacy"]);
        let get = |n: &str| imp.connections.iter().find(|c| c.name == n).unwrap();

        assert_eq!(get("bastion").username.as_deref(), Some("jump"));
        assert_eq!(
            get("core2").endpoint,
            Endpoint::Network {
                host: "core2.dc1.example.net".into(),
                port: 22
            }
        );
        assert_eq!(get("core1").jump_host.as_deref(), Some("bastion"));
        assert_eq!(
            get("core1").username.as_deref(),
            Some("fallback"),
            "Host * applies last"
        );
        let edge = get("sw-edge1");
        assert_eq!(edge.username.as_deref(), Some("netadmin"), "from the sw-* block");
        assert_eq!(
            edge.endpoint,
            Endpoint::Network {
                host: "10.0.0.5".into(),
                port: 2222
            }
        );
        assert_eq!(imp.warnings.len(), 2, "{:?}", imp.warnings); // Match + ProxyCommand
    }

    #[test]
    fn globbing() {
        assert!(glob("sw-*", "sw-edge1"));
        assert!(glob("10.0.?.1", "10.0.5.1"));
        assert!(!glob("sw-*", "core1"));
    }
}
