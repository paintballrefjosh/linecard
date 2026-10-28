//! XMODEM, XMODEM-1K and YMODEM file transfer, as used to load images over a
//! console cable (ROMMON `xmodem`, `copy xmodem: flash:`).
//!
//! Both ends are written against [`Link`], so they work over any byte
//! stream (a serial port in the app, an in-memory pipe in tests).
//!
//! * The sender follows the receiver: `C` means CRC-16 (and allows 1K
//!   blocks), NAK means the old 8-bit checksum with 128-byte blocks.
//! * Blocks are retried up to 10 times; two CANs from either side abort.
//! * YMODEM adds a block 0 with the file name and size (so the receiver can
//!   drop the padding) and an empty block 0 to end the batch.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SOH: u8 = 0x01; // 128-byte block
const STX: u8 = 0x02; // 1024-byte block
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a; // padding
const CRC_MODE: u8 = b'C';

const RETRIES: usize = 10;
/// How long a sender waits for the receiver to start (people need time to
/// type `xmodem ...` on the device).
const START_WAIT: Duration = Duration::from_secs(60);
const REPLY_WAIT: Duration = Duration::from_secs(10);
const BYTE_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Xmodem,
    Xmodem1k,
    Ymodem,
}

/// A byte pipe to the other end.
pub trait Link {
    fn send(&mut self, data: &[u8]) -> io::Result<()>;
    /// The next byte, or `None` if nothing arrived within `timeout`.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<u8>>;
}

#[derive(Debug)]
pub enum TransferError {
    /// The user stopped it.
    Canceled,
    /// The device sent CAN CAN.
    CanceledByRemote,
    /// The receiver never started, or stopped answering.
    Timeout,
    /// A block kept failing.
    TooManyErrors,
    Io(io::Error),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Canceled => write!(f, "canceled"),
            TransferError::CanceledByRemote => write!(f, "canceled by the device"),
            TransferError::Timeout => write!(f, "the device stopped responding"),
            TransferError::TooManyErrors => write!(f, "too many errors on the line"),
            TransferError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for TransferError {
    fn from(e: io::Error) -> Self {
        TransferError::Io(e)
    }
}

type Result<T> = std::result::Result<T, TransferError>;

/// CRC-16/XMODEM (CCITT polynomial 0x1021, initial 0).
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

fn abort(link: &mut dyn Link) {
    let _ = link.send(&[CAN, CAN, CAN]);
}

fn check_cancel(link: &mut dyn Link, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        abort(link);
        return Err(TransferError::Canceled);
    }
    Ok(())
}

// ---- sending ---------------------------------------------------------------

/// Wait for the receiver's start signal: `Ok(true)` = CRC mode.
fn wait_start(link: &mut dyn Link, cancel: &AtomicBool, wait: Duration) -> Result<bool> {
    let deadline = Instant::now() + wait;
    let mut cans = 0;
    while Instant::now() < deadline {
        check_cancel(link, cancel)?;
        match link.recv(Duration::from_millis(250))? {
            Some(CRC_MODE) => return Ok(true),
            Some(NAK) => return Ok(false),
            Some(CAN) => {
                cans += 1;
                if cans >= 2 {
                    return Err(TransferError::CanceledByRemote);
                }
            }
            _ => cans = 0, // device chatter before the transfer starts
        }
    }
    Err(TransferError::Timeout)
}

fn send_block(link: &mut dyn Link, crc: bool, num: u8, payload: &[u8], cancel: &AtomicBool) -> Result<()> {
    let mut pkt = Vec::with_capacity(payload.len() + 5);
    pkt.push(if payload.len() == 1024 { STX } else { SOH });
    pkt.push(num);
    pkt.push(!num);
    pkt.extend_from_slice(payload);
    if crc {
        pkt.extend_from_slice(&crc16(payload).to_be_bytes());
    } else {
        pkt.push(checksum(payload));
    }
    for _ in 0..RETRIES {
        check_cancel(link, cancel)?;
        link.send(&pkt)?;
        let deadline = Instant::now() + REPLY_WAIT;
        loop {
            check_cancel(link, cancel)?;
            match link.recv(Duration::from_millis(250))? {
                Some(ACK) => return Ok(()),
                Some(NAK) | Some(CRC_MODE) => break, // resend
                Some(CAN) => {
                    if link.recv(BYTE_WAIT)? == Some(CAN) {
                        return Err(TransferError::CanceledByRemote);
                    }
                }
                Some(_) => {}
                None if Instant::now() >= deadline => break,
                None => {}
            }
        }
    }
    abort(link);
    Err(TransferError::TooManyErrors)
}

fn padded(chunk: &[u8], size: usize) -> Vec<u8> {
    let mut v = chunk.to_vec();
    v.resize(size, SUB);
    v
}

fn send_data(
    link: &mut dyn Link,
    crc: bool,
    big: bool,
    data: &[u8],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<()> {
    let mut num: u8 = 1;
    let mut sent = 0;
    while sent < data.len() {
        let left = data.len() - sent;
        // 1K blocks only in CRC mode, and not for a short tail (less padding).
        let size = if big && crc && left > 128 { 1024 } else { 128 };
        let chunk = &data[sent..data.len().min(sent + size)];
        send_block(link, crc, num, &padded(chunk, size), cancel)?;
        sent += chunk.len();
        num = num.wrapping_add(1);
        progress(sent as u64);
    }
    // EOT; receivers commonly NAK the first one.
    for _ in 0..RETRIES {
        check_cancel(link, cancel)?;
        link.send(&[EOT])?;
        match link.recv(REPLY_WAIT)? {
            Some(ACK) => return Ok(()),
            _ => continue,
        }
    }
    Err(TransferError::TooManyErrors)
}

/// Send `data`. `name` is only used by YMODEM. `progress` gets bytes sent.
pub fn send(
    link: &mut dyn Link,
    protocol: Protocol,
    name: &str,
    data: &[u8],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<()> {
    let crc = wait_start(link, cancel, START_WAIT)?;
    if protocol != Protocol::Ymodem {
        return send_data(link, crc, protocol == Protocol::Xmodem1k, data, cancel, progress);
    }
    // Block 0: "name\0size\0", then the receiver asks again with 'C'.
    let mut header = name.as_bytes().to_vec();
    header.push(0);
    header.extend_from_slice(data.len().to_string().as_bytes());
    header.push(0);
    let size = if header.len() > 128 { 1024 } else { 128 };
    header.resize(size, 0);
    send_block(link, crc, 0, &header, cancel)?;
    let crc = wait_start(link, cancel, REPLY_WAIT)?;
    send_data(link, crc, true, data, cancel, progress)?;
    // End of batch: an empty block 0.
    let crc = wait_start(link, cancel, REPLY_WAIT)?;
    send_block(link, crc, 0, &[0u8; 128], cancel)
}

// ---- receiving -------------------------------------------------------------

fn read_exact(link: &mut dyn Link, n: usize) -> Result<Option<Vec<u8>>> {
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        match link.recv(BYTE_WAIT)? {
            Some(b) => v.push(b),
            None => return Ok(None),
        }
    }
    Ok(Some(v))
}

/// Discard whatever is left of a bad block, then ask for it again.
fn purge_and_nak(link: &mut dyn Link) -> Result<()> {
    while link.recv(Duration::from_millis(200))?.is_some() {}
    link.send(&[NAK])?;
    Ok(())
}

enum Block {
    Data(u8, Vec<u8>),
    Eot,
}

/// Read one block (after the start signal). `None` = bad or missing block,
/// already NAKed.
fn read_block(link: &mut dyn Link, crc: bool, cancel: &AtomicBool) -> Result<Option<Block>> {
    check_cancel(link, cancel)?;
    let size = match link.recv(REPLY_WAIT)? {
        Some(SOH) => 128,
        Some(STX) => 1024,
        Some(EOT) => return Ok(Some(Block::Eot)),
        Some(CAN) => {
            if link.recv(BYTE_WAIT)? == Some(CAN) {
                return Err(TransferError::CanceledByRemote);
            }
            return Ok(None);
        }
        Some(_) => {
            purge_and_nak(link)?;
            return Ok(None);
        }
        None => {
            link.send(&[if crc { CRC_MODE } else { NAK }])?;
            return Ok(None);
        }
    };
    let trailer = if crc { 2 } else { 1 };
    let Some(rest) = read_exact(link, 2 + size + trailer)? else {
        purge_and_nak(link)?;
        return Ok(None);
    };
    let (num, inv, payload) = (rest[0], rest[1], &rest[2..2 + size]);
    let ok = num == !inv
        && if crc {
            crc16(payload).to_be_bytes() == rest[2 + size..]
        } else {
            checksum(payload) == rest[2 + size]
        };
    if !ok {
        purge_and_nak(link)?;
        return Ok(None);
    }
    Ok(Some(Block::Data(num, payload.to_vec())))
}

/// Start a transfer: try CRC mode, fall back to checksum. Returns CRC mode
/// and the first block.
fn start_receive(link: &mut dyn Link, cancel: &AtomicBool) -> Result<(bool, Block)> {
    for attempt in 0..RETRIES {
        let crc = attempt < 4;
        check_cancel(link, cancel)?;
        link.send(&[if crc { CRC_MODE } else { NAK }])?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match link.recv(Duration::from_millis(250))? {
                Some(b @ (SOH | STX)) => {
                    let size = if b == STX { 1024 } else { 128 };
                    let trailer = if crc { 2 } else { 1 };
                    if let Some(rest) = read_exact(link, 2 + size + trailer)? {
                        let payload = &rest[2..2 + size];
                        let good = rest[0] == !rest[1]
                            && if crc {
                                crc16(payload).to_be_bytes() == rest[2 + size..]
                            } else {
                                checksum(payload) == rest[2 + size]
                            };
                        if good {
                            return Ok((crc, Block::Data(rest[0], payload.to_vec())));
                        }
                    }
                    purge_and_nak(link)?;
                }
                Some(EOT) => return Ok((crc, Block::Eot)),
                Some(CAN) if link.recv(BYTE_WAIT)? == Some(CAN) => {
                    return Err(TransferError::CanceledByRemote);
                }
                _ => {}
            }
        }
    }
    Err(TransferError::Timeout)
}

/// Receive data blocks starting at block 1 until EOT. `limit` (YMODEM's
/// size) trims the padding exactly; otherwise trailing SUB bytes are dropped.
fn receive_data(
    link: &mut dyn Link,
    crc: bool,
    first: Option<Block>,
    limit: Option<u64>,
    out: &mut dyn Write,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<u64> {
    let mut expected: u8 = 1;
    let mut written: u64 = 0;
    // The newest block is held back until we know whether it's the last.
    let mut held: Option<Vec<u8>> = None;
    let mut errors = 0;
    let mut next = first;
    loop {
        let block = match next.take() {
            Some(b) => b,
            None => match read_block(link, crc, cancel)? {
                Some(b) => b,
                None => {
                    errors += 1;
                    if errors >= RETRIES {
                        abort(link);
                        return Err(TransferError::TooManyErrors);
                    }
                    continue;
                }
            },
        };
        match block {
            Block::Eot => {
                link.send(&[ACK])?;
                if let Some(mut last) = held.take() {
                    match limit {
                        Some(total) => last.truncate(total.saturating_sub(written) as usize),
                        None => {
                            while last.last() == Some(&SUB) {
                                last.pop();
                            }
                        }
                    }
                    out.write_all(&last)?;
                    written += last.len() as u64;
                }
                progress(written);
                return Ok(written);
            }
            Block::Data(num, payload) if num == expected => {
                if let Some(prev) = held.replace(payload) {
                    out.write_all(&prev)?;
                    written += prev.len() as u64;
                    progress(written);
                }
                expected = expected.wrapping_add(1);
                errors = 0;
                link.send(&[ACK])?;
            }
            // Our ACK was lost and the sender repeated the block.
            Block::Data(num, _) if num == expected.wrapping_sub(1) => link.send(&[ACK])?,
            Block::Data(..) => {
                abort(link);
                return Err(TransferError::TooManyErrors);
            }
        }
    }
}

/// Receive one file into `out`. For YMODEM the sender's file name is
/// returned too. `progress` gets bytes received.
pub fn receive(
    link: &mut dyn Link,
    protocol: Protocol,
    out: &mut dyn Write,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(u64, Option<String>)> {
    let (crc, first) = start_receive(link, cancel)?;
    if protocol != Protocol::Ymodem {
        let n = receive_data(link, crc, Some(first), None, out, cancel, progress)?;
        return Ok((n, None));
    }
    // Block 0: name and size.
    let Block::Data(0, header) = first else {
        abort(link);
        return Err(TransferError::TooManyErrors);
    };
    let mut fields = header.split(|b| *b == 0);
    let name = String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
    let size = fields
        .next()
        .and_then(|f| std::str::from_utf8(f).ok())
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u64>().ok());
    link.send(&[ACK, CRC_MODE])?;
    let n = receive_data(link, crc, None, size, out, cancel, progress)?;
    // End of batch: ask for the next file and accept the empty block 0.
    link.send(&[CRC_MODE])?;
    if let Some(Block::Data(0, _)) = read_block(link, crc, cancel)? {
        link.send(&[ACK])?;
    }
    Ok((n, Some(name)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Arc;
    use std::thread;

    /// One end of an in-memory serial line. `corrupt` flips a byte in the
    /// nth packet sent (to exercise retries).
    struct Pipe {
        tx: Sender<u8>,
        rx: Receiver<u8>,
        corrupt_packet: Option<usize>,
        packets: usize,
    }

    impl Link for Pipe {
        fn send(&mut self, data: &[u8]) -> io::Result<()> {
            self.packets += 1;
            for (i, &b) in data.iter().enumerate() {
                let b = if Some(self.packets) == self.corrupt_packet && i == 10 {
                    b ^ 0xff
                } else {
                    b
                };
                let _ = self.tx.send(b);
            }
            Ok(())
        }
        fn recv(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
            Ok(self.rx.recv_timeout(timeout).ok())
        }
    }

    fn pipes() -> (Pipe, Pipe) {
        let (a_tx, b_rx) = channel();
        let (b_tx, a_rx) = channel();
        let p = |tx, rx| Pipe {
            tx,
            rx,
            corrupt_packet: None,
            packets: 0,
        };
        (p(a_tx, a_rx), p(b_tx, b_rx))
    }

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 3) as u8).collect()
    }

    fn round_trip(protocol: Protocol, data: Vec<u8>, corrupt: Option<usize>) -> (Vec<u8>, Option<String>) {
        let (mut a, mut b) = pipes();
        a.corrupt_packet = corrupt;
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let sent = data.clone();
        let sender = thread::spawn(move || send(&mut a, protocol, "ios.bin", &sent, &c2, &mut |_| {}));
        let mut out = Vec::new();
        let (n, name) = receive(&mut b, protocol, &mut out, &cancel, &mut |_| {}).unwrap();
        sender.join().unwrap().unwrap();
        assert_eq!(n as usize, out.len());
        (out, name)
    }

    #[test]
    fn crc_matches_the_reference() {
        assert_eq!(crc16(b"123456789"), 0x31c3);
    }

    #[test]
    fn xmodem_round_trips() {
        // Trailing SUBs are trimmed, so test data that doesn't end in one.
        let data = sample(1000);
        assert_eq!(round_trip(Protocol::Xmodem, data.clone(), None).0, data);
        assert_eq!(round_trip(Protocol::Xmodem1k, sample(5000), None).0, sample(5000));
    }

    #[test]
    fn ymodem_keeps_exact_size_and_name() {
        // Ends in SUB bytes on purpose: YMODEM's size keeps them.
        let mut data = sample(3000);
        data.extend_from_slice(&[SUB, SUB, SUB]);
        let (out, name) = round_trip(Protocol::Ymodem, data.clone(), None);
        assert_eq!(out, data);
        assert_eq!(name.as_deref(), Some("ios.bin"));
    }

    #[test]
    fn a_corrupted_block_is_resent() {
        assert_eq!(round_trip(Protocol::Xmodem1k, sample(4000), Some(2)).0, sample(4000));
    }

    #[test]
    fn empty_file() {
        assert_eq!(round_trip(Protocol::Xmodem, vec![], None).0, Vec::<u8>::new());
    }

    #[test]
    fn checksum_mode_when_the_receiver_asks_for_it() {
        // A receiver that only speaks checksum: send NAK, accept 128-byte blocks.
        let (mut a, mut b) = pipes();
        let cancel = AtomicBool::new(false);
        let data = sample(300);
        let sent = data.clone();
        let sender = thread::spawn(move || {
            send(
                &mut a,
                Protocol::Xmodem1k,
                "",
                &sent,
                &AtomicBool::new(false),
                &mut |_| {},
            )
        });
        b.send(&[NAK]).unwrap();
        let mut out = Vec::new();
        let mut first = None;
        for _ in 0..3 {
            let blk = read_block(&mut b, false, &cancel).unwrap().unwrap();
            if let Block::Data(_, p) = &blk {
                assert_eq!(p.len(), 128, "no 1K blocks without CRC");
                out.extend_from_slice(p);
            }
            first.get_or_insert(());
            b.send(&[ACK]).unwrap();
        }
        assert!(matches!(read_block(&mut b, false, &cancel).unwrap(), Some(Block::Eot)));
        b.send(&[ACK]).unwrap();
        sender.join().unwrap().unwrap();
        assert_eq!(&out[..300], &data[..]);
    }

    #[test]
    fn cancel_stops_both_ends() {
        let (mut a, mut b) = pipes();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let sender = thread::spawn(move || {
            let mut n = 0;
            send(&mut a, Protocol::Xmodem1k, "", &sample(200_000), &c2, &mut |_| {
                n += 1;
                if n == 3 {
                    c2.store(true, Ordering::Relaxed);
                }
            })
        });
        let mut out = Vec::new();
        let r = receive(
            &mut b,
            Protocol::Xmodem1k,
            &mut out,
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert!(matches!(sender.join().unwrap(), Err(TransferError::Canceled)));
        assert!(matches!(r, Err(TransferError::CanceledByRemote)), "{r:?}");
    }

    #[test]
    fn no_receiver_times_out() {
        let (mut a, _b) = pipes();
        let r = wait_start(&mut a, &AtomicBool::new(false), Duration::from_millis(300));
        assert!(matches!(r, Err(TransferError::Timeout)));
    }
}
