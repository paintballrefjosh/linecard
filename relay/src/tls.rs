//! Optional HTTPS straight from the relay, for setups without a reverse
//! proxy. Give it a certificate chain and key (PEM files, e.g. from
//! certbot); it picks up renewed files without a restart.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rustls::crypto::ring;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

/// How often the certificate files are looked at (on a connection).
const RECHECK: Duration = Duration::from_secs(60);
/// A client has this long to finish its handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);

fn bad(what: impl fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn load(cert: &Path, key: &Path) -> io::Result<Arc<CertifiedKey>> {
    let chain = CertificateDer::pem_file_iter(cert)
        .map_err(|e| bad(format!("{}: {e}", cert.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| bad(format!("{}: {e}", cert.display())))?;
    if chain.is_empty() {
        return Err(bad(format!("{} holds no certificate", cert.display())));
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| bad(format!("{}: {e}", key.display())))?;
    let signing = ring::sign::any_supported_type(&key).map_err(|e| bad(format!("private key: {e}")))?;
    Ok(Arc::new(CertifiedKey::new(chain, signing)))
}

fn stamp(cert: &Path, key: &Path) -> (Option<SystemTime>, Option<SystemTime>) {
    let m = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    (m(cert), m(key))
}

struct Current {
    key: Arc<CertifiedKey>,
    stamp: (Option<SystemTime>, Option<SystemTime>),
    checked: Instant,
}

/// The certificate in use; re-read when the files change.
pub struct Reloading {
    cert: PathBuf,
    key: PathBuf,
    current: Mutex<Current>,
}

impl fmt::Debug for Reloading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Reloading({})", self.cert.display())
    }
}

impl Reloading {
    /// Fails if the files can't be read: better at startup than at the first client.
    pub fn new(cert: &Path, key: &Path) -> io::Result<Arc<Reloading>> {
        Ok(Arc::new(Reloading {
            cert: cert.into(),
            key: key.into(),
            current: Mutex::new(Current {
                key: load(cert, key)?,
                stamp: stamp(cert, key),
                checked: Instant::now(),
            }),
        }))
    }

    fn refresh(&self, every: Duration) -> Arc<CertifiedKey> {
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if cur.checked.elapsed() >= every {
            cur.checked = Instant::now();
            let now = stamp(&self.cert, &self.key);
            if now != cur.stamp {
                // Remember the attempt either way, so a bad file warns once, not per connection.
                cur.stamp = now;
                match load(&self.cert, &self.key) {
                    Ok(k) => {
                        cur.key = k;
                        tracing::info!("TLS certificate reloaded");
                    }
                    Err(e) => tracing::warn!("TLS certificate not reloaded, keeping the old one: {e}"),
                }
            }
        }
        cur.key.clone()
    }
}

impl ResolvesServerCert for Reloading {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.refresh(RECHECK))
    }
}

/// A TLS acceptor serving the certificate in `files`, checking for new ones every `every`.
pub fn acceptor(cert: &Path, key: &Path) -> io::Result<TlsAcceptor> {
    let resolver = Reloading::new(cert, key)?;
    let mut config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(bad)?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    // axum is built without HTTP/2 here; offering it would make browsers and curl try it and fail.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// What `axum::serve` listens on: TCP connections, each taken through its
/// TLS handshake first (in its own task, so a slow client holds up no one).
pub struct TlsListener {
    ready: mpsc::Receiver<(TlsStream<TcpStream>, Peer)>,
    local: SocketAddr,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, acceptor: TlsAcceptor) -> io::Result<TlsListener> {
        let local = tcp.local_addr()?;
        let (tx, ready) = mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // The server is gone: stop accepting.
                    () = tx.closed() => break,
                    accepted = tcp.accept() => match accepted {
                        Ok((stream, peer)) => {
                            let (acceptor, tx) = (acceptor.clone(), tx.clone());
                            tokio::spawn(async move {
                                if let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE, acceptor.accept(stream)).await {
                                    let _ = tx.send((tls, Peer(peer))).await;
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!("accept failed: {e}");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    },
                }
            }
        });
        Ok(TlsListener { ready, local })
    }
}

/// The client's address on a TLS connection (axum's own address-reading
/// support covers plain TCP only, so this stands in for `SocketAddr`).
#[derive(Debug, Clone, Copy)]
pub struct Peer(pub SocketAddr);

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TlsListener>> for Peer {
    fn connect_info(stream: axum::serve::IncomingStream<'_, TlsListener>) -> Self {
        *stream.remote_addr()
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = Peer;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(x) => x,
            // The accept task ended (it only does when we are dropped).
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(Peer(self.local))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn write_cert(dir: &Path, name: &str) -> Vec<u8> {
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        std::fs::write(dir.join("cert.pem"), ck.cert.pem()).unwrap();
        std::fs::write(dir.join("key.pem"), ck.signing_key.serialize_pem()).unwrap();
        let _ = name;
        ck.cert.der().to_vec()
    }

    /// GET /health over TLS, trusting only `trusted`; returns the response and the certificate seen.
    async fn get(addr: SocketAddr, trusted: &[u8]) -> io::Result<(String, Vec<u8>)> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(trusted.to_vec())).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut config = config;
        // Like curl and browsers: ask for HTTP/2 first.
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let tcp = TcpStream::connect(addr).await?;
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await?;
        let seen = tls.get_ref().1.peer_certificates().unwrap()[0].to_vec();
        assert_eq!(
            tls.get_ref().1.alpn_protocol(),
            Some(&b"http/1.1"[..]),
            "HTTP/2 must not be agreed"
        );
        tls.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut out = String::new();
        tls.read_to_string(&mut out).await.ok();
        Ok((out, seen))
    }

    async fn serve(acceptor: TlsAcceptor) -> SocketAddr {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let relay = Arc::new(crate::Relay::in_memory(crate::Limits::default()).unwrap());
        let app = crate::router(relay).into_make_service_with_connect_info::<Peer>();
        let listener = TlsListener::new(tcp, acceptor).unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    #[tokio::test]
    async fn serves_https_with_the_given_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let der = write_cert(dir.path(), "a");
        let addr = serve(acceptor(&dir.path().join("cert.pem"), &dir.path().join("key.pem")).unwrap()).await;
        let (response, seen) = get(addr, &der).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("ok"), "{response}");
        assert_eq!(seen, der);

        // A client that doesn't trust the certificate is refused.
        let other = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        assert!(get(addr, other.cert.der()).await.is_err());
        // Plain HTTP gets nothing useful.
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let mut buf = String::new();
        let _ = tcp.read_to_string(&mut buf).await;
        assert!(!buf.contains("200 OK"), "{buf}");
    }

    #[test]
    fn a_renewed_certificate_is_picked_up_and_a_broken_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        let first = write_cert(dir.path(), "a");
        let r = Reloading::new(&cert, &key).unwrap();
        assert_eq!(r.refresh(Duration::ZERO).cert[0].as_ref(), first);

        std::thread::sleep(Duration::from_millis(30)); // distinct modification times
        let second = write_cert(dir.path(), "b");
        assert_ne!(first, second);
        assert_eq!(r.refresh(Duration::ZERO).cert[0].as_ref(), second);

        // Half-written files (mid-renewal) keep the working certificate.
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(&cert, "not a certificate").unwrap();
        assert_eq!(r.refresh(Duration::ZERO).cert[0].as_ref(), second);
        // Not looked at again until the interval has passed.
        assert_eq!(r.refresh(Duration::from_secs(3600)).cert[0].as_ref(), second);
    }

    #[test]
    fn missing_or_wrong_files_fail_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        assert!(acceptor(&cert, &key).is_err(), "no files");
        write_cert(dir.path(), "a");
        std::fs::write(&key, "nope").unwrap();
        assert!(acceptor(&cert, &key).is_err(), "bad key");
    }
}
