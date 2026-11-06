//! ZMODEM file transfer, as `sz` and `rz` (lrzsz) speak it over an SSH or
//! Telnet shell: type `sz file` on the server and Linecard saves it; type
//! `rz` and Linecard sends the file you pick.
//!
//! Written against [`Link`] like X/YMODEM, so it runs over any byte stream.
//!
//! * Headers: hex, binary with CRC-16, or binary with CRC-32 (used whenever
//!   the receiver offers it). Data: ZDLE-escaped subpackets.
//! * Sending streams subpackets and asks for an acknowledgment every
//!   [`ACK_EVERY`] bytes, never running more than [`WINDOW`] ahead, so a slow
//!   receiver (or our own unbounded write queue) can't pile up the file in
//!   memory. A ZRPOS from the receiver resumes from where it asks.
//! * Telnet isn't 8-bit clean outside binary mode, so there every control
//!   character is escaped both ways ([`Options::escape_ctl`]).
//! * Received names are reduced to a bare file name inside the folder you
//!   picked; a name that would leave it, or a device name, is refused.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::xmodem::{crc16, Link, TransferError};

const ZPAD: u8 = b'*';
const ZDLE: u8 = 0x18;
const ZBIN: u8 = b'A';
const ZHEX: u8 = b'B';
const ZBIN32: u8 = b'C';

// Frame types.
const ZRQINIT: u8 = 0;
const ZRINIT: u8 = 1;
const ZSINIT: u8 = 2;
const ZACK: u8 = 3;
const ZFILE: u8 = 4;
const ZSKIP: u8 = 5;
const ZNAK: u8 = 6;
const ZABORT: u8 = 7;
const ZFIN: u8 = 8;
const ZRPOS: u8 = 9;
const ZDATA: u8 = 10;
const ZEOF: u8 = 11;
const ZFERR: u8 = 12;
const ZCRC: u8 = 13;
const ZCAN: u8 = 16;

// Subpacket ends.
const ZCRCE: u8 = b'h'; // end of frame, header follows
const ZCRCG: u8 = b'i'; // more data follows, no reply wanted
const ZCRCQ: u8 = b'j'; // more data follows, ZACK wanted
const ZCRCW: u8 = b'k'; // end of frame, ZACK wanted
const ZRUB0: u8 = b'l'; // escaped 0x7f
const ZRUB1: u8 = b'm'; // escaped 0xff

// ZRINIT capability flags (ZF0).
const CANFDX: u8 = 0x01;
const CANOVIO: u8 = 0x02;
const CANFC32: u8 = 0x20;
const ESCCTL: u8 = 0x40;

/// ZFILE conversion option: binary.
const ZCBIN: u8 = 1;

const XON: u8 = 0x11;
const XOFF: u8 = 0x13;

/// Data per subpacket.
const BLOCK: usize = 1024;
/// Ask the receiver to acknowledge this often while streaming.
const ACK_EVERY: u64 = 32 * 1024;
/// Never run further ahead of the last acknowledgment than this.
const WINDOW: u64 = 256 * 1024;
/// The longest subpacket accepted (lrzsz sends up to 8 KiB).
const MAX_SUBPACKET: usize = 16 * 1024;

/// How long to wait for the other side at each step.
const WAIT: Duration = Duration::from_secs(10);
/// How long to wait for the CR LF after a hex header.
const LINE_END_WAIT: Duration = Duration::from_millis(100);
/// How long a sender listens for a reply it doesn't need yet.
const GLANCE: Duration = Duration::from_millis(100);
/// Tries at each step before giving up.
const RETRIES: usize = 10;

/// What a remote `rz` or `sz` gets told to stop: eight CANs, then
/// backspaces to wipe them from a shell that reads them as input.
pub const ABORT: &[u8] = &[
    ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
];

/// The start of a hex header: how a running `sz` (ZRQINIT, "00") or `rz`
/// (ZRINIT, "01") announces itself in a shell's output.
pub const SENDER_START: &[u8] = b"**\x18B00";
pub const RECEIVER_START: &[u8] = b"**\x18B01";

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Escape every control character (for links that aren't 8-bit clean).
    pub escape_ctl: bool,
}

type Result<T> = std::result::Result<T, TransferError>;

// ---- CRC-32 --------------------------------------------------------------------

/// CRC-32 (IEEE 802.3), as ZMODEM uses it for 32-bit frames.
fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

fn crc32(data: &[u8]) -> u32 {
    !crc32_update(0xffff_ffff, data)
}

// ---- headers -------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    kind: u8,
    /// P0..P3 (a position, little-endian) or ZF3..ZF0 (flags, ZF0 last).
    data: [u8; 4],
}

impl Header {
    fn pos(kind: u8, pos: u64) -> Self {
        Header {
            kind,
            data: (pos as u32).to_le_bytes(),
        }
    }
    fn flags(kind: u8, zf0: u8) -> Self {
        Header {
            kind,
            data: [0, 0, 0, zf0],
        }
    }
    fn position(&self) -> u64 {
        u64::from(u32::from_le_bytes(self.data))
    }
    fn zf0(&self) -> u8 {
        self.data[3]
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_hex(out: &mut Vec<u8>, b: u8) {
    out.push(HEX[usize::from(b >> 4)]);
    out.push(HEX[usize::from(b & 15)]);
}

fn hex_header(h: Header) -> Vec<u8> {
    let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
    let mut body = vec![h.kind];
    body.extend_from_slice(&h.data);
    for &b in &body {
        push_hex(&mut out, b);
    }
    let crc = crc16(&body);
    push_hex(&mut out, (crc >> 8) as u8);
    push_hex(&mut out, crc as u8);
    out.extend_from_slice(b"\r\x8a");
    // An XON wakes a line stopped by XOFF; not after these two.
    if h.kind != ZACK && h.kind != ZFIN {
        out.push(XON);
    }
    out
}

/// Escapes data for the wire.
struct Escaper {
    ctl: bool,
    /// The last byte written (CR after `@` must be escaped).
    last: u8,
}

impl Escaper {
    fn new(opts: Options) -> Self {
        Escaper {
            ctl: opts.escape_ctl,
            last: 0,
        }
    }

    fn push(&mut self, out: &mut Vec<u8>, b: u8) {
        let escape = match b {
            ZDLE | 0x10 | 0x90 | XON | 0x91 | XOFF | 0x93 => true,
            0x0d | 0x8d => self.ctl || self.last & 0x7f == b'@',
            b if b & 0x60 == 0 => self.ctl,
            _ => false,
        };
        if escape {
            out.push(ZDLE);
            out.push(b ^ 0x40);
        } else if self.ctl && b == 0x7f {
            out.extend_from_slice(&[ZDLE, ZRUB0]);
        } else if self.ctl && b == 0xff {
            out.extend_from_slice(&[ZDLE, ZRUB1]);
        } else {
            out.push(b);
        }
        self.last = b;
    }

    fn extend(&mut self, out: &mut Vec<u8>, data: &[u8]) {
        for &b in data {
            self.push(out, b);
        }
    }
}

fn bin_header(h: Header, crc32_frames: bool, opts: Options) -> Vec<u8> {
    let mut out = vec![ZPAD, ZDLE, if crc32_frames { ZBIN32 } else { ZBIN }];
    let mut body = vec![h.kind];
    body.extend_from_slice(&h.data);
    let mut e = Escaper::new(opts);
    e.extend(&mut out, &body);
    if crc32_frames {
        e.extend(&mut out, &crc32(&body).to_le_bytes());
    } else {
        e.extend(&mut out, &crc16(&body).to_be_bytes());
    }
    out
}

fn subpacket(data: &[u8], end: u8, crc32_frames: bool, opts: Options) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 12);
    let mut e = Escaper::new(opts);
    e.extend(&mut out, data);
    out.extend_from_slice(&[ZDLE, end]);
    if crc32_frames {
        let crc = !crc32_update(crc32_update(0xffff_ffff, data), &[end]);
        e.extend(&mut out, &crc.to_le_bytes());
    } else {
        let mut body = data.to_vec();
        body.push(end);
        e.extend(&mut out, &crc16(&body).to_be_bytes());
    }
    // A frame that ends here wants the line to keep moving.
    if end == ZCRCW {
        out.push(XON);
    }
    out
}

// ---- reading -------------------------------------------------------------------

/// One unit off the wire after unescaping.
enum Unit {
    Byte(u8),
    /// A subpacket end (ZCRCE/G/Q/W).
    End(u8),
}

struct Reader<'a> {
    link: &'a mut dyn Link,
    cancel: &'a AtomicBool,
    /// How long to wait for each byte.
    wait: Duration,
}

impl Reader<'_> {
    fn byte(&mut self, timeout: Duration) -> Result<u8> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(TransferError::Canceled);
        }
        self.link.recv(timeout)?.ok_or(TransferError::Timeout)
    }

    /// The next unescaped unit. Flow-control bytes on the line are dropped.
    fn unit(&mut self) -> Result<Unit> {
        loop {
            let b = self.byte(self.wait)?;
            match b {
                XON | XOFF | 0x91 | 0x93 => continue,
                ZDLE => {}
                _ => return Ok(Unit::Byte(b)),
            }
            // Escaped: five CANs in a row (the ZDLE plus four) mean abort.
            let mut cans = 1;
            loop {
                let c = self.byte(self.wait)?;
                match c {
                    ZDLE => {
                        cans += 1;
                        if cans >= 5 {
                            return Err(TransferError::CanceledByRemote);
                        }
                    }
                    XON | XOFF | 0x91 | 0x93 => {}
                    ZCRCE | ZCRCG | ZCRCQ | ZCRCW => return Ok(Unit::End(c)),
                    ZRUB0 => return Ok(Unit::Byte(0x7f)),
                    ZRUB1 => return Ok(Unit::Byte(0xff)),
                    c if c & 0x60 == 0x40 => return Ok(Unit::Byte(c ^ 0x40)),
                    // Garbage after ZDLE: a damaged frame.
                    _ => return Err(TransferError::TooManyErrors),
                }
            }
        }
    }

    fn escaped(&mut self) -> Result<u8> {
        match self.unit()? {
            Unit::Byte(b) => Ok(b),
            Unit::End(_) => Err(TransferError::TooManyErrors),
        }
    }

    fn hex_digit(&mut self) -> Result<u8> {
        loop {
            let b = self.byte(self.wait)? & 0x7f;
            match b {
                b'0'..=b'9' => return Ok(b - b'0'),
                b'a'..=b'f' => return Ok(b - b'a' + 10),
                XON | XOFF => continue,
                _ => return Err(TransferError::TooManyErrors),
            }
        }
    }

    fn hex_byte(&mut self) -> Result<u8> {
        Ok(self.hex_digit()? << 4 | self.hex_digit()?)
    }

    /// The next header, skipping anything before it. `Ok(None)` for a
    /// header that arrived damaged (the caller asks again). The bool is
    /// whether it was a 32-bit frame (its data will be too).
    fn header(&mut self) -> Result<Option<(Header, bool)>> {
        // Find ZPAD [ZPAD] ZDLE <format>. Line noise and terminal output
        // before it are skipped; five CANs abort.
        let mut cans = 0;
        let mut seen_pad = false;
        let mut skipped = 0usize;
        let format = loop {
            let b = self.byte(self.wait)?;
            match b {
                ZPAD | 0xaa => {
                    seen_pad = true;
                    cans = 0;
                }
                ZDLE if seen_pad => {
                    let f = self.byte(self.wait)?;
                    if f == ZDLE {
                        cans = 2;
                        seen_pad = false;
                        continue;
                    }
                    break f;
                }
                ZDLE => {
                    cans += 1;
                    if cans >= 5 {
                        return Err(TransferError::CanceledByRemote);
                    }
                }
                XON | XOFF | 0x91 | 0x93 | b'\r' | b'\n' | 0x8d | 0x8a => {
                    seen_pad = false;
                    cans = 0;
                }
                _ => {
                    seen_pad = false;
                    cans = 0;
                    skipped += 1;
                    // A whole screen of something else: not a ZMODEM peer.
                    if skipped > 64 * 1024 {
                        return Err(TransferError::Timeout);
                    }
                }
            }
        };
        let read_body = |r: &mut Self, hex: bool| -> Result<[u8; 5]> {
            let mut body = [0u8; 5];
            for b in &mut body {
                *b = if hex { r.hex_byte()? } else { r.escaped()? };
            }
            Ok(body)
        };
        let (body, ok, wide) = match format {
            ZHEX => {
                let body = read_body(self, true)?;
                let crc = u16::from(self.hex_byte()?) << 8 | u16::from(self.hex_byte()?);
                // A hex header ends with CR LF (and maybe XON): take them off
                // the line so they don't look like the start of a reply.
                for _ in 0..2 {
                    match self.link.recv(LINE_END_WAIT)? {
                        Some(b'\r' | 0x8d | b'\n' | 0x8a) => {}
                        _ => break,
                    }
                }
                (body, crc16(&body) == crc, false)
            }
            ZBIN => {
                let body = read_body(self, false)?;
                let crc = u16::from_be_bytes([self.escaped()?, self.escaped()?]);
                (body, crc16(&body) == crc, false)
            }
            ZBIN32 => {
                let body = read_body(self, false)?;
                let mut c = [0u8; 4];
                for b in &mut c {
                    *b = self.escaped()?;
                }
                (body, crc32(&body) == u32::from_le_bytes(c), true)
            }
            _ => return Ok(None),
        };
        if !ok {
            return Ok(None);
        }
        Ok(Some((
            Header {
                kind: body[0],
                data: [body[1], body[2], body[3], body[4]],
            },
            wide,
        )))
    }

    /// One data subpacket and how it ended; `Ok(None)` if its CRC was wrong.
    fn subpacket(&mut self, wide: bool) -> Result<Option<(Vec<u8>, u8)>> {
        let mut data = Vec::with_capacity(BLOCK);
        let end = loop {
            match self.unit()? {
                Unit::Byte(b) => {
                    if data.len() >= MAX_SUBPACKET {
                        return Ok(None);
                    }
                    data.push(b);
                }
                Unit::End(e) => break e,
            }
        };
        let ok = if wide {
            let mut c = [0u8; 4];
            for b in &mut c {
                *b = self.escaped()?;
            }
            !crc32_update(crc32_update(0xffff_ffff, &data), &[end]) == u32::from_le_bytes(c)
        } else {
            let crc = u16::from_be_bytes([self.escaped()?, self.escaped()?]);
            let mut body = data.clone();
            body.push(end);
            crc16(&body) == crc
        };
        Ok(ok.then_some((data, end)))
    }
}

// ---- receiving -----------------------------------------------------------------

/// A name from the sender as a bare file name, or `None` if it can't be one
/// (empty, `.`/`..`, or a Windows device name).
pub fn safe_name(raw: &str) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let name: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_end_matches('.').to_string();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    (!device).then_some(name)
}

/// `dir/name`, or `dir/name (2).ext` and so on if that's taken.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

/// Progress: the file being moved, bytes done in it, and its size if known.
pub type Progress<'a> = dyn FnMut(&str, u64, Option<u64>) + 'a;

/// Receive files from a remote `sz` into `dir`. Returns the files written.
pub fn receive(
    link: &mut dyn Link,
    dir: &Path,
    opts: Options,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<Vec<PathBuf>> {
    let result = receive_files(link, dir, opts, cancel, progress);
    if matches!(result, Err(TransferError::Canceled)) {
        let _ = link.send(ABORT);
    }
    result
}

fn receive_files(
    link: &mut dyn Link,
    dir: &Path,
    opts: Options,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<Vec<PathBuf>> {
    let mut flags = CANFDX | CANOVIO | CANFC32;
    if opts.escape_ctl {
        flags |= ESCCTL;
    }
    let zrinit = hex_header(Header::flags(ZRINIT, flags));
    let mut done = Vec::new();
    // The file being received: where, its name, the handle, bytes so far, size.
    let mut current: Option<(PathBuf, String, File, u64, Option<u64>)> = None;
    let mut retry = zrinit.clone();
    let mut tries = 0;
    link.send(&zrinit)?;
    loop {
        let mut r = Reader {
            link: &mut *link,
            cancel,
            wait: WAIT,
        };
        let got = match r.header() {
            Ok(h) => h,
            Err(TransferError::Timeout) => {
                tries += 1;
                if tries > RETRIES {
                    return Err(TransferError::Timeout);
                }
                link.send(&retry)?;
                continue;
            }
            Err(TransferError::TooManyErrors) => None,
            Err(e) => return Err(e),
        };
        let Some((h, wide)) = got else {
            tries += 1;
            if tries > RETRIES {
                return Err(TransferError::TooManyErrors);
            }
            link.send(&retry)?;
            continue;
        };
        match h.kind {
            ZRQINIT => {
                retry = zrinit.clone();
                link.send(&zrinit)?;
            }
            ZSINIT => {
                // The sender's attention string; we have no use for it.
                let _ = r.subpacket(wide)?;
                link.send(&hex_header(Header::pos(ZACK, 0)))?;
            }
            ZFILE => {
                let Some((info, _)) = r.subpacket(wide)? else {
                    link.send(&hex_header(Header::pos(ZNAK, 0)))?;
                    continue;
                };
                let mut parts = info.split(|&b| b == 0);
                let raw = String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
                let size = parts
                    .next()
                    .and_then(|m| String::from_utf8_lossy(m).split_whitespace().next()?.parse().ok());
                let Some(name) = safe_name(&raw) else {
                    // Not a name we can write; skip it.
                    link.send(&hex_header(Header::pos(ZSKIP, 0)))?;
                    continue;
                };
                // The same ZFILE again (our ZRPOS crossed a retry): carry on
                // with the file already open rather than starting a second.
                if let Some((_, open, _, at, _)) = &current {
                    if *open == name {
                        retry = hex_header(Header::pos(ZRPOS, *at));
                        link.send(&retry)?;
                        continue;
                    }
                }
                let path = free_path(dir, &name);
                let file = File::create(&path)?;
                progress(&name, 0, size);
                current = Some((path, name, file, 0, size));
                retry = hex_header(Header::pos(ZRPOS, 0));
                tries = 0;
                link.send(&retry)?;
            }
            ZDATA => {
                let Some((_, name, file, at, size)) = current.as_mut() else {
                    link.send(&hex_header(Header::pos(ZRPOS, 0)))?;
                    continue;
                };
                if h.position() != *at {
                    // Out of step: say where we are and skip to the next header.
                    retry = hex_header(Header::pos(ZRPOS, *at));
                    link.send(&retry)?;
                    continue;
                }
                loop {
                    match r.subpacket(wide) {
                        Ok(Some((data, end))) => {
                            file.write_all(&data)?;
                            *at += data.len() as u64;
                            progress(name, *at, *size);
                            tries = 0;
                            match end {
                                ZCRCW => {
                                    link.send(&hex_header(Header::pos(ZACK, *at)))?;
                                    break;
                                }
                                ZCRCQ => {
                                    r.link.send(&hex_header(Header::pos(ZACK, *at)))?;
                                }
                                ZCRCE => break,
                                _ => {} // ZCRCG: more follows
                            }
                        }
                        Ok(None) | Err(TransferError::TooManyErrors) | Err(TransferError::Timeout) => {
                            // Damaged or stalled: ask for it again from here.
                            tries += 1;
                            if tries > RETRIES {
                                return Err(TransferError::TooManyErrors);
                            }
                            retry = hex_header(Header::pos(ZRPOS, *at));
                            r.link.send(&retry)?;
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            ZEOF => {
                let at_end = current.as_ref().is_some_and(|c| c.3 == h.position());
                if at_end {
                    let (path, _, mut file, _, _) = current.take().expect("checked");
                    file.flush()?;
                    done.push(path);
                    retry = zrinit.clone();
                    tries = 0;
                    link.send(&zrinit)?;
                }
                // Otherwise data is still missing; the sender resends after our ZRPOS.
            }
            ZFIN => {
                link.send(&hex_header(Header::pos(ZFIN, 0)))?;
                // The sender's "OO" (over and out), if it comes.
                for _ in 0..2 {
                    if link.recv(Duration::from_millis(500))?.is_none() {
                        break;
                    }
                }
                return Ok(done);
            }
            ZCAN | ZABORT | ZFERR => return Err(TransferError::CanceledByRemote),
            _ => {}
        }
    }
}

// ---- sending -------------------------------------------------------------------

/// What the receiver told us it can do.
struct Peer {
    crc32: bool,
    escape_ctl: bool,
    /// 0 = can take a full stream; else the most it buffers before writing.
    buffer: u64,
    streaming: bool,
}

/// Send one file to a remote `rz`, as `name`.
pub fn send(
    link: &mut dyn Link,
    file: &mut File,
    name: &str,
    opts: Options,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<u64> {
    let result = send_file(link, file, name, opts, cancel, progress);
    if matches!(result, Err(TransferError::Canceled)) {
        let _ = link.send(ABORT);
    }
    result
}

fn send_file(
    link: &mut dyn Link,
    file: &mut File,
    name: &str,
    opts: Options,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<u64> {
    let size = file.metadata()?.len();
    let peer = await_zrinit(link, cancel)?;
    let o = Options {
        escape_ctl: opts.escape_ctl || peer.escape_ctl,
    };
    let wide = peer.crc32;
    progress(name, 0, Some(size));

    // ZFILE with the name and size; wait to hear where to start.
    let mtime = file
        .metadata()?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let mut info = name.as_bytes().to_vec();
    info.push(0);
    info.extend_from_slice(format!("{size} {mtime:o} 100644 0 1 {size}").as_bytes());
    info.push(0);
    let mut zfile = bin_header(
        Header {
            kind: ZFILE,
            data: [0, 0, 0, ZCBIN],
        },
        wide,
        o,
    );
    zfile.extend(subpacket(&info, ZCRCW, wide, o));

    let mut start = None;
    for _ in 0..RETRIES {
        link.send(&zfile)?;
        match next_header(link, cancel)? {
            Some(h) if h.kind == ZRPOS => {
                start = Some(h.position());
                break;
            }
            Some(h) if h.kind == ZSKIP => {
                return Err(TransferError::Io(io::Error::other(
                    "the server skipped the file (it may exist there already)",
                )))
            }
            Some(h) if h.kind == ZCRC => {
                // It wants the file's CRC to decide whether to resume.
                let crc = file_crc(file)?;
                link.send(&hex_header(Header {
                    kind: ZCRC,
                    data: crc.to_le_bytes(),
                }))?;
            }
            Some(h) if h.kind == ZCAN || h.kind == ZABORT || h.kind == ZFERR => {
                return Err(TransferError::CanceledByRemote)
            }
            _ => {}
        }
    }
    let mut pos = start.ok_or(TransferError::Timeout)?;

    // Data: stream subpackets, with ZCRCQ checkpoints and a bounded window.
    let mut acked = pos;
    let mut errors = 0;
    'resend: loop {
        file.seek(SeekFrom::Start(pos))?;
        link.send(&bin_header(Header::pos(ZDATA, pos), wide, o))?;
        let mut since_ack = 0u64;
        let mut buf = vec![0u8; BLOCK];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(TransferError::Canceled);
            }
            let n = read_full(file, &mut buf)?;
            let last = pos + n as u64 >= size;
            since_ack += n as u64;
            let end = if last {
                ZCRCE
            } else if peer.buffer > 0 && since_ack >= peer.buffer {
                ZCRCW
            } else if !peer.streaming || since_ack >= ACK_EVERY {
                ZCRCQ
            } else {
                ZCRCG
            };
            link.send(&subpacket(&buf[..n], end, wide, o))?;
            pos += n as u64;
            progress(name, pos, Some(size));
            if matches!(end, ZCRCQ | ZCRCW) {
                since_ack = 0;
            }

            // Listen: an ACK moves the window, a ZRPOS rewinds.
            let must_wait = end == ZCRCW || pos - acked >= WINDOW;
            loop {
                let waiting = must_wait && acked < pos;
                if !waiting && !line_has_data(link)? {
                    break;
                }
                let reply = if waiting {
                    next_header(link, cancel)?
                } else {
                    read_header(link, cancel, GLANCE)?
                };
                match reply {
                    Some(h) if h.kind == ZACK => acked = acked.max(h.position().min(pos)),
                    Some(h) if h.kind == ZRPOS => {
                        errors += 1;
                        if errors > RETRIES {
                            return Err(TransferError::TooManyErrors);
                        }
                        pos = h.position().min(size);
                        acked = pos;
                        continue 'resend;
                    }
                    Some(h) if h.kind == ZCAN || h.kind == ZABORT || h.kind == ZFERR => {
                        return Err(TransferError::CanceledByRemote)
                    }
                    None if waiting => {
                        // No answer: start again from what it last confirmed.
                        errors += 1;
                        if errors > RETRIES {
                            return Err(TransferError::Timeout);
                        }
                        pos = acked;
                        continue 'resend;
                    }
                    _ => {}
                }
                if end == ZCRCW && acked >= pos {
                    break;
                }
            }
            if end == ZCRCW && !last {
                // After a ZCRCW the frame is over; open the next one.
                link.send(&bin_header(Header::pos(ZDATA, pos), wide, o))?;
            }
            if last {
                break;
            }
        }

        // ZEOF; the receiver answers ZRINIT when it has everything.
        for _ in 0..RETRIES {
            link.send(&bin_header(Header::pos(ZEOF, size), wide, o))?;
            match next_header(link, cancel)? {
                Some(h) if h.kind == ZRINIT => break 'resend,
                Some(h) if h.kind == ZRPOS => {
                    errors += 1;
                    if errors > RETRIES {
                        return Err(TransferError::TooManyErrors);
                    }
                    pos = h.position().min(size);
                    acked = pos;
                    continue 'resend;
                }
                Some(h) if h.kind == ZCAN || h.kind == ZABORT || h.kind == ZFERR => {
                    return Err(TransferError::CanceledByRemote)
                }
                _ => {}
            }
        }
        return Err(TransferError::Timeout);
    }

    // Done with the batch.
    for _ in 0..RETRIES {
        link.send(&hex_header(Header::pos(ZFIN, 0)))?;
        match next_header(link, cancel)? {
            Some(h) if h.kind == ZFIN => {
                link.send(b"OO")?;
                return Ok(size);
            }
            None => {}
            _ => {}
        }
    }
    // The file arrived; a lost ZFIN only leaves rz to time out on its own.
    Ok(size)
}

fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn file_crc(file: &mut File) -> io::Result<u32> {
    file.seek(SeekFrom::Start(0))?;
    let mut crc = 0xffff_ffff;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buf)? {
            0 => return Ok(!crc),
            n => crc = crc32_update(crc, &buf[..n]),
        }
    }
}

/// Whether the receiver has said something (a ZACK or ZRPOS) we haven't read.
fn line_has_data(link: &mut dyn Link) -> Result<bool> {
    Ok(link.pending()?)
}

/// The next good header, or `None` if nothing (or only damage) arrived in time.
fn next_header(link: &mut dyn Link, cancel: &AtomicBool) -> Result<Option<Header>> {
    read_header(link, cancel, WAIT)
}

/// Like [`next_header`], waiting at most `wait` for each byte.
fn read_header(link: &mut dyn Link, cancel: &AtomicBool, wait: Duration) -> Result<Option<Header>> {
    let mut r = Reader { link, cancel, wait };
    match r.header() {
        Ok(h) => Ok(h.map(|(h, _)| h)),
        Err(TransferError::Timeout) | Err(TransferError::TooManyErrors) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Wait for the receiver's ZRINIT (nudging it with ZRQINIT).
fn await_zrinit(link: &mut dyn Link, cancel: &AtomicBool) -> Result<Peer> {
    for i in 0..RETRIES {
        match next_header(link, cancel)? {
            Some(h) if h.kind == ZRINIT => {
                let f = h.zf0();
                return Ok(Peer {
                    crc32: f & CANFC32 != 0,
                    escape_ctl: f & ESCCTL != 0,
                    buffer: u64::from(u16::from_le_bytes([h.data[0], h.data[1]])),
                    streaming: f & CANFDX != 0 && f & CANOVIO != 0,
                });
            }
            Some(h) if h.kind == ZCAN || h.kind == ZABORT => return Err(TransferError::CanceledByRemote),
            _ => {
                if i > 0 {
                    link.send(&hex_header(Header::pos(ZRQINIT, 0)))?;
                }
            }
        }
    }
    Err(TransferError::Timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Arc;

    /// One end of an in-memory line. `corrupt_at` flips one byte sent at
    /// that offset (once), to exercise ZRPOS recovery.
    struct Pipe {
        tx: Sender<u8>,
        rx: Receiver<u8>,
        held: VecDeque<u8>,
        sent: usize,
        corrupt_at: Option<usize>,
        /// Bytes this end refuses to pass through (to prove escaping works).
        forbidden: Vec<u8>,
    }

    impl Link for Pipe {
        fn send(&mut self, data: &[u8]) -> io::Result<()> {
            for &b in data {
                assert!(!self.forbidden.contains(&b), "unescaped {b:#04x} on the wire");
                let b = if Some(self.sent) == self.corrupt_at {
                    b ^ 0x01
                } else {
                    b
                };
                self.sent += 1;
                let _ = self.tx.send(b);
            }
            Ok(())
        }
        fn recv(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
            if let Some(b) = self.held.pop_front() {
                return Ok(Some(b));
            }
            Ok(self.rx.recv_timeout(timeout).ok())
        }
        fn pending(&mut self) -> io::Result<bool> {
            while let Ok(b) = self.rx.try_recv() {
                self.held.push_back(b);
            }
            Ok(!self.held.is_empty())
        }
    }

    fn pipes() -> (Pipe, Pipe) {
        let (a_tx, b_rx) = channel();
        let (b_tx, a_rx) = channel();
        let p = |tx, rx| Pipe {
            tx,
            rx,
            held: VecDeque::new(),
            sent: 0,
            corrupt_at: None,
            forbidden: vec![],
        };
        (p(a_tx, a_rx), p(b_tx, b_rx))
    }

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 + i / 7) as u8).collect()
    }

    /// Send `data` as "report.bin" from one end and receive it at the other.
    fn round_trip(data: &[u8], mut sender: Pipe, mut receiver: Pipe, opts: Options) -> (Vec<u8>, String) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        std::fs::write(&src, data).unwrap();
        let out = dir.path().join("in");
        std::fs::create_dir(&out).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let out2 = out.clone();
        let rx = std::thread::spawn(move || receive(&mut receiver, &out2, opts, &c, &mut |_, _, _| {}));
        let mut f = File::open(&src).unwrap();
        let sent = send(&mut sender, &mut f, "report.bin", opts, &cancel, &mut |_, _, _| {}).unwrap();
        assert_eq!(sent, data.len() as u64);
        let files = rx.join().unwrap().unwrap();
        assert_eq!(files.len(), 1);
        let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
        (std::fs::read(&files[0]).unwrap(), name)
    }

    #[test]
    fn a_file_goes_across_whole() {
        // Bigger than the window, with every byte value in it.
        let data = sample(600 * 1024 + 123);
        let (a, b) = pipes();
        let (got, name) = round_trip(&data, a, b, Options::default());
        assert_eq!(name, "report.bin");
        assert!(got == data, "contents differ");
    }

    #[test]
    fn empty_and_tiny_files() {
        for n in [0, 1, 1023, 1024, 1025] {
            let data = sample(n);
            let (a, b) = pipes();
            assert_eq!(round_trip(&data, a, b, Options::default()).0, data, "size {n}");
        }
    }

    #[test]
    fn control_characters_are_escaped_for_telnet() {
        let data = sample(40 * 1024);
        let (mut a, mut b) = pipes();
        // Neither side may put a raw NUL or CR inside data on the line.
        a.forbidden = vec![0x00];
        b.forbidden = vec![0x00];
        let opts = Options { escape_ctl: true };
        assert_eq!(round_trip(&data, a, b, opts).0, data);
    }

    #[test]
    fn a_damaged_subpacket_is_sent_again() {
        let data = sample(100 * 1024);
        let (mut a, b) = pipes();
        a.corrupt_at = Some(20_000);
        assert!(round_trip(&data, a, b, Options::default()).0 == data);
    }

    #[test]
    fn escapes_headers_and_crc16_frames() {
        let cancel = AtomicBool::new(false);
        let mut out = Vec::new();
        let mut e = Escaper::new(Options { escape_ctl: true });
        let raw = [0x0d, 0x10, 0x7f, 0xff, b'@', 0x0d, b'A', 0x00, ZDLE];
        e.extend(&mut out, &raw);
        assert!(!out.contains(&0x0d) && !out.contains(&0x00));
        let (mut a, mut b) = pipes();
        a.send(&out).unwrap();
        let mut r = Reader {
            link: &mut b,
            cancel: &cancel,
            wait: WAIT,
        };
        let mut got = vec![];
        for _ in 0..raw.len() {
            match r.unit().unwrap() {
                Unit::Byte(x) => got.push(x),
                Unit::End(_) => panic!("no end here"),
            }
        }
        assert_eq!(got, raw);

        let h = Header::pos(ZDATA, 0x0102_0304);
        let (mut a, mut b) = pipes();
        a.send(&bin_header(h, false, Options::default())).unwrap();
        a.send(&subpacket(b"hello", ZCRCE, false, Options::default())).unwrap();
        let mut r = Reader {
            link: &mut b,
            cancel: &cancel,
            wait: WAIT,
        };
        let (got, wide) = r.header().unwrap().unwrap();
        assert_eq!((got, wide), (h, false));
        assert_eq!(got.position(), 0x0102_0304);
        assert_eq!(r.subpacket(false).unwrap(), Some((b"hello".to_vec(), ZCRCE)));
    }

    #[test]
    fn hex_headers_match_what_sz_prints() {
        // lrzsz's ZRQINIT, byte for byte.
        assert_eq!(hex_header(Header::pos(ZRQINIT, 0)), b"**\x18B00000000000000\r\x8a\x11");
        assert!(hex_header(Header::flags(ZRINIT, 0x23)).starts_with(RECEIVER_START));
    }

    #[test]
    fn cancel_stops_both_ends() {
        let (mut a, _b) = pipes();
        let cancel = AtomicBool::new(true);
        let dir = tempfile::tempdir().unwrap();
        let err = receive(&mut a, dir.path(), Options::default(), &cancel, &mut |_, _, _| {}).unwrap_err();
        assert!(matches!(err, TransferError::Canceled));

        // Five CANs from the other side abort us.
        let (mut a, mut b) = pipes();
        b.send(ABORT).unwrap();
        let cancel = AtomicBool::new(false);
        let err = receive(&mut a, dir.path(), Options::default(), &cancel, &mut |_, _, _| {}).unwrap_err();
        assert!(matches!(err, TransferError::CanceledByRemote), "{err}");
    }

    #[test]
    fn names_stay_inside_the_folder() {
        assert_eq!(safe_name("backup.cfg").as_deref(), Some("backup.cfg"));
        assert_eq!(safe_name("/etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(safe_name("..\\..\\boot.ini").as_deref(), Some("boot.ini"));
        assert_eq!(safe_name("C:evil.txt").as_deref(), Some("C_evil.txt"));
        assert_eq!(safe_name("a\u{0}b\nc").as_deref(), Some("abc"));
        assert_eq!(safe_name(".."), None);
        assert_eq!(safe_name("dir/"), None);
        assert_eq!(safe_name("CON"), None);
        assert_eq!(safe_name("com1.txt"), None);
        assert_eq!(safe_name("console.log").as_deref(), Some("console.log"));

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        assert_eq!(free_path(dir.path(), "a.txt"), dir.path().join("a (2).txt"));
        assert_eq!(free_path(dir.path(), "b.txt"), dir.path().join("b.txt"));
    }
}
