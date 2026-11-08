//! What the relay says about itself: one log line per request, Prometheus
//! metrics, and readiness. Logs and metrics never contain tokens, account
//! keys or blobs; groups show only as their first eight characters.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::guard::{client_ip, group_of, short};
use crate::Shared;

/// Upper edges of the latency buckets, in seconds. The long-poll sits in the
/// last ones by design.
const BUCKETS: [f64; 9] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 1.0, 5.0, 30.0];

#[derive(Default)]
struct Latency {
    /// Per bucket, not cumulative; the last slot is "longer than all of them".
    counts: [u64; BUCKETS.len() + 1],
    sum: f64,
    count: u64,
}

#[derive(Default)]
pub struct Metrics {
    requests: Mutex<BTreeMap<(String, u16), u64>>,
    latency: Mutex<BTreeMap<String, Latency>>,
    limited: Mutex<BTreeMap<&'static str, u64>>,
    waiting: AtomicI64,
}

/// What the database holds, read when metrics are scraped.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stored {
    pub groups: i64,
    pub accounts: i64,
    pub batches: i64,
    pub bytes: i64,
    pub devices: i64,
    pub db_bytes: u64,
}

impl Metrics {
    pub fn record(&self, route: &str, status: u16, seconds: f64) {
        *self
            .requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry((route.to_string(), status))
            .or_default() += 1;
        let mut all = self.latency.lock().unwrap_or_else(|p| p.into_inner());
        let l = all.entry(route.to_string()).or_default();
        let slot = BUCKETS.iter().position(|b| seconds <= *b).unwrap_or(BUCKETS.len());
        l.counts[slot] += 1;
        l.sum += seconds;
        l.count += 1;
    }

    /// A request was turned away by a limit (`address`, `group`, `blocked`, `held`).
    pub fn limited(&self, kind: &'static str) {
        *self
            .limited
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(kind)
            .or_default() += 1;
    }

    pub fn wait_started(&self) {
        self.waiting.fetch_add(1, Ordering::Relaxed);
    }

    pub fn wait_ended(&self) {
        self.waiting.fetch_sub(1, Ordering::Relaxed);
    }

    /// Prometheus text format.
    pub fn render(&self, stored: &Stored) -> String {
        let mut out = String::new();
        let mut put = |s: &str| {
            out.push_str(s);
            out.push('\n');
        };
        put("# HELP linecard_relay_requests_total Requests answered, by route and status.");
        put("# TYPE linecard_relay_requests_total counter");
        for ((route, status), n) in self.requests.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            put(&format!(
                "linecard_relay_requests_total{{route=\"{route}\",status=\"{status}\"}} {n}"
            ));
        }
        put("# HELP linecard_relay_request_duration_seconds Time to answer, by route.");
        put("# TYPE linecard_relay_request_duration_seconds histogram");
        for (route, l) in self.latency.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            let mut running = 0;
            for (i, edge) in BUCKETS.iter().enumerate() {
                running += l.counts[i];
                put(&format!(
                    "linecard_relay_request_duration_seconds_bucket{{route=\"{route}\",le=\"{edge}\"}} {running}"
                ));
            }
            put(&format!(
                "linecard_relay_request_duration_seconds_bucket{{route=\"{route}\",le=\"+Inf\"}} {}",
                l.count
            ));
            put(&format!(
                "linecard_relay_request_duration_seconds_sum{{route=\"{route}\"}} {}",
                l.sum
            ));
            put(&format!(
                "linecard_relay_request_duration_seconds_count{{route=\"{route}\"}} {}",
                l.count
            ));
        }
        put("# HELP linecard_relay_rate_limited_total Requests turned away by a limit.");
        put("# TYPE linecard_relay_rate_limited_total counter");
        for (kind, n) in self.limited.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            put(&format!("linecard_relay_rate_limited_total{{kind=\"{kind}\"}} {n}"));
        }
        let mut gauge = |name: &str, help: &str, value: String| {
            let _ = writeln!(out, "# HELP linecard_relay_{name} {help}\n# TYPE linecard_relay_{name} gauge\nlinecard_relay_{name} {value}");
        };
        gauge(
            "waiting_requests",
            "Long-poll requests currently held open.",
            self.waiting.load(Ordering::Relaxed).to_string(),
        );
        gauge("groups", "Sync groups stored.", stored.groups.to_string());
        gauge("accounts", "Accounts.", stored.accounts.to_string());
        gauge("batches", "Batches of changes stored.", stored.batches.to_string());
        gauge("stored_bytes", "Bytes of batches stored.", stored.bytes.to_string());
        gauge(
            "devices",
            "Computers that have reported in.",
            stored.devices.to_string(),
        );
        gauge(
            "database_bytes",
            "Size of the database file.",
            stored.db_bytes.to_string(),
        );
        out
    }
}

/// `/v1/groups/{group}/ops` for logs and metrics: ids never become labels.
pub fn route_of(path: &str) -> String {
    let mut parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() >= 3 && parts[0] == "v1" && parts[1] == "groups" {
        parts[2] = "{group}";
        if parts.len() >= 5 && parts[3] == "devices" {
            parts[4] = "{device}";
        }
        return format!("/{}", parts.join("/"));
    }
    match path {
        "/health" | "/ready" | "/metrics" => path.to_string(),
        _ => "other".to_string(),
    }
}

/// One log line per request, and the counts and timings for `/metrics`.
pub async fn observe(State(relay): State<Shared>, req: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let route = route_of(&path);
    let group = short(group_of(&path));
    let ip = client_ip(&req, relay.guard().trust_proxy);
    let waiting = path.ends_with("/ops/wait");
    if waiting {
        relay.metrics().wait_started();
    }
    let response = next.run(req).await;
    if waiting {
        relay.metrics().wait_ended();
    }
    let seconds = started.elapsed().as_secs_f64();
    let status = response.status().as_u16();
    relay.metrics().record(&route, status, seconds);
    // Probes would drown everything else.
    if route != "/health" && route != "/ready" {
        let ip = ip.map_or_else(|| "-".to_string(), |i| i.to_string());
        let ms = (seconds * 1000.0).round() as u64;
        if status >= 500 {
            tracing::error!(%method, %route, status, ms, %group, %ip, "request");
        } else if status == 429 || status == 401 || status == 403 {
            tracing::warn!(%method, %route, status, ms, %group, %ip, "request");
        } else {
            tracing::info!(%method, %route, status, ms, %group, %ip, "request");
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_never_carry_ids() {
        let g = "0123456789abcdef0123456789abcdef";
        assert_eq!(route_of(&format!("/v1/groups/{g}/ops")), "/v1/groups/{group}/ops");
        assert_eq!(
            route_of(&format!("/v1/groups/{g}/ops/wait")),
            "/v1/groups/{group}/ops/wait"
        );
        assert_eq!(
            route_of(&format!("/v1/groups/{g}/devices/laptop-1")),
            "/v1/groups/{group}/devices/{device}"
        );
        assert_eq!(route_of("/health"), "/health");
        assert_eq!(route_of("/wp-login.php"), "other", "scanner paths don't become labels");
    }

    #[test]
    fn metrics_render_as_prometheus_text() {
        let m = Metrics::default();
        m.record("/v1/groups/{group}/ops", 200, 0.003);
        m.record("/v1/groups/{group}/ops", 200, 0.2);
        m.record("/v1/groups/{group}/ops", 401, 40.0);
        m.limited("address");
        (0..4).for_each(|_| m.wait_started());
        m.limited("address");
        let text = m.render(&Stored {
            groups: 2,
            accounts: 1,
            batches: 50,
            bytes: 1234,
            devices: 3,
            db_bytes: 8192,
        });
        let has = |line: &str| assert!(text.lines().any(|l| l == line), "missing {line:?} in:\n{text}");
        has("linecard_relay_requests_total{route=\"/v1/groups/{group}/ops\",status=\"200\"} 2");
        has("linecard_relay_requests_total{route=\"/v1/groups/{group}/ops\",status=\"401\"} 1");
        // Cumulative buckets: 0.003 is in the first; 0.2 joins at 0.25; 40 s only in +Inf.
        has("linecard_relay_request_duration_seconds_bucket{route=\"/v1/groups/{group}/ops\",le=\"0.005\"} 1");
        has("linecard_relay_request_duration_seconds_bucket{route=\"/v1/groups/{group}/ops\",le=\"0.25\"} 2");
        has("linecard_relay_request_duration_seconds_bucket{route=\"/v1/groups/{group}/ops\",le=\"30\"} 2");
        has("linecard_relay_request_duration_seconds_bucket{route=\"/v1/groups/{group}/ops\",le=\"+Inf\"} 3");
        has("linecard_relay_request_duration_seconds_count{route=\"/v1/groups/{group}/ops\"} 3");
        has("linecard_relay_rate_limited_total{kind=\"address\"} 2");
        has("linecard_relay_waiting_requests 4");
        has("linecard_relay_stored_bytes 1234");
        has("linecard_relay_database_bytes 8192");
        assert!(text.contains("# TYPE linecard_relay_groups gauge"));
        // No token, key or blob can appear: labels are routes and statuses only.
        assert!(!text.contains("Bearer") && !text.contains("lcr_"));
    }
}
