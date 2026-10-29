//! LAN sync between transports in one process.

use std::net::SocketAddr;
use std::time::Duration;

use linecard_sync::crypto::{open, seal};
use linecard_sync::lan::LanTransport;
use linecard_sync::{DeviceKey, GroupKey};

async fn node(group: &GroupKey, device: &str, discover: bool) -> LanTransport {
    LanTransport::start(None, &group.group_id(), &group.relay_token(), device, discover)
        .await
        .unwrap()
}

fn at(t: &LanTransport) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], t.port()))
}

fn nums(group: &GroupKey, got: &[linecard_sync::Received]) -> Vec<i32> {
    let mut v: Vec<i32> = got.iter().map(|r| open(group, &r.sealed).unwrap()).collect();
    v.sort();
    v
}

#[tokio::test]
async fn peers_exchange_and_forward_changes() {
    let group = GroupKey::generate();
    let (ka, kc) = (DeviceKey::generate(), DeviceKey::generate());
    let a = node(&group, "a", false).await;
    let b = node(&group, "b", false).await;
    let c = node(&group, "c", false).await;

    a.push(&seal(&group, &ka, "a", &1)).unwrap();
    a.push(&seal(&group, &ka, "a", &2)).unwrap();

    // B hears A; C only ever hears B.
    b.add_peer("a", at(&a));
    c.add_peer("b", at(&b));

    let (got, cur_b) = b.pull(&Default::default()).await.unwrap();
    assert_eq!(nums(&group, &got), vec![1, 2]);
    assert!(b.pull(&cur_b).await.unwrap().0.is_empty(), "nothing new");

    // A is gone now; C still gets A's changes through B.
    drop(a);
    let (got, cur_c) = c.pull(&Default::default()).await.unwrap();
    assert_eq!(nums(&group, &got), vec![1, 2], "forwarded by B");

    // C's own change travels back to B, once.
    c.push(&seal(&group, &kc, "c", &3)).unwrap();
    b.add_peer("c", at(&c));
    let (got, cur_b) = b.pull(&cur_b).await.unwrap();
    assert_eq!(nums(&group, &got), vec![3]);
    assert!(b.pull(&cur_b).await.unwrap().0.is_empty());
    // C doesn't get its own change back as news.
    assert!(c.pull(&cur_c).await.unwrap().0.is_empty());
}

#[tokio::test]
async fn other_groups_cant_read() {
    let group = GroupKey::generate();
    let other = GroupKey::generate();
    let k = DeviceKey::generate();
    let a = node(&group, "a", false).await;
    a.push(&seal(&group, &k, "a", &1)).unwrap();
    let stranger = LanTransport::start(None, &group.group_id(), &other.relay_token(), "x", false)
        .await
        .unwrap();
    stranger.add_peer("a", at(&a));
    assert!(stranger.pull(&Default::default()).await.unwrap().0.is_empty());
}

#[tokio::test]
async fn heartbeats_from_peers() {
    let group = GroupKey::generate();
    let k = DeviceKey::generate();
    let a = node(&group, "a", false).await;
    let b = node(&group, "b", false).await;
    a.put_device(&seal(&group, &k, "a", &"hello")).unwrap();
    b.add_peer("a", at(&a));
    let devs = b.devices().await.unwrap();
    assert_eq!(devs.len(), 1);
    assert_eq!(open::<String>(&group, &devs[0].sealed).unwrap(), "hello");
}

/// Uses real multicast, which some CI machines don't allow:
/// `cargo test -p linecard-sync --test lan -- --ignored`
#[tokio::test]
#[ignore]
async fn computers_find_each_other() {
    let group = GroupKey::generate();
    let k = DeviceKey::generate();
    let a = node(&group, "a", true).await;
    let b = node(&group, "b", true).await;
    a.push(&seal(&group, &k, "a", &7)).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while b.peers().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no beacon heard");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let (got, _) = b.pull(&Default::default()).await.unwrap();
    assert_eq!(nums(&group, &got), vec![7]);
}
