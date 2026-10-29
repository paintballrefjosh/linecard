//! SSH port forwarding: local (`-L`), remote (`-R`) and dynamic SOCKS (`-D`).
//!
//! Local and dynamic forwards listen on this computer and open a
//! `direct-tcpip` channel per connection. Remote forwards ask the server to
//! listen (`tcpip-forward`); it opens a `forwarded-tcpip` channel per
//! connection, which [`RemoteForwards`] matches to a destination here.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use linecard_core_model::{ForwardKind, PortForward};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// How one forward is doing, for the tab's tunnel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardState {
    /// e.g. "L 127.0.0.1:8080 → 10.0.0.5:80"
    pub label: String,
    /// `None` while working; why it isn't otherwise.
    pub error: Option<String>,
}

/// Remote-forward destinations by the port the server listens on.
#[derive(Clone, Default)]
pub(crate) struct RemoteForwards(Arc<Mutex<HashMap<u32, (String, u16)>>>);

impl RemoteForwards {
    pub(crate) fn add(&self, server_port: u32, host: String, port: u16) {
        if let Ok(mut m) = self.0.lock() {
            m.insert(server_port, (host, port));
        }
    }

    pub(crate) fn get(&self, server_port: u32) -> Option<(String, u16)> {
        self.0.lock().ok()?.get(&server_port).cloned()
    }
}

/// Opens a channel to `host:port` through the SSH connection.
pub(crate) type Opener =
    Arc<dyn Fn(String, u16, String, u16) -> Pin<Box<dyn Future<Output = io::Result<Tunnel>> + Send>> + Send + Sync>;

/// Either side of a tunnel: an SSH channel stream.
pub(crate) type Tunnel = Box<dyn TunnelStream>;
pub(crate) trait TunnelStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> TunnelStream for T {}

/// Listen for a local or dynamic forward. Returns once listening (or with
/// the reason it can't); connections are served on spawned tasks until
/// `stop` fires.
pub(crate) async fn listen(fwd: PortForward, open: Opener, stop: tokio::sync::watch::Receiver<bool>) -> io::Result<()> {
    let listener = TcpListener::bind((fwd.bind(), fwd.bind_port)).await?;
    tokio::spawn(async move {
        let mut stop = stop;
        loop {
            let (sock, peer) = tokio::select! {
                r = listener.accept() => match r {
                    Ok(x) => x,
                    Err(_) => continue,
                },
                _ = stop.changed() => return,
            };
            let open = open.clone();
            let fwd = fwd.clone();
            tokio::spawn(async move {
                let _ = serve(sock, peer, &fwd, open).await;
            });
        }
    });
    Ok(())
}

async fn serve(mut sock: TcpStream, peer: std::net::SocketAddr, fwd: &PortForward, open: Opener) -> io::Result<()> {
    let _ = sock.set_nodelay(true);
    let origin = (peer.ip().to_string(), peer.port());
    let mut tunnel = match fwd.kind {
        ForwardKind::Dynamic => {
            let (host, port, socks) = socks_request(&mut sock).await?;
            match open(host, port, origin.0, origin.1).await {
                Ok(t) => {
                    socks.reply(&mut sock, true).await?;
                    t
                }
                Err(e) => {
                    socks.reply(&mut sock, false).await?;
                    return Err(e);
                }
            }
        }
        _ => open(fwd.host.clone(), fwd.port, origin.0, origin.1).await?,
    };
    tokio::io::copy_bidirectional(&mut sock, &mut tunnel).await?;
    Ok(())
}

/// A forwarded-tcpip channel from the server: connect it to its destination.
pub(crate) async fn deliver(mut tunnel: Tunnel, host: String, port: u16) -> io::Result<()> {
    let mut sock = TcpStream::connect((host.as_str(), port)).await?;
    let _ = sock.set_nodelay(true);
    tokio::io::copy_bidirectional(&mut sock, &mut tunnel).await?;
    Ok(())
}

// ---- SOCKS ----------------------------------------------------------------------

enum Socks {
    V4,
    V5,
}

impl Socks {
    async fn reply(&self, sock: &mut TcpStream, ok: bool) -> io::Result<()> {
        match self {
            Socks::V4 => {
                sock.write_all(&[0, if ok { 0x5a } else { 0x5b }, 0, 0, 0, 0, 0, 0])
                    .await
            }
            // Bound address 0.0.0.0:0: clients don't use it for CONNECT.
            Socks::V5 => {
                sock.write_all(&[5, if ok { 0 } else { 5 }, 0, 1, 0, 0, 0, 0, 0, 0])
                    .await
            }
        }
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Read a SOCKS4/4a/5 CONNECT request (no authentication): the destination
/// and how to answer.
async fn socks_request(sock: &mut TcpStream) -> io::Result<(String, u16, Socks)> {
    match sock.read_u8().await? {
        4 => {
            if sock.read_u8().await? != 1 {
                sock.write_all(&[0, 0x5b, 0, 0, 0, 0, 0, 0]).await?;
                return Err(bad("only SOCKS CONNECT is supported"));
            }
            let port = sock.read_u16().await?;
            let mut ip = [0u8; 4];
            sock.read_exact(&mut ip).await?;
            read_cstr(sock).await?; // user id
                                    // 4a: 0.0.0.x means a host name follows.
            let host = if ip[..3] == [0, 0, 0] && ip[3] != 0 {
                read_cstr(sock).await?
            } else {
                Ipv4Addr::from(ip).to_string()
            };
            Ok((host, port, Socks::V4))
        }
        5 => {
            let n = sock.read_u8().await?;
            let mut methods = vec![0u8; n.into()];
            sock.read_exact(&mut methods).await?;
            if !methods.contains(&0) {
                sock.write_all(&[5, 0xff]).await?;
                return Err(bad("the SOCKS client wants authentication"));
            }
            sock.write_all(&[5, 0]).await?;
            let mut head = [0u8; 4];
            sock.read_exact(&mut head).await?;
            if head[1] != 1 {
                sock.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                return Err(bad("only SOCKS CONNECT is supported"));
            }
            let host = match head[3] {
                1 => {
                    let mut ip = [0u8; 4];
                    sock.read_exact(&mut ip).await?;
                    Ipv4Addr::from(ip).to_string()
                }
                3 => {
                    let len = sock.read_u8().await?;
                    let mut name = vec![0u8; len.into()];
                    sock.read_exact(&mut name).await?;
                    String::from_utf8(name).map_err(|_| bad("bad host name"))?
                }
                4 => {
                    let mut ip = [0u8; 16];
                    sock.read_exact(&mut ip).await?;
                    Ipv6Addr::from(ip).to_string()
                }
                _ => return Err(bad("unknown SOCKS address type")),
            };
            let port = sock.read_u16().await?;
            Ok((host, port, Socks::V5))
        }
        _ => Err(bad("not a SOCKS request")),
    }
}

async fn read_cstr(sock: &mut TcpStream) -> io::Result<String> {
    let mut out = Vec::new();
    loop {
        match sock.read_u8().await? {
            0 => return String::from_utf8(out).map_err(|_| bad("bad SOCKS string")),
            b if out.len() < 255 => out.push(b),
            _ => return Err(bad("SOCKS string too long")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An "SSH server" that just connects to the destination directly.
    fn direct() -> Opener {
        Arc::new(|host, port, _, _| {
            Box::pin(async move { Ok(Box::new(TcpStream::connect((host.as_str(), port)).await?) as Tunnel) })
        })
    }

    async fn echo_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        port
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn roundtrip(sock: &mut TcpStream) {
        sock.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        sock.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn local_forward_carries_traffic() {
        let echo = echo_server().await;
        let (_stop_tx, stop) = tokio::sync::watch::channel(false);
        let bind_port = free_port();
        let fwd = PortForward {
            kind: ForwardKind::Local,
            bind_address: String::new(),
            bind_port,
            host: "127.0.0.1".into(),
            port: echo,
        };
        listen(fwd, direct(), stop).await.unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", bind_port)).await.unwrap();
        roundtrip(&mut c).await;
    }

    #[tokio::test]
    async fn socks5_and_socks4a_connect() {
        let echo = echo_server().await;
        let (_stop_tx, stop) = tokio::sync::watch::channel(false);
        let bind_port = free_port();
        let fwd = PortForward {
            kind: ForwardKind::Dynamic,
            bind_address: String::new(),
            bind_port,
            ..Default::default()
        };
        listen(fwd, direct(), stop).await.unwrap();

        // SOCKS5, host name
        let mut c = TcpStream::connect(("127.0.0.1", bind_port)).await.unwrap();
        c.write_all(&[5, 1, 0]).await.unwrap();
        let mut r = [0u8; 2];
        c.read_exact(&mut r).await.unwrap();
        assert_eq!(r, [5, 0]);
        let name = b"localhost";
        let mut req = vec![5, 1, 0, 3, name.len() as u8];
        req.extend_from_slice(name);
        req.extend_from_slice(&echo.to_be_bytes());
        c.write_all(&req).await.unwrap();
        let mut rep = [0u8; 10];
        c.read_exact(&mut rep).await.unwrap();
        assert_eq!(rep[1], 0, "SOCKS5 success");
        roundtrip(&mut c).await;

        // SOCKS4a, host name
        let mut c = TcpStream::connect(("127.0.0.1", bind_port)).await.unwrap();
        let mut req = vec![4, 1];
        req.extend_from_slice(&echo.to_be_bytes());
        req.extend_from_slice(&[0, 0, 0, 1, b'u', 0]);
        req.extend_from_slice(b"127.0.0.1\0");
        c.write_all(&req).await.unwrap();
        let mut rep = [0u8; 8];
        c.read_exact(&mut rep).await.unwrap();
        assert_eq!(rep[1], 0x5a, "SOCKS4 granted");
        roundtrip(&mut c).await;
    }

    #[tokio::test]
    async fn port_in_use_is_reported() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_stop_tx, stop) = tokio::sync::watch::channel(false);
        let fwd = PortForward {
            kind: ForwardKind::Local,
            bind_port: taken.local_addr().unwrap().port(),
            host: "127.0.0.1".into(),
            port: 1,
            ..Default::default()
        };
        assert!(listen(fwd, direct(), stop).await.is_err());
    }
}
