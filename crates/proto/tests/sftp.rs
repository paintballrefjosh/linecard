//! The SFTP client against an in-process SSH server whose SFTP subsystem
//! serves a real temporary folder.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use linecard_proto::sftp::{join, EntryKind, SftpClient, Transfer};
use linecard_proto::{EventSink, HostKeyDecision, PromptReply, SessionEvent, SshOptions, TerminalTarget};
use russh::keys::PrivateKey;
use russh::server::{self, Auth, Msg};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::unbounded_channel;

/// Throwaway host key generated for the SSH tests only.
const HOST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDtwJ0rd6G1qUoAaxjzytAz1RP/966yrWAwGQZtmZ0IegAAAJguX39ILl9/
SAAAAAtzc2gtZWQyNTUxOQAAACDtwJ0rd6G1qUoAaxjzytAz1RP/966yrWAwGQZtmZ0Ieg
AAAEA8hph6w7PFXOL7C8KMMrkeSeLXCzA6XIPUsjOyk0gQk+3AnSt3obWpSgBrGPPK0DPV
E//3rrKtYDAZBm2ZnQh6AAAAEmxpbmVjYXJkLXRlc3QtaG9zdAECAw==
-----END OPENSSH PRIVATE KEY-----
";

// ---- test server -----------------------------------------------------------------

/// (path, mode) of every setstat the test server received.
static CHMODS: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

/// What an open SFTP handle refers to.
enum Open {
    Dir(Option<Vec<File>>),
    File(tokio::fs::File),
}

/// The SFTP side: paths are relative to `root`, which stands in for "/".
struct Files {
    root: PathBuf,
    handles: HashMap<String, Open>,
    next: u32,
}

impl Files {
    fn real(&self, path: &str) -> PathBuf {
        let mut p = self.root.clone();
        for part in path.split('/').filter(|s| !s.is_empty() && *s != ".") {
            if part == ".." {
                p.pop();
            } else {
                p.push(part);
            }
        }
        p
    }

    fn handle(&mut self, id: u32, open: Open) -> Handle {
        self.next += 1;
        let handle = format!("h{}", self.next);
        self.handles.insert(handle.clone(), open);
        Handle { id, handle }
    }
}

fn attrs(m: &std::fs::Metadata) -> FileAttributes {
    let mode = if m.is_dir() { 0o040755 } else { 0o100644 };
    FileAttributes {
        size: Some(m.len()),
        permissions: Some(mode),
        mtime: m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32),
        ..Default::default()
    }
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

fn io(e: std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _v: u32, _e: HashMap<String, String>) -> Result<Version, StatusCode> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        // "/" is the root; a relative path is under it.
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split('/').filter(|s| !s.is_empty() && *s != ".") {
            if part == ".." {
                parts.pop();
            } else {
                parts.push(part);
            }
        }
        Ok(Name {
            id,
            files: vec![File::dummy(format!("/{}", parts.join("/")))],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        let mut files = Vec::new();
        for e in std::fs::read_dir(self.real(&path)).map_err(io)? {
            let e = e.map_err(io)?;
            let name = e.file_name().to_string_lossy().into_owned();
            files.push(File {
                longname: name.clone(),
                filename: name,
                attrs: attrs(&e.metadata().map_err(io)?),
            });
        }
        Ok(self.handle(id, Open::Dir(Some(files))))
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir(files)) => match files.take() {
                Some(files) => Ok(Name { id, files }),
                None => Err(StatusCode::Eof),
            },
            _ => Err(StatusCode::Failure),
        }
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let m = std::fs::metadata(self.real(&path)).map_err(io)?;
        Ok(Attrs { id, attrs: attrs(&m) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat(id, path).await
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        _a: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        let mut opts = tokio::fs::OpenOptions::new();
        opts.read(flags.contains(OpenFlags::READ) || !flags.contains(OpenFlags::WRITE))
            .write(flags.contains(OpenFlags::WRITE))
            .create(flags.contains(OpenFlags::CREATE))
            .truncate(flags.contains(OpenFlags::TRUNCATE));
        let file = opts.open(self.real(&filename)).await.map_err(io)?;
        Ok(self.handle(id, Open::File(file)))
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, StatusCode> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        f.seek(std::io::SeekFrom::Start(offset)).await.map_err(io)?;
        let mut data = vec![0u8; len as usize];
        let n = f.read(&mut data).await.map_err(io)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(n);
        Ok(Data { id, data })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, StatusCode> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        f.seek(std::io::SeekFrom::Start(offset)).await.map_err(io)?;
        f.write_all(&data).await.map_err(io)?;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        if let Some(Open::File(mut f)) = self.handles.remove(&handle) {
            f.flush().await.map_err(io)?;
        }
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _a: FileAttributes) -> Result<Status, StatusCode> {
        std::fs::create_dir(self.real(&path)).map_err(io)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        std::fs::remove_dir(self.real(&path)).map_err(io)?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        std::fs::remove_file(self.real(&path)).map_err(io)?;
        Ok(ok(id))
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, StatusCode> {
        if !self.real(&path).exists() {
            return Err(StatusCode::NoSuchFile);
        }
        // The files live on the test machine's own filesystem, whose permission
        // model varies; record what was asked instead.
        if let Some(mode) = attrs.permissions {
            CHMODS.lock().unwrap().push((path, mode));
        }
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, StatusCode> {
        std::fs::rename(self.real(&from), self.real(&to)).map_err(io)?;
        Ok(ok(id))
    }
}

/// The SSH side: password login, then hand "sftp" channels to `Files`.
struct Ssh {
    root: PathBuf,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl server::Handler for Ssh {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "admin" && password == "secret" {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::Password][..])),
                partial_success: false,
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let Some(ch) = self.channels.remove(&channel) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        let files = Files {
            root: self.root.clone(),
            handles: HashMap::new(),
            next: 0,
        };
        russh_sftp::server::run(ch.into_stream(), files).await;
        Ok(())
    }
}

async fn start_server(root: PathBuf) -> u16 {
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::from_openssh(HOST_KEY).unwrap()],
        methods: MethodSet::from(&[MethodKind::Password][..]),
        auth_rejection_time: Duration::from_millis(5),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (config, root) = (config.clone(), root.clone());
            tokio::spawn(async move {
                let handler = Ssh {
                    root,
                    channels: HashMap::new(),
                };
                if let Ok(running) = server::run_stream(config, stream, handler).await {
                    let _ = running.await;
                }
            });
        }
    });
    port
}

// ---- helpers -----------------------------------------------------------------

fn temp_dir(label: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "linecard-sftp-{label}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Log in, answering the host-key and password prompts the way a user would.
async fn connect(port: u16, password: &str) -> std::io::Result<SftpClient> {
    let opts = SshOptions {
        credentials: None,
        known_hosts: temp_dir("hosts").join("known_hosts"),
        extra_known_hosts: vec![],
        identity_files: vec![],
        use_agent: false,
    };
    let mut target = TerminalTarget::new("127.0.0.1", port);
    target.username = Some("admin".into());
    let (reply_tx, reply_rx) = unbounded_channel();
    let password = password.to_string();
    let sink: EventSink = Box::new(move |e| match e {
        SessionEvent::HostKey(_) => {
            let _ = reply_tx.send(PromptReply::HostKey(HostKeyDecision::AcceptOnce));
        }
        SessionEvent::Auth(_) => {
            let _ = reply_tx.send(PromptReply::Fields {
                values: vec![password.clone()],
                remember: false,
            });
        }
        _ => {}
    });
    let replies = Arc::new(tokio::sync::Mutex::new(reply_rx));
    tokio::time::timeout(
        Duration::from_secs(15),
        SftpClient::connect(&target, &opts, sink, replies),
    )
    .await
    .expect("timed out connecting")
}

fn silent() -> impl FnMut(u64) + Send {
    |_| {}
}

// ---- tests -------------------------------------------------------------------

#[tokio::test]
async fn browse_download_upload_rename_and_delete() {
    let root = temp_dir("root");
    std::fs::create_dir(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/notes.txt"), "remote notes").unwrap();
    std::fs::write(root.join("readme.md"), vec![b'x'; 100_000]).unwrap(); // several chunks
    let port = start_server(root.clone()).await;
    let sftp = connect(port, "secret").await.expect("login");

    // Browsing: the login directory, then folders before files.
    assert_eq!(sftp.home().await.unwrap(), "/");
    let top = sftp.list("/").await.unwrap();
    let names: Vec<_> = top.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(names, [("docs", EntryKind::Dir), ("readme.md", EntryKind::File)]);
    assert_eq!(top[1].size, 100_000);
    assert!(top[1].modified.is_some());
    assert_eq!(sftp.resolve("/docs/..").await.unwrap(), "/");

    // Download a file, reporting progress up to its size.
    let out = temp_dir("out");
    let mut seen = 0;
    sftp.download("/readme.md", &out.join("readme.md"), plain(), &mut |n| seen = n)
        .await
        .unwrap();
    assert_eq!(seen, 100_000);
    assert_eq!(std::fs::read(out.join("readme.md")).unwrap().len(), 100_000);

    // Download a folder: its tree comes along.
    sftp.download("/docs", &out.join("docs"), plain(), &mut silent())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("docs/notes.txt")).unwrap(),
        "remote notes"
    );

    // Upload a folder with a nested file, then a single file over an existing one.
    let local = temp_dir("local");
    std::fs::create_dir_all(local.join("conf/sub")).unwrap();
    std::fs::write(local.join("conf/a.cfg"), "a").unwrap();
    std::fs::write(local.join("conf/sub/b.cfg"), "bb").unwrap();
    sftp.upload(&local.join("conf"), "/conf", plain(), &mut silent())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(root.join("conf/sub/b.cfg")).unwrap(), "bb");
    std::fs::write(local.join("one.txt"), "new contents").unwrap();
    std::fs::write(root.join("one.txt"), "old contents that are longer").unwrap();
    sftp.upload(&local.join("one.txt"), "/one.txt", plain(), &mut silent())
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(root.join("one.txt")).unwrap(), "new contents");

    // Rename, make a folder, delete a folder with contents.
    sftp.rename("/one.txt", "/two.txt").await.unwrap();
    assert!(root.join("two.txt").exists() && !root.join("one.txt").exists());
    sftp.mkdir("/fresh").await.unwrap();
    assert!(root.join("fresh").is_dir());
    sftp.remove("/conf").await.unwrap();
    assert!(!root.join("conf").exists());
    sftp.remove("/two.txt").await.unwrap();
    assert!(!root.join("two.txt").exists());

    // A missing path is an error, not a hang.
    assert!(sftp.list("/nope").await.is_err());
    sftp.close().await;
    assert_eq!(join("/a", "b"), "/a/b");
}

#[tokio::test]
async fn a_wrong_password_fails_to_connect() {
    let root = temp_dir("root");
    let port = start_server(root).await;
    let err = connect(port, "wrong").await.err().expect("login should fail");
    assert!(!err.to_string().is_empty());
}

// ---- cancel, resume, permissions ---------------------------------------------

static NEVER: AtomicBool = AtomicBool::new(false);

/// A transfer that is never canceled and refuses to resume.
fn plain() -> Transfer<'static> {
    Transfer {
        resume: false,
        cancel: &NEVER,
    }
}

fn resuming() -> Transfer<'static> {
    Transfer {
        resume: true,
        cancel: &NEVER,
    }
}

/// `n` bytes that differ from place to place, so a wrong offset shows.
fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[tokio::test]
async fn a_canceled_download_stops_and_can_be_resumed() {
    let root = temp_dir("root");
    let data = pattern(300_000);
    std::fs::write(root.join("big.bin"), &data).unwrap();
    let port = start_server(root).await;
    let sftp = connect(port, "secret").await.expect("login");
    let out = temp_dir("out");
    let dest = out.join("big.bin");

    let cancel = AtomicBool::new(false);
    let opts = Transfer {
        resume: false,
        cancel: &cancel,
    };
    let err = sftp
        .download("/big.bin", &dest, opts, &mut |n| {
            if n >= 64 * 1024 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    let partial = std::fs::metadata(&dest).unwrap().len() as usize;
    assert!(partial >= 64 * 1024 && partial < data.len(), "kept {partial} bytes");

    // Resuming picks up where it stopped, and progress counts the part already there.
    let mut last = 0;
    sftp.download("/big.bin", &dest, resuming(), &mut |n| last = n)
        .await
        .unwrap();
    assert_eq!(last as usize, data.len());
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    // A complete file is left alone, and a longer (unrelated) one is started over.
    sftp.download("/big.bin", &dest, resuming(), &mut |n| last = n)
        .await
        .unwrap();
    assert_eq!(last as usize, data.len());
    std::fs::write(&dest, vec![9u8; 400_000]).unwrap();
    sftp.download("/big.bin", &dest, resuming(), &mut silent())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    sftp.close().await;
}

#[tokio::test]
async fn a_canceled_upload_stops_and_can_be_resumed() {
    let root = temp_dir("root");
    let local = temp_dir("local");
    let data = pattern(300_000);
    std::fs::write(local.join("big.bin"), &data).unwrap();
    let port = start_server(root.clone()).await;
    let sftp = connect(port, "secret").await.expect("login");

    let cancel = AtomicBool::new(false);
    let opts = Transfer {
        resume: false,
        cancel: &cancel,
    };
    let err = sftp
        .upload(&local.join("big.bin"), "/big.bin", opts, &mut |n| {
            if n >= 64 * 1024 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    let partial = std::fs::metadata(root.join("big.bin")).unwrap().len() as usize;
    assert!(partial >= 64 * 1024 && partial < data.len(), "kept {partial} bytes");

    let mut last = 0;
    sftp.upload(&local.join("big.bin"), "/big.bin", resuming(), &mut |n| last = n)
        .await
        .unwrap();
    assert_eq!(last as usize, data.len());
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), data);

    // Without resume an upload still replaces what is there.
    std::fs::write(root.join("big.bin"), vec![1u8; 10]).unwrap();
    sftp.upload(&local.join("big.bin"), "/big.bin", plain(), &mut silent())
        .await
        .unwrap();
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), data);
    sftp.close().await;
}

#[tokio::test]
async fn permissions_can_be_set_on_one_item_or_a_whole_folder() {
    let root = temp_dir("root");
    std::fs::create_dir_all(root.join("site/sub")).unwrap();
    std::fs::write(root.join("site/a.txt"), "a").unwrap();
    std::fs::write(root.join("site/sub/b.txt"), "b").unwrap();
    let port = start_server(root).await;
    let sftp = connect(port, "secret").await.expect("login");

    sftp.chmod("/site/a.txt", 0o600, false).await.unwrap();
    sftp.chmod("/site", 0o755, true).await.unwrap();
    let mut seen: Vec<_> = CHMODS
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p.starts_with("/site"))
        .cloned()
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("/site".to_string(), 0o755),
            ("/site/a.txt".to_string(), 0o600),
            ("/site/a.txt".to_string(), 0o755),
            ("/site/sub".to_string(), 0o755),
            ("/site/sub/b.txt".to_string(), 0o755),
        ]
    );
    // Only the permission bits are sent, whatever else the caller passed.
    sftp.chmod("/site/a.txt", 0o100644, false).await.unwrap();
    assert!(CHMODS.lock().unwrap().contains(&("/site/a.txt".to_string(), 0o644)));
    assert!(sftp.chmod("/nope", 0o644, false).await.is_err());
    sftp.close().await;
}
