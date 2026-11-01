//! SFTP over an SSH login (jump hosts, host-key and password prompts work as
//! they do for shells). Paths on the remote side are always `/`-separated.

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use russh::client::Handle;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, OpenFlags};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::ssh::{login, Client, SharedReplies};
use crate::{EventSink, SshOptions, TerminalTarget};

const CHUNK: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Link,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: Option<u64>,
    /// Unix mode bits (permissions), when the server sent them.
    pub mode: Option<u32>,
}

/// Bytes moved so far in one transfer.
pub type Progress<'a> = &'a mut (dyn FnMut(u64) + Send);

/// How a transfer behaves.
#[derive(Clone, Copy)]
pub struct Transfer<'a> {
    /// Continue files that already exist at the destination.
    pub resume: bool,
    /// Set to stop the transfer at the next chunk; what was copied stays.
    pub cancel: &'a AtomicBool,
}

impl Transfer<'_> {
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(io::Error::new(io::ErrorKind::Interrupted, "canceled"))
        } else {
            Ok(())
        }
    }
}

fn fail(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// `dir` + `name` as a remote path.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// A logged-in SFTP session. Dropping it ends the connection.
pub struct SftpClient {
    sftp: SftpSession,
    _handle: Handle<Client>,
    _jumps: Vec<Handle<Client>>,
}

impl SftpClient {
    /// Log in to `target` and start the SFTP subsystem. Prompts go to
    /// `events` and are answered on `replies`.
    pub async fn connect(
        target: &TerminalTarget,
        opts: &SshOptions,
        events: EventSink,
        replies: SharedReplies,
    ) -> io::Result<Self> {
        let (handle, jumps) = login(target, opts, events, replies).await?;
        let channel = handle.channel_open_session().await.map_err(fail)?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| fail(format!("the server didn't start SFTP: {e}")))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| fail(format!("SFTP failed to start: {e}")))?;
        Ok(SftpClient {
            sftp,
            _handle: handle,
            _jumps: jumps,
        })
    }

    /// The login directory.
    pub async fn home(&self) -> io::Result<String> {
        self.sftp.canonicalize(".").await.map_err(fail)
    }

    /// The absolute form of `path` (resolves `..`, `~`-less relative paths).
    pub async fn resolve(&self, path: &str) -> io::Result<String> {
        self.sftp.canonicalize(path).await.map_err(fail)
    }

    /// Directory contents, folders first, then by name (case-insensitive).
    pub async fn list(&self, path: &str) -> io::Result<Vec<SftpEntry>> {
        let mut out: Vec<SftpEntry> = self
            .sftp
            .read_dir(path)
            .await
            .map_err(fail)?
            .filter(|e| e.file_name() != "." && e.file_name() != "..")
            .map(|e| {
                let m = e.metadata();
                SftpEntry {
                    name: e.file_name(),
                    kind: match e.file_type() {
                        FileType::Dir => EntryKind::Dir,
                        FileType::File => EntryKind::File,
                        FileType::Symlink => EntryKind::Link,
                        FileType::Other => EntryKind::Other,
                    },
                    size: m.len(),
                    modified: m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs()),
                    mode: m.permissions,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            (a.kind != EntryKind::Dir)
                .cmp(&(b.kind != EntryKind::Dir))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    /// Whether a remote path is a folder (following links).
    pub async fn is_dir(&self, path: &str) -> io::Result<bool> {
        Ok(self.sftp.metadata(path).await.map_err(fail)?.is_dir())
    }

    pub async fn mkdir(&self, path: &str) -> io::Result<()> {
        self.sftp.create_dir(path).await.map_err(fail)
    }

    pub async fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.sftp.rename(from, to).await.map_err(fail)
    }

    /// Delete a file, or a folder with everything in it.
    pub fn remove<'a>(&'a self, path: &'a str) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let meta = self.sftp.symlink_metadata(path).await.map_err(fail)?;
            if !meta.is_dir() {
                return self.sftp.remove_file(path).await.map_err(fail);
            }
            for e in self.list(path).await? {
                self.remove(&join(path, &e.name)).await?;
            }
            self.sftp.remove_dir(path).await.map_err(fail)
        })
    }

    /// Copy a remote file or folder to `local`. With `resume`, files that
    /// already exist locally are continued (or skipped when complete) rather
    /// than refused; this assumes they are the start of the same file.
    pub async fn download(
        &self,
        remote: &str,
        local: &Path,
        opts: Transfer<'_>,
        progress: Progress<'_>,
    ) -> io::Result<()> {
        let mut done = 0;
        self.download_into(remote, local, opts, &mut done, progress).await
    }

    fn download_into<'a>(
        &'a self,
        remote: &'a str,
        local: &'a Path,
        opts: Transfer<'a>,
        done: &'a mut u64,
        progress: Progress<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            opts.check()?;
            if self.is_dir(remote).await? {
                tokio::fs::create_dir_all(local).await?;
                for e in self.list(remote).await? {
                    let (r, l) = (join(remote, &e.name), local.join(&e.name));
                    self.download_into(&r, &l, opts, done, progress).await?;
                }
                return Ok(());
            }
            let size = self.sftp.metadata(remote).await.map_err(fail)?.len();
            let have = match tokio::fs::metadata(local).await {
                Ok(m) if opts.resume && m.is_file() && m.len() <= size => m.len(),
                _ => 0,
            };
            if have > 0 && have == size {
                *done += size;
                progress(*done);
                return Ok(());
            }
            let mut src = self.sftp.open(remote).await.map_err(fail)?;
            let mut dst = if have > 0 {
                src.seek(io::SeekFrom::Start(have)).await?;
                tokio::fs::OpenOptions::new().append(true).open(local).await?
            } else {
                tokio::fs::File::create(local).await?
            };
            *done += have;
            let mut buf = vec![0u8; CHUNK];
            let copied: io::Result<()> = async {
                loop {
                    opts.check()?;
                    let n = src.read(&mut buf).await?;
                    if n == 0 {
                        return Ok(());
                    }
                    dst.write_all(&buf[..n]).await?;
                    *done += n as u64;
                    progress(*done);
                }
            }
            .await;
            // Close the file even when canceled or failed, so what was copied is really there.
            let closed = dst.flush().await;
            copied.and(closed)
        })
    }

    /// Copy a local file or folder to the remote path `remote`. Existing
    /// remote files are replaced, or with `resume` continued (or skipped
    /// when complete).
    pub async fn upload(
        &self,
        local: &Path,
        remote: &str,
        opts: Transfer<'_>,
        progress: Progress<'_>,
    ) -> io::Result<()> {
        let mut done = 0;
        self.upload_from(local, remote, opts, &mut done, progress).await
    }

    fn upload_from<'a>(
        &'a self,
        local: &'a Path,
        remote: &'a str,
        opts: Transfer<'a>,
        done: &'a mut u64,
        progress: Progress<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            opts.check()?;
            let meta = tokio::fs::metadata(local).await?;
            if meta.is_dir() {
                // An existing folder is fine: its files merge into it.
                if !self.sftp.try_exists(remote).await.map_err(fail)? {
                    self.mkdir(remote).await?;
                }
                let mut rd = tokio::fs::read_dir(local).await?;
                while let Some(e) = rd.next_entry().await? {
                    let name = e.file_name().to_string_lossy().into_owned();
                    self.upload_from(&e.path(), &join(remote, &name), opts, done, progress)
                        .await?;
                }
                return Ok(());
            }
            let have = if opts.resume && self.sftp.try_exists(remote).await.map_err(fail)? {
                let n = self.sftp.metadata(remote).await.map_err(fail)?.len();
                if n <= meta.len() {
                    n
                } else {
                    0
                }
            } else {
                0
            };
            if have > 0 && have == meta.len() {
                *done += have;
                progress(*done);
                return Ok(());
            }
            let mut src = tokio::fs::File::open(local).await?;
            let mut dst = if have > 0 {
                src.seek(io::SeekFrom::Start(have)).await?;
                let mut f = self
                    .sftp
                    .open_with_flags(remote, OpenFlags::WRITE)
                    .await
                    .map_err(fail)?;
                f.seek(io::SeekFrom::Start(have)).await?;
                f
            } else {
                self.sftp
                    .open_with_flags(remote, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE)
                    .await
                    .map_err(fail)?
            };
            *done += have;
            let mut buf = vec![0u8; CHUNK];
            let copied: io::Result<()> = async {
                loop {
                    opts.check()?;
                    let n = src.read(&mut buf).await?;
                    if n == 0 {
                        return Ok(());
                    }
                    dst.write_all(&buf[..n]).await?;
                    *done += n as u64;
                    progress(*done);
                }
            }
            .await;
            // Close the file even when canceled or failed, so what was copied is really there.
            let closed = dst.shutdown().await;
            copied.and(closed)
        })
    }

    /// Set permission bits (e.g. `0o644`) on a file or folder, and with
    /// `recursive` on everything inside a folder too (links are left alone).
    pub fn chmod<'a>(
        &'a self,
        path: &'a str,
        mode: u32,
        recursive: bool,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let meta = self.sftp.symlink_metadata(path).await.map_err(fail)?;
            if meta.is_symlink() {
                return Ok(());
            }
            let attrs = russh_sftp::protocol::FileAttributes {
                permissions: Some(mode & 0o7777),
                ..Default::default()
            };
            self.sftp.set_metadata(path, attrs).await.map_err(fail)?;
            if recursive && meta.is_dir() {
                for e in self.list(path).await? {
                    self.chmod(&join(path, &e.name), mode, true).await?;
                }
            }
            Ok(())
        })
    }

    pub async fn close(&self) {
        let _ = self.sftp.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::join;

    #[test]
    fn joins_remote_paths() {
        assert_eq!(join("/home/a", "x"), "/home/a/x");
        assert_eq!(join("/", "x"), "/x");
        assert_eq!(join("rel/", "x"), "rel/x");
    }
}
