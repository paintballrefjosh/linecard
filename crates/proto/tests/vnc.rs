//! The VNC client against a small RFB 3.8 server in this process.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use linecard_proto::gfx::{GfxEvent, GfxSession, GfxSink};
use linecard_proto::vnc::{vnc_auth_response, zrle, VncSession};
use linecard_proto::{Credentials, PromptReply, TerminalTarget};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

const W: u16 = 80;
const H: u16 = 70;
const PASSWORD: &str = "s3cret";
const CHALLENGE: [u8; 16] = *b"0123456789abcdef";

/// What the client sent after setup.
#[derive(Debug, PartialEq)]
enum Input {
    Pointer(u8, u16, u16),
    Key(bool, u32),
}

fn zlib(data: &[u8], z: &mut ZlibEncoder<Vec<u8>>) -> Vec<u8> {
    // One zlib stream for the whole connection, flushed per rectangle.
    z.write_all(data).unwrap();
    z.flush().unwrap();
    std::mem::take(z.get_mut())
}

/// ZRLE for an 80×70 rect: tiles (64×64, 16×64, 64×6, 16×6) using solid,
/// raw, packed-palette and plain-RLE encodings.
fn zrle_payload() -> Vec<u8> {
    let mut t = Vec::new();
    // Tile 1 (64x64): solid red.
    t.push(1);
    t.extend([255, 0, 0]);
    // Tile 2 (16x64): packed palette, 2 colors (1 bit), alternate columns.
    t.push(2);
    t.extend([0, 0, 0, 0, 255, 0]);
    for _ in 0..64 {
        t.extend([0b0101_0101, 0b0101_0101]);
    }
    // Tile 3 (64x6): plain RLE, all blue in one run of 384.
    t.push(128);
    t.extend([0, 0, 255]);
    t.extend([255, 128]); // 1 + 255 + 128 = 384
                          // Tile 4 (16x6): raw gray.
    t.push(0);
    for _ in 0..16 * 6 {
        t.extend([7, 7, 7]);
    }
    t
}

async fn server(inputs: UnboundedSender<Input>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        s.write_all(b"RFB 003.008\n").await.unwrap();
        let mut v = [0u8; 12];
        s.read_exact(&mut v).await.unwrap();
        assert_eq!(&v, b"RFB 003.008\n");
        s.write_all(&[1, 2]).await.unwrap(); // VNC auth only
        assert_eq!(s.read_u8().await.unwrap(), 2);
        s.write_all(&CHALLENGE).await.unwrap();
        let mut resp = [0u8; 16];
        s.read_exact(&mut resp).await.unwrap();
        if resp != vnc_auth_response(PASSWORD, &CHALLENGE) {
            s.write_u32(1).await.unwrap();
            let msg = b"Authentication failed";
            s.write_u32(msg.len() as u32).await.unwrap();
            s.write_all(msg).await.unwrap();
            return;
        }
        s.write_u32(0).await.unwrap();
        assert_eq!(s.read_u8().await.unwrap(), 1, "shared");
        s.write_u16(W).await.unwrap();
        s.write_u16(H).await.unwrap();
        s.write_all(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0])
            .await
            .unwrap();
        s.write_u32(4).await.unwrap();
        s.write_all(b"test").await.unwrap();

        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        let mut updates = 0;
        loop {
            let Ok(msg) = s.read_u8().await else { return };
            match msg {
                0 => {
                    let mut pf = [0u8; 19];
                    s.read_exact(&mut pf).await.unwrap();
                    assert_eq!(&pf[3..6], &[32, 24, 0], "32 bpp little endian");
                }
                2 => {
                    let _pad = s.read_u8().await.unwrap();
                    let n = s.read_u16().await.unwrap();
                    let mut e = vec![0u8; usize::from(n) * 4];
                    s.read_exact(&mut e).await.unwrap();
                }
                3 => {
                    let mut req = [0u8; 9];
                    s.read_exact(&mut req).await.unwrap();
                    updates += 1;
                    let mut out = vec![0u8, 0];
                    match updates {
                        // Full screen, Raw: green.
                        1 => {
                            out.extend(1u16.to_be_bytes());
                            out.extend([0, 0, 0, 0]);
                            out.extend(W.to_be_bytes());
                            out.extend(H.to_be_bytes());
                            out.extend(0i32.to_be_bytes());
                            for _ in 0..usize::from(W) * usize::from(H) {
                                out.extend([0, 200, 0, 0]);
                            }
                        }
                        // ZRLE over everything, then CopyRect of the top-left
                        // 2x2 (red) to (70, 60).
                        2 => {
                            out.extend(2u16.to_be_bytes());
                            out.extend([0, 0, 0, 0]);
                            out.extend(W.to_be_bytes());
                            out.extend(H.to_be_bytes());
                            out.extend(16i32.to_be_bytes());
                            let data = zlib(&zrle_payload(), &mut z);
                            out.extend((data.len() as u32).to_be_bytes());
                            out.extend(data);
                            out.extend(70u16.to_be_bytes());
                            out.extend(60u16.to_be_bytes());
                            out.extend(2u16.to_be_bytes());
                            out.extend(2u16.to_be_bytes());
                            out.extend(1i32.to_be_bytes());
                            out.extend([0, 0, 0, 0]);
                        }
                        _ => continue, // nothing new; don't answer
                    }
                    s.write_all(&out).await.unwrap();
                }
                4 => {
                    let down = s.read_u8().await.unwrap() == 1;
                    let _pad = s.read_u16().await.unwrap();
                    let k = s.read_u32().await.unwrap();
                    let _ = inputs.send(Input::Key(down, k));
                }
                5 => {
                    let b = s.read_u8().await.unwrap();
                    let (x, y) = (s.read_u16().await.unwrap(), s.read_u16().await.unwrap());
                    let _ = inputs.send(Input::Pointer(b, x, y));
                }
                other => panic!("unexpected client message {other}"),
            }
        }
    });
    port
}

#[derive(Debug, Default)]
struct Saved(Mutex<Option<(String, String)>>);

impl Credentials for Saved {
    fn password(&self, _key: &str) -> Option<String> {
        None
    }
    fn save_password(&self, key: &str, _label: &str, password: &str) {
        *self.0.lock().unwrap() = Some((key.into(), password.into()));
    }
}

fn sink() -> (GfxSink, UnboundedReceiver<GfxEvent>) {
    let (tx, rx) = unbounded_channel();
    (
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
        rx,
    )
}

async fn next(rx: &mut UnboundedReceiver<GfxEvent>) -> GfxEvent {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("timed out")
        .expect("events ended")
}

fn pixel(screen: &[u8], x: usize, y: usize) -> [u8; 4] {
    let o = (y * usize::from(W) + x) * 4;
    [screen[o], screen[o + 1], screen[o + 2], screen[o + 3]]
}

#[tokio::test]
async fn password_frames_and_input() {
    let (inputs_tx, mut inputs) = unbounded_channel();
    let port = server(inputs_tx).await;
    let (sink, mut events) = sink();
    let saved = Arc::new(Saved::default());
    let mut session = VncSession::connect(&TerminalTarget::new("127.0.0.1", port), Some(saved.clone()), sink).unwrap();

    let GfxEvent::Auth(prompt) = next(&mut events).await else {
        panic!("expected a password prompt")
    };
    assert_eq!(prompt.fields.len(), 1);
    session
        .reply(PromptReply::Fields {
            values: vec![PASSWORD.into()],
            remember: true,
        })
        .unwrap();
    assert_eq!(
        next(&mut events).await,
        GfxEvent::Connected {
            width: W,
            height: H,
            name: "test".into()
        }
    );
    assert_eq!(
        saved.0.lock().unwrap().as_ref().unwrap().1,
        PASSWORD,
        "saved after it worked"
    );

    // Paint frames onto our own copy of the screen, as the UI does.
    let mut screen = vec![0u8; usize::from(W) * usize::from(H) * 4];
    for _ in 0..3 {
        let GfxEvent::Frame {
            x,
            y,
            width,
            height,
            rgba,
        } = next(&mut events).await
        else {
            panic!("expected a frame")
        };
        for row in 0..usize::from(height) {
            let src = row * usize::from(width) * 4;
            let dst = ((usize::from(y) + row) * usize::from(W) + usize::from(x)) * 4;
            screen[dst..dst + usize::from(width) * 4].copy_from_slice(&rgba[src..src + usize::from(width) * 4]);
        }
    }
    assert_eq!(pixel(&screen, 0, 0), [255, 0, 0, 255], "solid tile");
    assert_eq!(pixel(&screen, 64, 0), [0, 0, 0, 255], "palette column 0");
    assert_eq!(pixel(&screen, 65, 0), [0, 255, 0, 255], "palette column 1");
    assert_eq!(pixel(&screen, 10, 66), [0, 0, 255, 255], "RLE tile");
    assert_eq!(pixel(&screen, 70, 66), [7, 7, 7, 255], "raw tile");
    assert_eq!(pixel(&screen, 71, 61), [255, 0, 0, 255], "copied from the top left");

    session.pointer(5, 6, 1).unwrap();
    session.wheel(5, 6, 0, 1).unwrap();
    session.key("KeyA", "a", true).unwrap();
    let mut got = Vec::new();
    while got.len() < 4 {
        got.push(
            tokio::time::timeout(Duration::from_secs(5), inputs.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(
        got,
        vec![
            Input::Pointer(1, 5, 6),
            Input::Pointer(1 | 16, 5, 6), // wheel down while holding left
            Input::Pointer(1, 5, 6),
            Input::Key(true, 0x61),
        ]
    );
    session.close().unwrap();
}

#[tokio::test]
async fn wrong_password_says_so() {
    let (inputs_tx, _inputs) = unbounded_channel();
    let port = server(inputs_tx).await;
    let (sink, mut events) = sink();
    let mut session = VncSession::connect(&TerminalTarget::new("127.0.0.1", port), None, sink).unwrap();
    assert!(matches!(next(&mut events).await, GfxEvent::Auth(_)));
    session
        .reply(PromptReply::Fields {
            values: vec!["nope".into()],
            remember: false,
        })
        .unwrap();
    match next(&mut events).await {
        GfxEvent::Disconnected { reason: Some(r) } => assert!(r.contains("wrong VNC password"), "{r}"),
        other => panic!("expected a disconnect, got {other:?}"),
    }
}

#[test]
fn zrle_rejects_truncated_data() {
    assert!(zrle(&[1, 255], 4, 4).is_err());
}
