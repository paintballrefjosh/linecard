//! The relay's limits, metrics and probes over real HTTP.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use linecard_relay::{metrics_router, router, Limits, Rate, Relay};

const GROUP: &str = "0123456789abcdef0123456789abcdef";
const OTHER_GROUP: &str = "fedcba9876543210fedcba9876543210";
const TOKEN: &str = "token-for-the-group-xyz";

async fn serve(limits: Limits) -> (String, Arc<Relay>) {
    let relay = Arc::new(Relay::in_memory(limits).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(relay.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), relay)
}

fn limited(per_sec: f64, burst: f64) -> Limits {
    Limits {
        rate: Some(Rate { per_sec, burst }),
        ..Default::default()
    }
}

async fn get(base: &str, path: &str, token: &str, from: Option<&str>) -> reqwest::Response {
    let mut req = reqwest::Client::new().get(format!("{base}{path}")).bearer_auth(token);
    if let Some(ip) = from {
        req = req.header("x-forwarded-for", ip);
    }
    req.send().await.unwrap()
}

#[tokio::test]
async fn an_address_that_asks_too_often_is_told_when_to_come_back() {
    let (base, relay) = serve(limited(0.5, 3.0)).await;
    let path = format!("/v1/groups/{GROUP}/ops");
    for _ in 0..3 {
        assert_eq!(get(&base, &path, TOKEN, None).await.status(), 200);
    }
    let refused = get(&base, &path, TOKEN, None).await;
    assert_eq!(refused.status(), 429);
    let wait: u64 = refused.headers()["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=2).contains(&wait), "one token every 2 s: {wait}");
    assert!(refused.text().await.unwrap().contains("too many requests"));

    // Probes are never limited, so a busy relay still reads as healthy.
    for _ in 0..10 {
        assert_eq!(get(&base, "/health", "", None).await.status(), 200);
        assert_eq!(get(&base, "/ready", "", None).await.status(), 200);
    }
    let text = relay.metrics().render(&relay.stored().unwrap());
    assert!(
        text.contains("linecard_relay_rate_limited_total{kind=\"address\"} 1"),
        "{text}"
    );
}

#[tokio::test]
async fn a_group_has_its_own_allowance_across_addresses() {
    let limits = Limits {
        trust_proxy: true,
        ..limited(0.5, 3.0)
    };
    let (base, _) = serve(limits).await;
    let path = format!("/v1/groups/{GROUP}/ops");
    // Three different computers, one request each, then a fourth computer: the group is spent.
    for ip in ["198.51.100.1", "198.51.100.2", "198.51.100.3"] {
        assert_eq!(get(&base, &path, TOKEN, Some(ip)).await.status(), 200);
    }
    let r = get(&base, &path, TOKEN, Some("198.51.100.4")).await;
    assert_eq!(r.status(), 429);
    assert!(r.text().await.unwrap().contains("sync group"));
    // Another group, from a fresh address, is untouched.
    let other = format!("/v1/groups/{OTHER_GROUP}/ops");
    assert_eq!(get(&base, &other, TOKEN, Some("198.51.100.5")).await.status(), 200);
}

#[tokio::test]
async fn forged_forwarding_headers_are_ignored_unless_a_proxy_is_trusted() {
    let (base, _) = serve(limited(0.5, 2.0)).await; // trust_proxy off
    let path = format!("/v1/groups/{GROUP}/ops");
    // Varying the header doesn't buy a fresh allowance: they are all one address.
    assert_eq!(get(&base, &path, TOKEN, Some("1.1.1.1")).await.status(), 200);
    assert_eq!(get(&base, &path, TOKEN, Some("2.2.2.2")).await.status(), 200);
    assert_eq!(get(&base, &path, TOKEN, Some("3.3.3.3")).await.status(), 429);
}

#[tokio::test]
async fn an_address_that_keeps_sending_wrong_tokens_is_turned_away() {
    let limits = Limits {
        trust_proxy: true,
        ..Default::default()
    };
    let (base, relay) = serve(limits).await;
    let path = format!("/v1/groups/{GROUP}/ops");
    // The group is claimed by its owner; then someone guesses.
    let claim = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({ "device": "laptop", "blob": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(claim.status(), 200);

    let thief = "203.0.113.50";
    for i in 0..20 {
        let r = get(&base, &path, &format!("wrong-token-number-{i:04}"), Some(thief)).await;
        assert_eq!(r.status(), 401, "attempt {i}");
    }
    let blocked = get(&base, &path, TOKEN, Some(thief)).await; // even the right token is refused for now
    assert_eq!(blocked.status(), 429);
    let secs: u64 = blocked.headers()["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((200..=301).contains(&secs), "{secs}");
    // The owner, from their own address, carries on.
    assert_eq!(get(&base, &path, TOKEN, Some("198.51.100.77")).await.status(), 200);
    let text = relay.metrics().render(&relay.stored().unwrap());
    assert!(
        text.contains("linecard_relay_rate_limited_total{kind=\"blocked\"} 1"),
        "{text}"
    );
}

#[tokio::test]
async fn only_so_many_long_polls_can_be_held_at_once() {
    let (base, _) = serve(Limits::default()).await;
    // Hold the most one group allows, each waiting for a change that won't come.
    let path = format!("/v1/groups/{GROUP}/ops/wait?after=999999");
    let held: Vec<_> = (0..linecard_relay::guard::MAX_HELD)
        .map(|_| {
            let (base, path) = (base.clone(), path.clone());
            tokio::spawn(async move { get(&base, &path, TOKEN, None).await.status() })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let extra = get(&base, &path, TOKEN, None).await;
    assert_eq!(extra.status(), 429, "the 33rd is turned away");
    assert!(extra.headers().contains_key("retry-after"));
    // Other requests for the same group aren't affected.
    assert_eq!(
        get(&base, &format!("/v1/groups/{GROUP}/ops"), TOKEN, None)
            .await
            .status(),
        200
    );
    for h in held {
        h.abort();
    }
}

#[tokio::test]
async fn responses_name_their_epoch_and_the_probes_answer() {
    let (base, relay) = serve(Limits::default()).await;
    let r = get(&base, "/ready", "", None).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["x-linecard-epoch"], relay.epoch());
    assert_eq!(get(&base, "/health", "", None).await.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn metrics_count_requests_by_route_and_show_what_is_stored() {
    let (base, relay) = serve(Limits::default()).await;
    let path = format!("/v1/groups/{GROUP}/ops");
    let c = reqwest::Client::new();
    for blob in ["first", "second"] {
        c.post(format!("{base}{path}"))
            .bearer_auth(TOKEN)
            .json(&serde_json::json!({ "device": "laptop", "blob": blob }))
            .send()
            .await
            .unwrap();
    }
    get(&base, &path, TOKEN, None).await;
    get(&base, &path, "wrong-token-for-this-group", None).await;

    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let app = metrics_router(relay.clone());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let text = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let has = |line: &str| assert!(text.lines().any(|l| l == line), "missing {line:?} in:\n{text}");
    has("linecard_relay_requests_total{route=\"/v1/groups/{group}/ops\",status=\"200\"} 3");
    has("linecard_relay_requests_total{route=\"/v1/groups/{group}/ops\",status=\"401\"} 1");
    has("linecard_relay_groups 1");
    has("linecard_relay_batches 2");
    has("linecard_relay_stored_bytes 11"); // "first" + "second"
    assert!(
        !text.contains(TOKEN) && !text.contains(GROUP),
        "no token or group id in metrics"
    );
    // The public port doesn't serve metrics.
    assert_eq!(get(&base, "/metrics", "", None).await.status(), 404);
}

#[tokio::test]
async fn a_compaction_may_be_bigger_than_an_ordinary_request() {
    let (base, _) = serve(Limits::default()).await;
    let path = format!("/v1/groups/{GROUP}/ops");
    let c = reqwest::Client::new();
    // Ten batches of about 300 KiB each.
    for _ in 0..10 {
        let r = c
            .post(format!("{base}{path}"))
            .bearer_auth(TOKEN)
            .json(&serde_json::json!({ "device": "laptop", "blob": "a".repeat(300_000) }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    // A snapshot of three ~900 KB batches: over the 1 MiB request limit as a whole.
    let blobs: Vec<String> = (0..3).map(|_| "s".repeat(900_000)).collect();
    let r = c
        .post(format!("{base}/v1/groups/{GROUP}/compact"))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({ "device": "laptop", "up_to": 10, "blobs": blobs }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    let stats: serde_json::Value = c
        .get(format!("{base}/v1/groups/{GROUP}/stats"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["batches"], 3);
    assert_eq!(stats["head"], 13);
    // Ordinary pushes still obey the 1 MiB limit.
    let too_big = c
        .post(format!("{base}{path}"))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({ "device": "laptop", "blob": "a".repeat(2_000_000) }))
        .send()
        .await
        .unwrap();
    assert_eq!(too_big.status(), 413);
}
