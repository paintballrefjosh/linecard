//! SFTP over an SSH login (jump hosts, host-key and password prompts work as
//! they do for shells). Paths on the remote side are always `/`-separated.

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::time::UNIX_EPOCH;

use russh::client::Handle;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, OpenFlags};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// Copy a remote file or folder to `local` (a path that doesn't exist yet).
    pub async fn download(&self, remote: &str, local: &Path, progress: Progress<'_>) -> io::Result<()> {
        let mut done = 0;
        self.download_into(remote, local, &mut done, progress).await
    }

    fn download_into<'a>(
        &'a self,
        remote: &'a str,
        local: &'a Path,
        done: &'a mut u64,
        progress: Progress<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if self.is_dir(remote).await? {
                tokio::fs::create_dir_all(local).await?;
                for e in self.list(remote).await? {
                    let (r, l) = (join(remote, &e.name), local.join(&e.name));
                    self.download_into(&r, &l, done, progress).await?;
                }
                return Ok(());
            }
            let mut src = self.sftp.open(remote).await.map_err(fail)?;
            let mut dst = tokio::fs::File::create(local).await?;
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = src.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n]).await?;
                *done += n as u64;
                progress(*done);
            }
            dst.flush().await
        })
    }

    /// Copy a local file or folder to the remote path `remote`.
    pub async fn upload(&self, local: &Path, remote: &str, progress: Progress<'_>) -> io::Result<()> {
        let mut done = 0;
        self.upload_from(local, remote, &mut done, progress).await
    }

    fn upload_from<'a>(
        &'a self,
        local: &'a Path,
        remote: &'a str,
        done: &'a mut u64,
        progress: Progress<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if tokio::fs::metadata(local).await?.is_dir() {
                // An existing folder is fine: its files merge into it.
                if !self.sftp.try_exists(remote).await.map_err(fail)? {
                    self.mkdir(remote).await?;
                }
                let mut rd = tokio::fs::read_dir(local).await?;
                while let Some(e) = rd.next_entry().await? {
                    let name = e.file_name().to_string_lossy().into_owned();
                    self.upload_from(&e.path(), &join(remote, &name), done, progress)
                        .await?;
                }
                return Ok(());
            }
            let mut src = tokio::fs::File::open(local).await?;
            let mut dst = self
                .sftp
                .open_with_flags(remote, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE)
                .await
                .map_err(fail)?;
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = src.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n]).await?;
                *done += n as u64;
                progress(*done);
            }
            dst.shutdown().await
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
