//! The VNC client against a small RFB 3.8 server in this process.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use linecard_proto::gfx::{CursorShape, GfxEvent, GfxSession, GfxSink};
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
    Clipboard(Vec<u8>),
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
                6 => {
                    let mut pad = [0u8; 3];
                    s.read_exact(&mut pad).await.unwrap();
                    let len = s.read_u32().await.unwrap();
                    let mut text = vec![0u8; len as usize];
                    s.read_exact(&mut text).await.unwrap();
                    let _ = inputs.send(Input::Clipboard(text));
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
    let mut session = VncSession::connect(
        &TerminalTarget::new("127.0.0.1", port),
        Some(saved.clone()),
        Default::default(),
        sink,
    )
    .unwrap();

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
    session.clipboard("caf\u{e9} \u{2603}").unwrap();
    let mut got = Vec::new();
    while got.len() < 5 {
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
            Input::Clipboard(b"caf\xe9 ?".to_vec()), // Latin-1; the snowman becomes ?
        ]
    );
    session.close().unwrap();
}

#[tokio::test]
async fn wrong_password_says_so() {
    let (inputs_tx, _inputs) = unbounded_channel();
    let port = server(inputs_tx).await;
    let (sink, mut events) = sink();
    let mut session =
        VncSession::connect(&TerminalTarget::new("127.0.0.1", port), None, Default::default(), sink).unwrap();
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

/// Apple Remote Desktop login (what macOS Screen Sharing uses) and a pointer
/// shape from the Cursor pseudo-encoding.
#[tokio::test]
async fn apple_remote_desktop_login_and_pointer_shape() {
    use aes::cipher::generic_array::GenericArray;
    use aes::cipher::{BlockDecrypt, KeyInit};
    use md5::{Digest, Md5};
    use num_bigint::BigUint;

    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(None::<(String, String)>));
    let seen_by_server = seen.clone();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        s.write_all(b"RFB 003.008\n").await.unwrap();
        let mut v = [0u8; 12];
        s.read_exact(&mut v).await.unwrap();
        s.write_all(&[1, 30]).await.unwrap(); // Apple Remote Desktop only
        assert_eq!(s.read_u8().await.unwrap(), 30);

        let prime: [u8; 16] = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x61,
        ];
        let p = BigUint::from_bytes_be(&prime);
        let b = BigUint::from(0x1234_5678_9abc_def1u64);
        let public = BigUint::from(2u32).modpow(&b, &p).to_bytes_be();
        let mut padded = vec![0u8; 16 - public.len()];
        padded.extend(&public);
        s.write_u16(2).await.unwrap();
        s.write_u16(16).await.unwrap();
        s.write_all(&prime).await.unwrap();
        s.write_all(&padded).await.unwrap();

        let mut sealed = [0u8; 128];
        s.read_exact(&mut sealed).await.unwrap();
        let mut client_public = [0u8; 16];
        s.read_exact(&mut client_public).await.unwrap();
        let shared = BigUint::from_bytes_be(&client_public).modpow(&b, &p).to_bytes_be();
        let mut shared_padded = vec![0u8; 16 - shared.len()];
        shared_padded.extend(&shared);
        let key = Md5::digest(&shared_padded);
        let cipher = aes::Aes128::new(GenericArray::from_slice(&key));
        for block in sealed.chunks_exact_mut(16) {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
        let text = |b: &[u8]| String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap()]).into_owned();
        *seen_by_server.lock().unwrap() = Some((text(&sealed[..64]), text(&sealed[64..])));
        s.write_u32(0).await.unwrap(); // SecurityResult: ok

        assert_eq!(s.read_u8().await.unwrap(), 1, "shared");
        s.write_u16(W).await.unwrap();
        s.write_u16(H).await.unwrap();
        s.write_all(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0])
            .await
            .unwrap();
        s.write_u32(4).await.unwrap();
        s.write_all(b"mac!").await.unwrap();
        loop {
            let Ok(msg) = s.read_u8().await else { return };
            match msg {
                0 => {
                    let mut pf = [0u8; 19];
                    s.read_exact(&mut pf).await.unwrap();
                }
                2 => {
                    let _pad = s.read_u8().await.unwrap();
                    let n = s.read_u16().await.unwrap();
                    let mut e = vec![0u8; usize::from(n) * 4];
                    s.read_exact(&mut e).await.unwrap();
                    assert!(
                        e.chunks_exact(4).any(|c| c == (-239i32).to_be_bytes()),
                        "asks for pointer shapes"
                    );
                }
                3 => {
                    let mut req = [0u8; 9];
                    s.read_exact(&mut req).await.unwrap();
                    // A 2x2 cursor, hotspot (1, 0): red, green / blue, white;
                    // the mask leaves the bottom-right pixel transparent.
                    let mut out = vec![0u8, 0, 0, 1];
                    out.extend(1u16.to_be_bytes()); // hotspot x
                    out.extend(0u16.to_be_bytes()); // hotspot y
                    out.extend(2u16.to_be_bytes());
                    out.extend(2u16.to_be_bytes());
                    out.extend((-239i32).to_be_bytes());
                    out.extend([255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0, 255, 255, 255, 0]);
                    out.extend([0b1100_0000, 0b1000_0000]);
                    s.write_all(&out).await.unwrap();
                }
                _ => return,
            }
        }
    });

    let (sink, mut events) = sink();
    let mut target = TerminalTarget::new("127.0.0.1", port);
    target.username = Some("admin".into());
    let mut session = VncSession::connect(&target, None, Default::default(), sink).unwrap();
    let GfxEvent::Auth(prompt) = next(&mut events).await else {
        panic!("expected a login prompt")
    };
    assert_eq!(
        prompt.fields.len(),
        1,
        "the username is known; only the password is asked"
    );
    session
        .reply(PromptReply::Fields {
            values: vec!["hunter2".into()],
            remember: false,
        })
        .unwrap();
    assert!(matches!(next(&mut events).await, GfxEvent::Connected { .. }));
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some(("admin".to_string(), "hunter2".to_string())),
        "the server decrypted the login"
    );
    assert_eq!(
        next(&mut events).await,
        GfxEvent::Cursor(CursorShape::Image {
            width: 2,
            height: 2,
            hot_x: 1,
            hot_y: 0,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 0],
        })
    );
    session.close().unwrap();
}
