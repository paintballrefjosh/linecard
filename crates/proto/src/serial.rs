//! Serial consoles (COM ports / USB console cables) via `serialport`.
//!
//! A reader thread forwards bytes to the UI; writes go straight to the port.
//! BREAK is supported for password recovery and ROMMON on network gear.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use linecard_core_model::{FlowControl, Parity, SerialSettings};
use serialport::SerialPort;

use crate::{ConnectionKind, EventSink, Session, SessionEvent};

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
}

impl SerialSession {
    pub fn connect(settings: &SerialSettings, mut events: EventSink) -> io::Result<Self> {
        let port = builder(settings)?.open().map_err(|e| match e.kind {
            serialport::ErrorKind::NoDevice => io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} isn't present (is the cable plugged in?)", settings.port),
            ),
            _ => io::Error::other(format!("{}: {}", settings.port, e.description)),
        })?;
        let mut reader = port.try_clone().map_err(|e| io::Error::other(e.description))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = stop.clone();
        events(SessionEvent::Connected);
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
                        Ok(n) => events(SessionEvent::Data(buf[..n].to_vec())),
                        Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        // Cable unplugged, adapter removed…
                        Err(e) => break Some(e.to_string()),
                    }
                };
                events(SessionEvent::Disconnected { reason });
            })?;
        Ok(SerialSession { port, stop })
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

    fn close(&mut self) -> io::Result<()> {
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
