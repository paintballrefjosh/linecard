//! LAN sync between transports in one process.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use linecard_relay::{router, Limits, Relay};
use linecard_sync::crypto::{open, seal};
use linecard_sync::lan::LanTransport;
use linecard_sync::relay::RelayTransport;
use linecard_sync::{DeviceKey, GroupKey};

async fn node(group: &GroupKey, device: &str, discover: bool) -> LanTransport {
    LanTransport::start(None, group, device, discover).await.unwrap()
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
    let stranger = node(&other, "x", false).await;
    stranger.add_peer("a", at(&a));
    assert!(stranger.pull(&Default::default()).await.unwrap().0.is_empty());

    // Knowing the group id but not a token gets nothing either.
    let url = format!("http://{}", at(&a));
    let guess = RelayTransport::new(&url, &group.group_id(), &other.lan_token(), "x").unwrap();
    assert!(guess.pull(0).await.is_err());
}

#[tokio::test]
async fn the_network_side_is_read_only_and_old_peers_can_still_read() {
    let group = GroupKey::generate();
    let k = DeviceKey::generate();
    let a = node(&group, "a", false).await;
    a.push(&seal(&group, &k, "a", &1)).unwrap();
    let url = format!("http://{}", at(&a));

    // A computer on an older Linecard presents the relay token: it may read.
    let old = RelayTransport::new(&url, &group.group_id(), &group.relay_token(), "old").unwrap();
    assert_eq!(old.pull(0).await.unwrap().0.len(), 1);

    // Nobody can write, whatever token they have.
    for token in [group.lan_token(), group.relay_token()] {
        let peer = RelayTransport::new(&url, &group.group_id(), &token, "w").unwrap();
        assert!(peer.push(&seal(&group, &k, "w", &2)).await.is_err());
        assert!(peer.put_device(&seal(&group, &k, "w", &"hi")).await.is_err());
    }
    assert_eq!(old.pull(0).await.unwrap().0.len(), 1, "still only the one item");
}

#[tokio::test]
async fn items_a_peer_serves_are_kept_only_if_they_open_with_the_group_key() {
    let group = GroupKey::generate();
    let other = GroupKey::generate();
    let k = DeviceKey::generate();

    // A fake peer (anyone who heard a beacon) serving one forged item and one real one.
    let fake = Arc::new(
        Relay::in_memory(Limits {
            open: true,
            ..Default::default()
        })
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_at = listener.local_addr().unwrap();
    tokio::spawn({
        let fake = fake.clone();
        async move { axum::serve(listener, router(fake)).await.unwrap() }
    });
    for (sealed, device) in [
        (seal(&other, &k, "laptop", &666), "laptop"),
        (seal(&group, &k, "a", &1), "a"),
    ] {
        let blob = serde_json::to_string(&sealed).unwrap();
        fake.push(&group.group_id(), &group.lan_token(), None, device, &blob)
            .unwrap();
    }

    let b = node(&group, "b", false).await;
    b.add_peer("fake", fake_at);
    let (got, _) = b.pull(&Default::default()).await.unwrap();
    assert_eq!(nums(&group, &got), vec![1], "the forged item is dropped");

    // ...and isn't passed on to the next computer.
    let c = node(&group, "c", false).await;
    c.add_peer("b", at(&b));
    let (got, _) = c.pull(&Default::default()).await.unwrap();
    assert_eq!(nums(&group, &got), vec![1]);
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
