//! Protection against abuse: request rate limits per client address and per
//! sync group, a cool-down for addresses that keep sending wrong tokens, caps
//! on held-open requests, and a time limit for everything except the
//! long-poll. Nothing here sees a token or a blob; groups appear only by id.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::{valid_group, Shared};

/// Requests per second, with room for a burst.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub per_sec: f64,
    pub burst: f64,
}

/// A token bucket per key.
pub struct RateLimiter {
    rate: Rate,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn new(rate: Rate) -> Self {
        RateLimiter {
            rate,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one token for `key`, or say how many seconds until there is one.
    pub fn check(&self, key: &str, now: Instant) -> Result<(), u64> {
        let mut map = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() > 20_000 {
            // Forget keys that have refilled completely (they behave as new).
            let (per_sec, burst) = (self.rate.per_sec, self.rate.burst);
            map.retain(|_, (tokens, last)| {
                *tokens + now.saturating_duration_since(*last).as_secs_f64() * per_sec < burst
            });
        }
        let (tokens, last) = map.entry(key.to_string()).or_insert((self.rate.burst, now));
        let elapsed = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * self.rate.per_sec).min(self.rate.burst);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - *tokens) / self.rate.per_sec).ceil().max(1.0) as u64)
        }
    }
}

/// Wrong tokens per client address: too many in a short time and the address
/// is turned away for a while. (Tokens are 256-bit derived, so guessing one
/// is hopeless; this keeps the attempt from costing the relay anything.)
pub struct Failures {
    map: Mutex<HashMap<IpAddr, Strikes>>,
}

#[derive(Default)]
struct Strikes {
    count: u32,
    since: Option<Instant>,
    blocked_until: Option<Instant>,
}

const STRIKE_WINDOW: Duration = Duration::from_secs(300);
const MAX_STRIKES: u32 = 20;
const BLOCK_FOR: Duration = Duration::from_secs(300);

impl Failures {
    pub fn new() -> Self {
        Failures {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Seconds left if `ip` is turned away.
    pub fn blocked(&self, ip: IpAddr, now: Instant) -> Option<u64> {
        let map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let until = map.get(&ip)?.blocked_until?;
        (until > now).then(|| (until - now).as_secs() + 1)
    }

    /// Note a wrong token. True when this one tipped the address into a block.
    pub fn strike(&self, ip: IpAddr, now: Instant) -> bool {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() > 20_000 {
            map.retain(|_, s| {
                s.blocked_until.is_some_and(|u| u > now) || s.since.is_some_and(|t| now - t < STRIKE_WINDOW)
            });
        }
        let s = map.entry(ip).or_default();
        if s.since.is_none_or(|t| now.saturating_duration_since(t) > STRIKE_WINDOW) {
            s.count = 0;
            s.since = Some(now);
        }
        s.count += 1;
        if s.count >= MAX_STRIKES && !s.blocked_until.is_some_and(|u| u > now) {
            s.blocked_until = Some(now + BLOCK_FOR);
            s.count = 0;
            return true;
        }
        false
    }
}

impl Default for Failures {
    fn default() -> Self {
        Self::new()
    }
}

/// Requests held open at once (the long-poll), per key.
pub struct Held {
    counts: Mutex<HashMap<String, usize>>,
}

/// At most this many long-polls per client address and per group. A computer
/// holds one; a household or an office has a handful.
pub const MAX_HELD: usize = 32;

/// Holds a slot until dropped.
pub struct Slot {
    held: Arc<Held>,
    keys: [String; 2],
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut counts = self.held.counts.lock().unwrap_or_else(|p| p.into_inner());
        for k in &self.keys {
            if let Some(n) = counts.get_mut(k) {
                *n -= 1;
                if *n == 0 {
                    counts.remove(k);
                }
            }
        }
    }
}

impl Held {
    pub fn new() -> Arc<Self> {
        Arc::new(Held {
            counts: Mutex::new(HashMap::new()),
        })
    }

    /// A slot for this address and group, unless either is at its cap.
    pub fn take(self: &Arc<Self>, ip: &str, group: &str, cap: usize) -> Option<Slot> {
        let keys = [format!("ip:{ip}"), format!("group:{group}")];
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        if keys.iter().any(|k| counts.get(k).copied().unwrap_or(0) >= cap) {
            return None;
        }
        for k in &keys {
            *counts.entry(k.clone()).or_insert(0) += 1;
        }
        drop(counts);
        Some(Slot {
            held: self.clone(),
            keys,
        })
    }

    pub fn total(&self) -> usize {
        // Each slot counts once under its address key.
        self.counts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(k, _)| k.starts_with("ip:"))
            .map(|(_, n)| n)
            .sum()
    }
}

/// Everything the middleware needs, built from the limits.
pub struct Guard {
    pub by_ip: Option<RateLimiter>,
    pub by_group: Option<RateLimiter>,
    pub failures: Failures,
    pub held: Arc<Held>,
    pub trust_proxy: bool,
}

impl Guard {
    pub fn new(rate: Option<Rate>, trust_proxy: bool) -> Self {
        Guard {
            by_ip: rate.map(RateLimiter::new),
            // A group is shared by a few computers, so it gets the same allowance.
            by_group: rate.map(RateLimiter::new),
            failures: Failures::new(),
            held: Held::new(),
            trust_proxy,
        }
    }
}

/// How long any request but the long-poll may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The client's address: the connection's, or (behind your own proxy) the
/// last `X-Forwarded-For` entry, which is the one the proxy added. Earlier
/// entries are whatever the client claimed and can't be trusted.
pub fn client_ip(req: &Request, trust_proxy: bool) -> Option<IpAddr> {
    if trust_proxy {
        let forwarded = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok());
        if forwarded.is_some() {
            return forwarded;
        }
    }
    let ext = req.extensions();
    ext.get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .or_else(|| ext.get::<ConnectInfo<crate::tls::Peer>>().map(|c| c.0 .0.ip()))
}

/// The group id in `/v1/groups/{group}/…`, if there is a plausible one.
pub fn group_of(path: &str) -> Option<&str> {
    let mut parts = path.trim_start_matches('/').split('/');
    (parts.next()? == "v1" && parts.next()? == "groups")
        .then(|| parts.next())
        .flatten()
        .filter(|g| valid_group(g))
}

fn refuse(status: StatusCode, retry_after: u64, why: &str) -> Response {
    let mut res = (status, axum::Json(serde_json::json!({ "error": why }))).into_response();
    if let Ok(v) = HeaderValue::from_str(&retry_after.to_string()) {
        res.headers_mut().insert(header::RETRY_AFTER, v);
    }
    res
}

/// The middleware: limits first, then the request, then a note of wrong tokens.
pub async fn guard(State(relay): State<Shared>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // Probes and monitoring aren't limited.
    if path == "/health" || path == "/ready" {
        return next.run(req).await;
    }
    let g = relay.guard();
    let now = Instant::now();
    let ip = client_ip(&req, g.trust_proxy);
    let ip_key = ip.map_or_else(|| "unknown".to_string(), |i| i.to_string());
    let group = group_of(&path).map(str::to_string);

    if let Some(ip) = ip {
        if let Some(secs) = g.failures.blocked(ip, now) {
            relay.metrics().limited("blocked");
            return refuse(
                StatusCode::TOO_MANY_REQUESTS,
                secs,
                "too many wrong tokens from this address; try again later",
            );
        }
    }
    if let Some(limiter) = &g.by_ip {
        if let Err(secs) = limiter.check(&ip_key, now) {
            relay.metrics().limited("address");
            return refuse(
                StatusCode::TOO_MANY_REQUESTS,
                secs,
                "too many requests from this address",
            );
        }
    }
    if let (Some(limiter), Some(group)) = (&g.by_group, &group) {
        if let Err(secs) = limiter.check(group, now) {
            relay.metrics().limited("group");
            return refuse(
                StatusCode::TOO_MANY_REQUESTS,
                secs,
                "too many requests for this sync group",
            );
        }
    }

    let long_poll = path.ends_with("/ops/wait");
    let socket = path.ends_with("/ws");
    let _slot = if long_poll || socket {
        match g.held.take(&ip_key, group.as_deref().unwrap_or("-"), MAX_HELD) {
            Some(slot) => Some(slot),
            None => {
                relay.metrics().limited("held");
                return refuse(
                    StatusCode::TOO_MANY_REQUESTS,
                    5,
                    "too many waiting requests; wait a few seconds",
                );
            }
        }
    } else {
        None
    };
    // A socket outlives this call: the handler keeps its slot until the socket closes.
    let _slot = match (socket, _slot) {
        (true, Some(slot)) => {
            req.extensions_mut().insert(Arc::new(slot));
            None
        }
        (_, slot) => slot,
    };

    let response = if long_poll {
        next.run(req).await
    } else {
        match tokio::time::timeout(REQUEST_TIMEOUT, next.run(req)).await {
            Ok(r) => r,
            Err(_) => return (StatusCode::REQUEST_TIMEOUT, "request took too long").into_response(),
        }
    };

    if response.status() == StatusCode::UNAUTHORIZED {
        if let Some(ip) = ip {
            if g.failures.strike(ip, Instant::now()) {
                tracing::warn!(%ip, group = %short(group.as_deref()), "turning this address away for a while: too many wrong tokens");
            }
        }
    }
    response
}

/// A group id shortened for logs.
pub fn short(group: Option<&str>) -> String {
    match group {
        Some(g) if g.len() > 8 => format!("{}…", &g[..8]),
        Some(g) => g.to_string(),
        None => "-".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: f64, base: Instant) -> Instant {
        base + Duration::from_secs_f64(secs)
    }

    #[test]
    fn a_bucket_allows_a_burst_then_the_steady_rate() {
        let t0 = Instant::now();
        let l = RateLimiter::new(Rate {
            per_sec: 2.0,
            burst: 4.0,
        });
        for _ in 0..4 {
            assert_eq!(l.check("a", t0), Ok(()));
        }
        assert_eq!(
            l.check("a", t0),
            Err(1),
            "empty: the next token is half a second off, shown as 1 s"
        );
        assert_eq!(l.check("a", at(0.5, t0)), Ok(()), "one token back after 0.5 s");
        assert!(l.check("a", at(0.5, t0)).is_err());
        // Others are unaffected, and a long rest refills only to the burst.
        assert_eq!(l.check("b", t0), Ok(()));
        for _ in 0..4 {
            assert_eq!(l.check("a", at(100.0, t0)), Ok(()));
        }
        assert!(l.check("a", at(100.0, t0)).is_err());
    }

    #[test]
    fn retry_after_is_honest_for_slow_rates() {
        let t0 = Instant::now();
        let l = RateLimiter::new(Rate {
            per_sec: 0.2,
            burst: 1.0,
        });
        assert_eq!(l.check("a", t0), Ok(()));
        assert_eq!(l.check("a", t0), Err(5), "one token every 5 s");
        assert_eq!(l.check("a", at(2.0, t0)), Err(3));
        assert_eq!(l.check("a", at(5.0, t0)), Ok(()));
    }

    #[test]
    fn repeated_wrong_tokens_block_an_address_for_a_while() {
        let t0 = Instant::now();
        let f = Failures::new();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let other: IpAddr = "203.0.113.10".parse().unwrap();
        for i in 0..MAX_STRIKES - 1 {
            assert!(!f.strike(ip, at(i as f64, t0)));
        }
        assert_eq!(f.blocked(ip, t0), None, "not yet");
        assert!(f.strike(ip, at(30.0, t0)), "the 20th wrong token tips it over");
        assert!(f.blocked(ip, at(31.0, t0)).is_some_and(|s| (268..=300).contains(&s)));
        assert_eq!(f.blocked(other, at(31.0, t0)), None, "other addresses are fine");
        assert_eq!(f.blocked(ip, at(331.0, t0)), None, "the block ends");
        // Strikes spread over more than the window don't add up.
        let slow: IpAddr = "203.0.113.11".parse().unwrap();
        for i in 0..40 {
            assert!(
                !f.strike(slow, at(i as f64 * 20.0, t0)),
                "one every 20 s is under the window's allowance"
            );
        }
    }

    #[test]
    fn long_polls_are_capped_per_address_and_per_group_and_slots_come_back() {
        let held = Held::new();
        let slots: Vec<Slot> = (0..3).map(|_| held.take("1.1.1.1", "g1", 3).unwrap()).collect();
        assert_eq!(held.total(), 3);
        assert!(held.take("1.1.1.1", "g2", 3).is_none(), "that address is at its cap");
        assert!(held.take("2.2.2.2", "g1", 3).is_none(), "that group is at its cap");
        assert!(held.take("2.2.2.2", "g2", 3).is_some(), "an unrelated one is fine");
        drop(slots);
        assert_eq!(held.total(), 0);
        assert!(held.take("1.1.1.1", "g1", 3).is_some(), "slots were released");
    }

    #[test]
    fn finds_the_group_and_the_client() {
        let g = "0123456789abcdef0123456789abcdef";
        assert_eq!(group_of(&format!("/v1/groups/{g}/ops")), Some(g));
        assert_eq!(group_of(&format!("/v1/groups/{g}")), Some(g));
        assert_eq!(group_of("/v1/groups/short/ops"), None);
        assert_eq!(group_of("/health"), None);

        let req = |xff: Option<&str>| {
            let mut b = Request::builder().uri("/x");
            if let Some(x) = xff {
                b = b.header("x-forwarded-for", x);
            }
            let mut r = b.body(axum::body::Body::empty()).unwrap();
            r.extensions_mut()
                .insert(ConnectInfo("10.0.0.1:5555".parse::<SocketAddr>().unwrap()));
            r
        };
        // Without a trusted proxy the header is ignored: anyone can write one.
        assert_eq!(
            client_ip(&req(Some("9.9.9.9")), false),
            Some("10.0.0.1".parse().unwrap())
        );
        // Behind a proxy, the last entry is the address the proxy saw.
        assert_eq!(
            client_ip(&req(Some("6.6.6.6, 198.51.100.7")), true),
            Some("198.51.100.7".parse().unwrap()),
            "a forged first entry is ignored"
        );
        assert_eq!(client_ip(&req(None), true), Some("10.0.0.1".parse().unwrap()));
        assert_eq!(
            client_ip(&req(Some("not an ip")), true),
            Some("10.0.0.1".parse().unwrap())
        );
    }

    #[test]
    fn group_ids_are_shortened_for_logs() {
        assert_eq!(short(Some("0123456789abcdef0123456789abcdef")), "01234567…");
        assert_eq!(short(None), "-");
    }
}
