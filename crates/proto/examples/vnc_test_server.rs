//! A throwaway VNC server for trying Linecard's VNC tab:
//!
//! ```text
//! cargo run -p linecard-proto --example vnc_test_server [port]
//! ```
//!
//! No password. Draws a moving gradient, a square that follows the mouse,
//! and prints the keys and clicks it receives.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const W: u16 = 640;
const H: u16 = 400;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(5900);
    let l = TcpListener::bind(("127.0.0.1", port)).await.expect("bind");
    println!("VNC test server on 127.0.0.1:{port} (no password)");
    while let Ok((s, peer)) = l.accept().await {
        println!("{peer} connected");
        tokio::spawn(async move {
            if let Err(e) = serve(s).await {
                println!("{peer}: {e}");
            }
        });
    }
}

async fn serve(mut s: TcpStream) -> std::io::Result<()> {
    s.write_all(b"RFB 003.008\n").await?;
    let mut v = [0u8; 12];
    s.read_exact(&mut v).await?;
    s.write_all(&[1, 1]).await?; // security: None
    s.read_u8().await?;
    s.write_u32(0).await?;
    s.read_u8().await?; // ClientInit
    s.write_u16(W).await?;
    s.write_u16(H).await?;
    s.write_all(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0]).await?;
    let name = b"Linecard VNC test";
    s.write_u32(name.len() as u32).await?;
    s.write_all(name).await?;

    let (mut mx, mut my) = (W / 2, H / 2);
    let mut tick: u32 = 0;
    loop {
        match s.read_u8().await? {
            0 => {
                let mut b = [0u8; 19];
                s.read_exact(&mut b).await?;
            }
            2 => {
                s.read_u8().await?;
                let n = s.read_u16().await?;
                let mut e = vec![0u8; usize::from(n) * 4];
                s.read_exact(&mut e).await?;
            }
            3 => {
                let mut req = [0u8; 9];
                s.read_exact(&mut req).await?;
                if req[0] == 1 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                tick = tick.wrapping_add(1);
                let mut out = vec![0u8, 0, 0, 1, 0, 0, 0, 0];
                out.extend(W.to_be_bytes());
                out.extend(H.to_be_bytes());
                out.extend(0i32.to_be_bytes());
                for y in 0..H {
                    for x in 0..W {
                        let near = x.abs_diff(mx) < 12 && y.abs_diff(my) < 12;
                        let px = if near {
                            [255, 255, 255, 0]
                        } else {
                            let t = tick as u16;
                            [((x + t * 2) % 256) as u8, ((y + t) % 256) as u8, 120, 0]
                        };
                        out.extend(px);
                    }
                }
                s.write_all(&out).await?;
            }
            4 => {
                let down = s.read_u8().await? == 1;
                s.read_u16().await?;
                let k = s.read_u32().await?;
                if down {
                    println!("key {k:#x}");
                }
            }
            5 => {
                let b = s.read_u8().await?;
                mx = s.read_u16().await?;
                my = s.read_u16().await?;
                if b != 0 {
                    println!("buttons {b} at {mx},{my}");
                }
            }
            6 => {
                let mut pad = [0u8; 3];
                s.read_exact(&mut pad).await?;
                let n = s.read_u32().await?;
                let mut t = vec![0u8; n as usize];
                s.read_exact(&mut t).await?;
            }
            m => return Err(std::io::Error::other(format!("unexpected message {m}"))),
        }
    }
}
