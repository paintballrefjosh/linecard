//! The relay's settings: read once from environment variables (12-factor),
//! checked, and describable (`linecard-relay config`).

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::{Limits, Rate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

/// One setting as `linecard-relay config` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub name: &'static str,
    /// What is in effect (secrets masked).
    pub value: String,
    /// Set in the environment (as opposed to the default).
    pub from_env: bool,
    pub note: &'static str,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub addr: SocketAddr,
    pub db: PathBuf,
    pub limits: Limits,
    /// Folder and count for the daily backups; `None` = off.
    pub backup: Option<(PathBuf, usize)>,
    pub log_level: String,
    pub log_format: LogFormat,
    /// Where `/metrics` is served (a separate listener); `None` = off.
    pub metrics_addr: Option<SocketAddr>,
    /// Things in the environment that were wrong and were replaced by defaults.
    pub warnings: Vec<String>,
    shown: Vec<Setting>,
}

/// "256MiB", "1GiB", "500000" or "0"/"off"/"unlimited" (= no limit) as bytes.
pub fn parse_size(text: &str) -> Option<i64> {
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

/// "20/s, burst 100", "20/s", "20" (burst defaults to 5 seconds' worth), or
/// "off"/"0" for no limit. `Ok(None)` means off.
pub fn parse_rate(text: &str) -> Result<Option<Rate>, String> {
    let t = text.trim().to_ascii_lowercase();
    if matches!(t.as_str(), "off" | "none" | "0" | "unlimited") {
        return Ok(None);
    }
    let bad = || format!("{text:?} is not a rate (try \"20/s, burst 100\")");
    let mut parts = t
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty());
    let first = parts.next().ok_or_else(bad)?;
    let per_sec: f64 = first.trim_end_matches("/s").parse().map_err(|_| bad())?;
    if per_sec <= 0.0 || !per_sec.is_finite() {
        return Err(bad());
    }
    let mut burst = (per_sec * 5.0).ceil();
    let rest: Vec<&str> = parts.collect();
    match rest.as_slice() {
        [] => {}
        ["burst", n] | [n] => burst = n.parse().map_err(|_| bad())?,
        _ => return Err(bad()),
    }
    if burst < 1.0 {
        return Err(bad());
    }
    Ok(Some(Rate { per_sec, burst }))
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

impl Config {
    pub fn from_env() -> Config {
        Config::from_vars(|name| std::env::var(name).ok())
    }

    /// Read settings through `var` (the environment, or a map in tests).
    /// Wrong values are reported in `warnings` and replaced by defaults.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Config {
        let mut warnings = Vec::new();
        let mut shown = Vec::new();
        let mut get = |name: &'static str, default: &str, note: &'static str| -> String {
            let set = var(name).filter(|v| !v.trim().is_empty());
            shown.push(Setting {
                name,
                value: String::new(), // filled in below, once validated
                from_env: set.is_some(),
                note,
            });
            set.unwrap_or_else(|| default.to_string())
        };
        let defaults = Limits::default();

        let addr_text = get("LINECARD_RELAY_ADDR", "0.0.0.0:8443", "listen address");
        let db_text = get("LINECARD_RELAY_DB", "linecard-relay.db", "SQLite database file");
        let open_text = get("LINECARD_RELAY_OPEN", "true", "anyone with a phrase may start groups");
        let ops_text = get(
            "LINECARD_RELAY_MAX_OPS",
            &defaults.max_ops.to_string(),
            "batches kept per group",
        );
        let bytes_text = get(
            "LINECARD_RELAY_MAX_GROUP_BYTES",
            "256MiB",
            "storage per group (0 = no limit)",
        );
        let idle_text = get(
            "LINECARD_RELAY_IDLE_DAYS",
            "0",
            "delete groups idle this many days (0 = never)",
        );
        let rate_text = get(
            "LINECARD_RELAY_RATE",
            "20/s, burst 100",
            "request limit per client address and per group (off = none)",
        );
        let proxy_text = get(
            "LINECARD_RELAY_TRUST_PROXY",
            "false",
            "take the client address from X-Forwarded-For (only behind your own proxy)",
        );
        let level_text = get(
            "LINECARD_RELAY_LOG",
            "info",
            "log level (error, warn, info, debug, trace)",
        );
        let format_text = get("LINECARD_RELAY_LOG_FORMAT", "text", "text or json");
        let metrics_text = get(
            "LINECARD_RELAY_METRICS_ADDR",
            "",
            "serve /metrics here, apart from the public port",
        );
        let backup_dir_text = get(
            "LINECARD_RELAY_BACKUP_DIR",
            "",
            "folder for daily backups (empty = none)",
        );
        let keep_text = get("LINECARD_RELAY_BACKUP_KEEP", "7", "daily backups kept");

        let mut warn = |msg: String| warnings.push(msg);

        let addr = addr_text.parse::<SocketAddr>().unwrap_or_else(|_| {
            warn(format!(
                "LINECARD_RELAY_ADDR={addr_text} is not host:port; using 0.0.0.0:8443"
            ));
            "0.0.0.0:8443".parse().expect("valid default")
        });
        let max_ops = ops_text.parse::<i64>().ok().filter(|n| *n > 0).unwrap_or_else(|| {
            warn(format!(
                "LINECARD_RELAY_MAX_OPS={ops_text} is not a positive number; using {}",
                defaults.max_ops
            ));
            defaults.max_ops
        });
        let max_group_bytes = parse_size(&bytes_text).unwrap_or_else(|| {
            warn(format!(
                "LINECARD_RELAY_MAX_GROUP_BYTES={bytes_text} is not a size (try 256MiB); using 256MiB"
            ));
            defaults.max_group_bytes
        });
        let idle_days = idle_text.parse::<i64>().ok().filter(|n| *n >= 0).unwrap_or_else(|| {
            warn(format!(
                "LINECARD_RELAY_IDLE_DAYS={idle_text} is not a number of days; using 0 (never)"
            ));
            0
        });
        let rate = parse_rate(&rate_text).unwrap_or_else(|e| {
            warn(format!("LINECARD_RELAY_RATE: {e}; using 20/s, burst 100"));
            parse_rate("20/s, burst 100").ok().flatten()
        });
        let level = level_text.trim().to_ascii_lowercase();
        let log_level = if matches!(level.as_str(), "error" | "warn" | "info" | "debug" | "trace") {
            level
        } else {
            warn(format!("LINECARD_RELAY_LOG={level_text} is not a level; using info"));
            "info".into()
        };
        let log_format = match format_text.trim().to_ascii_lowercase().as_str() {
            "text" => LogFormat::Text,
            "json" => LogFormat::Json,
            _ => {
                warn(format!(
                    "LINECARD_RELAY_LOG_FORMAT={format_text} is not text or json; using text"
                ));
                LogFormat::Text
            }
        };
        let metrics_addr = if metrics_text.trim().is_empty() {
            None
        } else {
            metrics_text.parse::<SocketAddr>().ok().or_else(|| {
                warn(format!(
                    "LINECARD_RELAY_METRICS_ADDR={metrics_text} is not host:port; metrics are off"
                ));
                None
            })
        };
        let keep = keep_text.parse::<usize>().ok().filter(|n| *n > 0).unwrap_or_else(|| {
            warn(format!(
                "LINECARD_RELAY_BACKUP_KEEP={keep_text} is not a positive number; using 7"
            ));
            7
        });
        let backup = (!backup_dir_text.trim().is_empty()).then(|| (PathBuf::from(backup_dir_text.trim()), keep));

        let limits = Limits {
            max_ops,
            open: !matches!(open_text.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no"),
            max_group_bytes,
            idle_days,
            rate,
            trust_proxy: truthy(&proxy_text),
        };

        // What `config` shows: the values in effect.
        let effective = |name: &str| -> String {
            match name {
                "LINECARD_RELAY_ADDR" => addr.to_string(),
                "LINECARD_RELAY_DB" => db_text.clone(),
                "LINECARD_RELAY_OPEN" => limits.open.to_string(),
                "LINECARD_RELAY_MAX_OPS" => max_ops.to_string(),
                "LINECARD_RELAY_MAX_GROUP_BYTES" => match max_group_bytes {
                    0 => "no limit".into(),
                    b => format!("{b} bytes"),
                },
                "LINECARD_RELAY_IDLE_DAYS" => match idle_days {
                    0 => "never".into(),
                    d => format!("{d} days"),
                },
                "LINECARD_RELAY_RATE" => match &limits.rate {
                    None => "off".into(),
                    Some(r) => format!("{}/s, burst {}", r.per_sec, r.burst),
                },
                "LINECARD_RELAY_TRUST_PROXY" => limits.trust_proxy.to_string(),
                "LINECARD_RELAY_LOG" => log_level.clone(),
                "LINECARD_RELAY_LOG_FORMAT" => format!("{log_format:?}").to_lowercase(),
                "LINECARD_RELAY_METRICS_ADDR" => metrics_addr.map_or("off".into(), |a| a.to_string()),
                "LINECARD_RELAY_BACKUP_DIR" => backup.as_ref().map_or("off".into(), |(d, _)| d.display().to_string()),
                "LINECARD_RELAY_BACKUP_KEEP" => keep.to_string(),
                _ => String::new(),
            }
        };
        for s in &mut shown {
            s.value = mask(s.name, effective(s.name));
        }

        Config {
            addr,
            db: PathBuf::from(db_text),
            limits,
            backup,
            log_level,
            log_format,
            metrics_addr,
            warnings,
            shown,
        }
    }

    /// Every setting, in the order `config` prints them.
    pub fn settings(&self) -> &[Setting] {
        &self.shown
    }
}

/// Settings whose names say they hold a secret are never printed in full.
fn mask(name: &str, value: String) -> String {
    let secret = ["KEY", "TOKEN", "SECRET", "PASSWORD"].iter().any(|w| name.contains(w));
    if secret && !value.is_empty() {
        "********".into()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn with(vars: &[(&str, &str)]) -> Config {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_vars(|n| map.get(n).cloned())
    }

    #[test]
    fn rates_parse_in_the_forms_people_write() {
        let r = |t: &str| parse_rate(t).unwrap();
        assert_eq!(
            r("20/s, burst 100"),
            Some(Rate {
                per_sec: 20.0,
                burst: 100.0
            })
        );
        assert_eq!(
            r("20/s burst 100"),
            Some(Rate {
                per_sec: 20.0,
                burst: 100.0
            })
        );
        assert_eq!(
            r("5/s"),
            Some(Rate {
                per_sec: 5.0,
                burst: 25.0
            }),
            "burst defaults to 5 seconds' worth"
        );
        assert_eq!(
            r("2.5"),
            Some(Rate {
                per_sec: 2.5,
                burst: 13.0
            })
        );
        assert_eq!(
            r("10, 40"),
            Some(Rate {
                per_sec: 10.0,
                burst: 40.0
            })
        );
        assert_eq!(r("off"), None);
        assert_eq!(r("0"), None);
        for bad in [
            "",
            "fast",
            "-3/s",
            "20/s, burst",
            "20/s, burst 0",
            "20/s burst lots",
            "1 2 3",
        ] {
            assert!(parse_rate(bad).is_err(), "{bad:?}");
        }
    }

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
    fn defaults_when_nothing_is_set() {
        let c = with(&[]);
        assert_eq!(c.addr.to_string(), "0.0.0.0:8443");
        assert_eq!(c.limits.max_ops, 200_000);
        assert!(c.limits.open && !c.limits.trust_proxy);
        assert_eq!(c.limits.max_group_bytes, 256 << 20);
        assert_eq!(
            c.limits.rate,
            Some(Rate {
                per_sec: 20.0,
                burst: 100.0
            })
        );
        assert_eq!((c.log_level.as_str(), c.log_format), ("info", LogFormat::Text));
        assert!(c.metrics_addr.is_none() && c.backup.is_none() && c.warnings.is_empty());
        assert!(c.settings().iter().all(|s| !s.from_env));
    }

    #[test]
    fn values_in_the_environment_take_effect_and_are_flagged() {
        let c = with(&[
            ("LINECARD_RELAY_OPEN", "false"),
            ("LINECARD_RELAY_RATE", "off"),
            ("LINECARD_RELAY_TRUST_PROXY", "true"),
            ("LINECARD_RELAY_LOG_FORMAT", "JSON"),
            ("LINECARD_RELAY_METRICS_ADDR", "127.0.0.1:9100"),
            ("LINECARD_RELAY_BACKUP_DIR", "/backups"),
            ("LINECARD_RELAY_BACKUP_KEEP", "3"),
        ]);
        assert!(!c.limits.open && c.limits.rate.is_none() && c.limits.trust_proxy);
        assert_eq!(c.log_format, LogFormat::Json);
        assert_eq!(c.metrics_addr.unwrap().to_string(), "127.0.0.1:9100");
        assert_eq!(c.backup, Some((PathBuf::from("/backups"), 3)));
        let shown: Vec<_> = c.settings().iter().filter(|s| s.from_env).map(|s| s.name).collect();
        assert_eq!(shown.len(), 7, "{shown:?}");
        let rate = c.settings().iter().find(|s| s.name == "LINECARD_RELAY_RATE").unwrap();
        assert_eq!(rate.value, "off");
    }

    #[test]
    fn wrong_values_fall_back_with_a_warning_each() {
        let c = with(&[
            ("LINECARD_RELAY_ADDR", "nonsense"),
            ("LINECARD_RELAY_MAX_OPS", "-4"),
            ("LINECARD_RELAY_MAX_GROUP_BYTES", "lots"),
            ("LINECARD_RELAY_IDLE_DAYS", "soon"),
            ("LINECARD_RELAY_RATE", "fast"),
            ("LINECARD_RELAY_LOG", "loud"),
            ("LINECARD_RELAY_LOG_FORMAT", "xml"),
            ("LINECARD_RELAY_METRICS_ADDR", "9100"),
            ("LINECARD_RELAY_BACKUP_KEEP", "0"),
        ]);
        assert_eq!(c.warnings.len(), 9, "{:#?}", c.warnings);
        assert_eq!(c.addr.to_string(), "0.0.0.0:8443");
        assert_eq!(c.limits.max_ops, 200_000);
        assert_eq!(
            c.limits.rate,
            Some(Rate {
                per_sec: 20.0,
                burst: 100.0
            })
        );
        assert!(c.metrics_addr.is_none());
    }

    #[test]
    fn secrets_are_masked_in_the_description() {
        assert_eq!(mask("LINECARD_RELAY_ACCESS_KEY", "lcr_abc".into()), "********");
        assert_eq!(mask("LINECARD_RELAY_ADDR", "0.0.0.0:8443".into()), "0.0.0.0:8443");
        assert_eq!(mask("LINECARD_RELAY_TOKEN", String::new()), "", "nothing to hide");
    }
}
