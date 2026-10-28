//! Serial consoles (COM ports / USB console cables) via `serialport`.
//!
//! A reader thread forwards bytes to the UI; writes go straight to the port.
//! BREAK is supported for password recovery and ROMMON on network gear, and
//! XMODEM/YMODEM transfers borrow the line while they run.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use linecard_core_model::{FlowControl, Parity, SerialSettings};
use serialport::SerialPort;

use crate::xmodem::{self, Link};
use crate::{ConnectionKind, EventSink, Session, SessionEvent, TransferRequest, TransferStatus};

/// How long BREAK is held (Cisco wants ≥ 250 ms; most gear accepts this).
const BREAK_MS: u64 = 300;

/// A port the user can pick, e.g. "COM3" / "USB Serial Port".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub name: String,
    pub description: Option<String>,
}

/// Serial ports present right now.
pub fn list_ports() -> Vec<PortInfo> {
    let mut ports: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| PortInfo {
            description: match p.port_type {
                serialport::SerialPortType::UsbPort(u) => u.product.or(u.manufacturer),
                serialport::SerialPortType::BluetoothPort => Some("Bluetooth".into()),
                _ => None,
            },
            name: p.port_name,
        })
        .collect();
    // COM3 before COM10.
    ports.sort_by_key(|p| {
        let digits: String = p.name.chars().filter(char::is_ascii_digit).collect();
        (
            p.name.trim_end_matches(char::is_numeric).to_string(),
            digits.parse::<u32>().unwrap_or(0),
        )
    });
    ports
}

fn config_err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

/// Map our settings onto the port builder; rejects what the OS layer can't do.
fn builder(s: &SerialSettings) -> io::Result<serialport::SerialPortBuilder> {
    let data_bits = match s.data_bits {
        5 => serialport::DataBits::Five,
        6 => serialport::DataBits::Six,
        7 => serialport::DataBits::Seven,
        8 => serialport::DataBits::Eight,
        _ => return Err(config_err("data bits must be 5–8")),
    };
    let parity = match s.parity {
        Parity::None => serialport::Parity::None,
        Parity::Odd => serialport::Parity::Odd,
        Parity::Even => serialport::Parity::Even,
        Parity::Mark | Parity::Space => return Err(config_err("mark and space parity aren't supported")),
    };
    let stop_bits = match s.stop_bits {
        1 => serialport::StopBits::One,
        2 => serialport::StopBits::Two,
        _ => return Err(config_err("stop bits must be 1 or 2")),
    };
    let flow = match s.flow {
        FlowControl::None => serialport::FlowControl::None,
        FlowControl::Software => serialport::FlowControl::Software,
        FlowControl::Hardware => serialport::FlowControl::Hardware,
    };
    Ok(serialport::new(s.port.clone(), s.baud)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(stop_bits)
        .flow_control(flow)
        .timeout(Duration::from_millis(50)))
}

pub struct SerialSession {
    port: Box<dyn SerialPort>,
    stop: Arc<AtomicBool>,
    events: Arc<Mutex<EventSink>>,
    /// While a transfer runs, received bytes go here instead of the terminal.
    route: Arc<Mutex<Option<Sender<Vec<u8>>>>>,
    /// Cancel flag of the running transfer, if any.
    transfer: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

fn emit(events: &Mutex<EventSink>, e: SessionEvent) {
    if let Ok(mut sink) = events.lock() {
        sink(e);
    }
}

impl SerialSession {
    pub fn connect(settings: &SerialSettings, events: EventSink) -> io::Result<Self> {
        let port = builder(settings)?.open().map_err(|e| match e.kind {
            serialport::ErrorKind::NoDevice => io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} isn't present (is the cable plugged in?)", settings.port),
            ),
            _ => io::Error::other(format!("{}: {}", settings.port, e.description)),
        })?;
        let mut reader = port.try_clone().map_err(|e| io::Error::other(e.description))?;
        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(events));
        let route: Arc<Mutex<Option<Sender<Vec<u8>>>>> = Arc::default();
        let (stop_reader, events_reader, route_reader) = (stop.clone(), events.clone(), route.clone());
        emit(&events, SessionEvent::Connected);
        thread::Builder::new()
            .name(format!("serial-{}", settings.port))
            .spawn(move || {
                let mut buf = [0u8; 4096];
                let reason = loop {
                    if stop_reader.load(Ordering::Relaxed) {
                        break None;
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => {}
                        Ok(n) => {
                            let to_transfer = route_reader.lock().ok().and_then(|r| r.clone());
                            match to_transfer {
                                Some(tx) => {
                                    let _ = tx.send(buf[..n].to_vec());
                                }
                                None => emit(&events_reader, SessionEvent::Data(buf[..n].to_vec())),
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        // Cable unplugged, adapter removed…
                        Err(e) => break Some(e.to_string()),
                    }
                };
                emit(&events_reader, SessionEvent::Disconnected { reason });
            })?;
        Ok(SerialSession {
            port,
            stop,
            events,
            route,
            transfer: Arc::default(),
        })
    }
}

/// The serial port as an XMODEM [`Link`]: writes go to the port, reads come
/// from the reader thread while it's routing to us.
struct PortLink {
    port: Box<dyn SerialPort>,
    rx: Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
}

impl Link for PortLink {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.port.write_all(data)?;
        self.port.flush()
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        if self.pending.is_empty() {
            if let Ok(chunk) = self.rx.recv_timeout(timeout) {
                self.pending.extend(chunk);
            }
        }
        Ok(self.pending.pop_front())
    }
}

impl SerialSession {
    fn run_transfer(&mut self, req: TransferRequest) -> io::Result<()> {
        let mut slot = self
            .transfer
            .lock()
            .map_err(|_| io::Error::other("transfer state poisoned"))?;
        if slot.is_some() {
            return Err(io::Error::other("a transfer is already running on this line"));
        }
        let (path, protocol, sending) = match &req {
            TransferRequest::Send { path, protocol } => (path.clone(), *protocol, true),
            TransferRequest::Receive { path, protocol } => (path.clone(), *protocol, false),
        };
        let file = path
            .file_name()
            .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        // Read (or create) the file up front so mistakes show immediately.
        let data = if sending { Some(std::fs::read(&path)?) } else { None };
        let mut out = if sending {
            None
        } else {
            Some(std::fs::File::create(&path)?)
        };
        let total = data.as_ref().map(|d| d.len() as u64);

        let (tx, rx) = channel();
        let mut link = PortLink {
            port: self.port.try_clone().map_err(|e| io::Error::other(e.description))?,
            rx,
            pending: VecDeque::new(),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        *slot = Some(cancel.clone());
        drop(slot);
        *self.route.lock().map_err(|_| io::Error::other("route poisoned"))? = Some(tx);

        let (events, route, slot) = (self.events.clone(), self.route.clone(), self.transfer.clone());
        let ymodem_name = file.clone();
        let status = move |done: u64, result: Option<Result<(), String>>| {
            SessionEvent::Transfer(TransferStatus {
                file: file.clone(),
                sending,
                done,
                total,
                result,
            })
        };
        thread::Builder::new().name("xmodem".into()).spawn(move || {
            emit(&events, status(0, None));
            // Progress at most ~10 times a second.
            let mut last = Instant::now();
            let mut progress = |done: u64| {
                if last.elapsed() >= Duration::from_millis(100) {
                    last = Instant::now();
                    emit(&events, status(done, None));
                }
            };
            let result = match (&data, &mut out) {
                (Some(d), _) => {
                    xmodem::send(&mut link, protocol, &ymodem_name, d, &cancel, &mut progress).map(|_| d.len() as u64)
                }
                (None, Some(f)) => xmodem::receive(&mut link, protocol, f, &cancel, &mut progress).map(|(n, _)| n),
                _ => unreachable!("send has data, receive has a file"),
            };
            // Back to the terminal.
            if let Ok(mut r) = route.lock() {
                *r = None;
            }
            if let Ok(mut s) = slot.lock() {
                *s = None;
            }
            match result {
                Ok(n) => emit(&events, status(n, Some(Ok(())))),
                Err(e) => emit(&events, status(0, Some(Err(e.to_string())))),
            }
        })?;
        Ok(())
    }
}

impl Session for SerialSession {
    fn kind(&self) -> ConnectionKind {
        ConnectionKind::Serial
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.port.write_all(data)
    }

    fn resize(&mut self, _cols: u16, _rows: u16) -> io::Result<()> {
        Ok(()) // a serial line has no window size
    }

    fn send_break(&mut self) -> io::Result<()> {
        self.port.set_break().map_err(|e| io::Error::other(e.description))?;
        thread::sleep(Duration::from_millis(BREAK_MS));
        self.port.clear_break().map_err(|e| io::Error::other(e.description))
    }

    fn start_transfer(&mut self, req: TransferRequest) -> io::Result<()> {
        self.run_transfer(req)
    }

    fn cancel_transfer(&mut self) {
        if let Some(c) = self.transfer.lock().ok().and_then(|t| t.clone()) {
            c.store(true, Ordering::Relaxed);
        }
    }

    fn close(&mut self) -> io::Result<()> {
        self.cancel_transfer();
        self.stop.store(true, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for SerialSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_map_and_unsupported_ones_are_rejected() {
        assert!(builder(&SerialSettings::console("COM3")).is_ok());
        let mut s = SerialSettings::console("COM3");
        s.parity = Parity::Mark;
        assert!(builder(&s).is_err());
        s = SerialSettings::console("COM3");
        s.data_bits = 9;
        assert!(builder(&s).is_err());
    }

    #[test]
    fn a_missing_port_is_a_clear_error() {
        let Err(e) = SerialSession::connect(&SerialSettings::console("COM250"), Box::new(|_| {})) else {
            panic!("COM250 shouldn't exist");
        };
        assert!(e.to_string().contains("COM250"), "{e}");
    }

    #[test]
    fn listing_ports_never_fails() {
        let _ = list_ports();
    }
}
