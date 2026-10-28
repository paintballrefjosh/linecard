//! The relay client against a real relay server on 127.0.0.1.

use std::sync::Arc;

use linecard_relay::{router, Relay};
use linecard_sync::crypto::{open, seal};
use linecard_sync::relay::RelayTransport;
use linecard_sync::{DeviceKey, GroupKey};

async fn start_relay() -> String {
    let relay = Arc::new(Relay::in_memory(10_000).unwrap());
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
