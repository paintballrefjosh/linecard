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

/// Three computers share a library through a relay; one compacts it midway.
/// A computer that was behind, one that joins afterwards, and the compactor
/// itself all end up with the same library.
#[tokio::test]
async fn libraries_converge_across_a_compaction() {
    use linecard_core_sync::{Replica, Value};
    use linecard_sync::Batch;
    use std::collections::BTreeMap;

    let base = start_relay().await;
    let group = GroupKey::generate();
    let keys = [DeviceKey::generate(), DeviceKey::generate(), DeviceKey::generate()];
    let (ta, tb, tc) = (
        client(&base, &group, "a"),
        client(&base, &group, "b"),
        client(&base, &group, "c"),
    );
    let one = |k: &str, v: String| BTreeMap::from([(k.to_string(), Value::from(v))]);
    let state = |r: &Replica| -> Vec<(String, Vec<(String, String)>)> {
        r.all_objects()
            .map(|(id, o)| {
                (
                    id.clone(),
                    o.fields.iter().map(|(k, (v, _))| (k.clone(), v.to_string())).collect(),
                )
            })
            .collect()
    };
    // Pull what is new and apply it; returns the new cursor.
    async fn sync_in(t: &RelayTransport, g: &GroupKey, r: &mut Replica, me: &str, cursor: i64) -> i64 {
        let (got, next) = t.pull(cursor).await.unwrap();
        for item in got.into_iter().filter(|x| x.sealed.device != me) {
            for op in open::<Batch>(g, &item.sealed).unwrap().ops {
                r.apply_remote(op, 9_000);
            }
        }
        next
    }

    let (mut a, mut b) = (Replica::new("a"), Replica::new("b"));
    // A and B edit the same objects over many pushes.
    let mut ops_a = Vec::new();
    for i in 0..30 {
        ops_a.push(a.create(&format!("c{i}"), "connection", one("name", format!("sw{i}")), 100 + i));
    }
    ta.push(&seal(&group, &keys[0], "a", &Batch { ops: ops_a }))
        .await
        .unwrap();
    let mut cb = sync_in(&tb, &group, &mut b, "b", 0).await;
    for round in 0..20u64 {
        let op = b
            .update("c1", one("host", format!("10.0.0.{round}")), 1_000 + round)
            .unwrap();
        tb.push(&seal(&group, &keys[1], "b", &Batch { ops: vec![op] }))
            .await
            .unwrap();
        let op = a
            .update("c1", one("name", format!("core{round}")), 2_000 + round)
            .unwrap();
        ta.push(&seal(&group, &keys[0], "a", &Batch { ops: vec![op] }))
            .await
            .unwrap();
    }
    // B has only taken in the first 5 batches when A (fully caught up) compacts.
    let mut ca = sync_in(&ta, &group, &mut a, "a", 0).await;
    let head = ca;
    let before = ta.stats().await.unwrap().unwrap();
    assert_eq!(before.batches, head);
    let snapshot: Vec<_> = a
        .snapshot()
        .chunks(400)
        .map(|c| seal(&group, &keys[0], "a", &Batch { ops: c.to_vec() }))
        .collect();
    let removed = ta.compact(head, &snapshot).await.unwrap();
    assert_eq!(removed, head);
    let after = ta.stats().await.unwrap().unwrap();
    assert_eq!(after.batches, 1);
    assert!(
        after.bytes < before.bytes / 2,
        "{} -> {} bytes",
        before.bytes,
        after.bytes
    );

    // B was behind: takes the snapshot (ops it has seen are skipped, the rest merge).
    cb = cb.min(5);
    sync_in(&tb, &group, &mut b, "b", cb).await;
    // C joins after the compaction with nothing.
    let mut c = Replica::new("c");
    let cc = sync_in(&tc, &group, &mut c, "c", 0).await;
    // A edit made after compaction still flows to everyone.
    let op = a.update("c2", one("name", "renamed".into()), 5_000).unwrap();
    ta.push(&seal(&group, &keys[0], "a", &Batch { ops: vec![op] }))
        .await
        .unwrap();
    ca = sync_in(&ta, &group, &mut a, "a", ca).await;
    sync_in(&tb, &group, &mut b, "b", tb.stats().await.unwrap().unwrap().head - 1).await;
    sync_in(&tc, &group, &mut c, "c", cc).await;

    assert_eq!(state(&b), state(&a), "the computer that was behind");
    assert_eq!(state(&c), state(&a), "the computer that joined afterwards");
    assert_eq!(ca, ta.stats().await.unwrap().unwrap().head);
    assert_eq!(a.object("c2").unwrap().get("name").unwrap().to_string(), "renamed");
    let _ = &keys[2];
}

#[tokio::test]
async fn waiting_uses_the_websocket_and_keeps_it_between_waits() {
    let base = start_relay().await;
    let group = GroupKey::generate();
    let (ka, kb) = (DeviceKey::generate(), DeviceKey::generate());
    let a = client(&base, &group, "laptop");
    let b = Arc::new(client(&base, &group, "desktop"));

    // Nothing there yet: the first wait ends by itself, not because of an error.
    a.push(&seal(&group, &ka, "laptop", &1)).await.unwrap();
    let mut cursor = b.wait(0).await.unwrap();
    assert_eq!(cursor, 1, "the socket reports what is already there at once");

    // Pushes from the other computer wake it within moments, wait after wait.
    for n in 2..5 {
        let waiter = tokio::spawn({
            let b = b.clone();
            async move { b.wait(cursor).await.unwrap() }
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let started = std::time::Instant::now();
        a.push(&seal(&group, &ka, "laptop", &n)).await.unwrap();
        cursor = tokio::time::timeout(std::time::Duration::from_secs(3), waiter)
            .await
            .expect("woken by the push")
            .unwrap();
        assert_eq!(cursor, n);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
    let _ = kb;
}

#[tokio::test]
async fn a_relay_without_websockets_is_waited_on_by_long_poll() {
    use axum::routing::get;
    // An older relay: only the long-poll exists (and /ws is a 404).
    let app = axum::Router::new().route(
        "/v1/groups/{group}/ops/wait",
        get(|| async { axum::Json(serde_json::json!({ "latest": 7 })) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let group = GroupKey::generate();
    let c = client(&base, &group, "laptop");
    assert_eq!(c.wait(0).await.unwrap(), 7);
    // And it doesn't try the socket again on every call.
    assert_eq!(c.wait(0).await.unwrap(), 7);
}
