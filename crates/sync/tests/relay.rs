//! The relay client against a real relay server on 127.0.0.1.

use std::sync::Arc;

use linecard_relay::{router, Limits, Relay};
use linecard_sync::crypto::{open, seal};
use linecard_sync::relay::RelayTransport;
use linecard_sync::{DeviceKey, GroupKey};

async fn start_relay() -> String {
    start_relay_with(Limits {
        max_ops: 10_000,
        open: true,
        ..Default::default()
    })
    .await
}

async fn start_relay_with(limits: Limits) -> String {
    let relay = Arc::new(Relay::in_memory(limits).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(relay)).await.unwrap() });
    format!("http://{addr}")
}

fn client(base: &str, group: &GroupKey, device: &str) -> RelayTransport {
    RelayTransport::new(base, &group.group_id(), &group.relay_token(), device).unwrap()
}

#[tokio::test]
async fn devices_exchange_batches_and_heartbeats() {
    let base = start_relay().await;
    let group = GroupKey::generate();
    let (ka, kb) = (DeviceKey::generate(), DeviceKey::generate());
    let a = client(&base, &group, "laptop");
    let b = client(&base, &group, "desktop");

    a.push(&seal(&group, &ka, "laptop", &1)).await.unwrap();
    a.push(&seal(&group, &ka, "laptop", &2)).await.unwrap();
    b.push(&seal(&group, &kb, "desktop", &3)).await.unwrap();

    let (got, cursor) = b.pull(0).await.unwrap();
    let nums: Vec<i32> = got.iter().map(|r| open(&group, &r.sealed).unwrap()).collect();
    assert_eq!(nums, vec![1, 2, 3], "arrival order, own pushes included");
    assert_eq!(cursor, 3);
    assert!(b.pull(cursor).await.unwrap().0.is_empty());

    a.put_device(&seal(&group, &ka, "laptop", &"hi")).await.unwrap();
    let devs = b.devices().await.unwrap();
    assert_eq!(devs.len(), 1);
    assert_eq!(open::<String>(&group, &devs[0].sealed).unwrap(), "hi");
}

#[tokio::test]
async fn another_phrase_cant_touch_the_group() {
    let base = start_relay().await;
    let group = GroupKey::generate();
    let k = DeviceKey::generate();
    client(&base, &group, "laptop")
        .push(&seal(&group, &k, "laptop", &1))
        .await
        .unwrap();

    // Same group id, wrong token (as if someone guessed the id).
    let intruder = RelayTransport::new(&base, &group.group_id(), "a-token-that-is-not-right", "evil").unwrap();
    let err = intruder.pull(0).await.unwrap_err().to_string();
    assert!(err.contains("refused"), "{err}");
}

#[tokio::test]
async fn unreachable_relay_and_bad_urls() {
    let group = GroupKey::generate();
    assert!(RelayTransport::new("relay.example.com", &group.group_id(), &group.relay_token(), "d").is_err());
    let dead = client("http://127.0.0.1:9", &group, "d");
    let err = dead.pull(0).await.unwrap_err().to_string();
    assert!(err.contains("can't reach"), "{err}");
}

#[tokio::test]
async fn wait_returns_as_soon_as_something_arrives() {
    let base = start_relay().await;
    let group = GroupKey::generate();
    let k = DeviceKey::generate();
    let a = client(&base, &group, "laptop");
    a.push(&seal(&group, &k, "laptop", &1)).await.unwrap();
    let b = client(&base, &group, "desktop");
    let started = std::time::Instant::now();
    let pusher = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        a.push(&seal(&group, &k, "laptop", &2)).await.unwrap();
    });
    assert_eq!(b.wait(1).await.unwrap(), 2);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    pusher.await.unwrap();
}

#[tokio::test]
async fn private_relays_take_an_access_key() {
    let relay = Arc::new(
        Relay::in_memory(Limits {
            max_ops: 100,
            open: false,
            ..Default::default()
        })
        .unwrap(),
    );
    let key = relay.add_account("alice", None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router(relay)).await.unwrap() });

    let group = GroupKey::generate();
    let k = DeviceKey::generate();
    let err = client(&base, &group, "laptop")
        .push(&seal(&group, &k, "laptop", &1))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("access key"), "{err}");
    client(&base, &group, "laptop")
        .with_account(Some(key))
        .push(&seal(&group, &k, "laptop", &1))
        .await
        .unwrap();
    // Joining needs only the phrase.
    client(&base, &group, "desktop")
        .push(&seal(&group, &k, "desktop", &2))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_relay_reports_which_history_it_is_serving() {
    let (one, two) = (start_relay().await, start_relay().await);
    let group = GroupKey::generate();
    let (a, b) = (client(&one, &group, "laptop"), client(&two, &group, "laptop"));
    let first = a.epoch().await.unwrap().expect("this relay reports an epoch");
    assert_eq!(
        a.epoch().await.unwrap().as_deref(),
        Some(first.as_str()),
        "stable while it runs"
    );
    assert_ne!(
        b.epoch().await.unwrap().unwrap(),
        first,
        "another database, another history"
    );
    // Authenticated responses carry it too.
    let resp = reqwest::get(format!("{one}/health")).await.unwrap();
    assert_eq!(resp.headers()["x-linecard-epoch"], first.as_str());
}

#[tokio::test]
async fn a_relay_that_says_slow_down_is_obeyed() {
    let base = start_relay_with(Limits {
        rate: Some(linecard_relay::Rate {
            per_sec: 0.2,
            burst: 2.0,
        }),
        ..Default::default()
    })
    .await;
    let group = GroupKey::generate();
    let a = client(&base, &group, "laptop");
    a.pull(0).await.unwrap();
    a.pull(0).await.unwrap();
    match a.pull(0).await.unwrap_err() {
        linecard_sync::SyncError::RateLimited(secs) => assert!((1..=5).contains(&secs), "{secs}"),
        other => panic!("expected the relay's rate limit, got {other:?}"),
    }
}

#[tokio::test]
async fn compaction_replaces_old_batches_for_every_computer() {
    let base = start_relay().await;
    let group = GroupKey::generate();
    let (ka, kb) = (DeviceKey::generate(), DeviceKey::generate());
    let a = client(&base, &group, "laptop");
    let b = client(&base, &group, "desktop");

    for i in 0..6 {
        a.push(&seal(
            &group,
            &ka,
            "laptop",
            &format!("old batch number {i} with some padding"),
        ))
        .await
        .unwrap();
    }
    let stats = a.stats().await.unwrap().expect("this relay can say");
    assert_eq!((stats.batches, stats.head), (6, 6));

    // The desktop is partway through (cursor 2) when the laptop compacts through 6.
    let (_, behind) = b.pull(0).await.map(|(g, _)| (g, 2)).unwrap();
    let removed = a.compact(6, &[seal(&group, &ka, "laptop", &"SNAPSHOT")]).await.unwrap();
    assert_eq!(removed, 6);
    b.push(&seal(&group, &kb, "desktop", &"after")).await.unwrap();

    let (got, cursor) = b.pull(behind).await.unwrap();
    let texts: Vec<String> = got.iter().map(|r| open(&group, &r.sealed).unwrap()).collect();
    assert_eq!(texts, ["SNAPSHOT", "after"]);
    assert_eq!(cursor, 8);
    let stats = a.stats().await.unwrap().unwrap();
    assert_eq!((stats.batches, stats.head), (2, 8));

    // A snapshot that isn't smaller, or reaches past the end, is refused.
    let big = seal(&group, &ka, "laptop", &"x".repeat(2000));
    assert!(a.compact(7, &[big]).await.is_err());
    assert!(a.compact(99, &[seal(&group, &ka, "laptop", &"s")]).await.is_err());
}
