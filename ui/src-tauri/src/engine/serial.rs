//! Arduino serial link with the same auto-detection and reconnect behaviour as
//! the Python engine.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serialport::{ClearBuffer, SerialPort, SerialPortType};

use super::Engine;

/// VIDs commonly used by Arduino boards/adapters (Arduino, CH340, FTDI, CP210x).
const ARDUINO_VIDS: [u16; 7] = [0x2341, 0x2A03, 0x1A86, 0x0403, 0x10C4, 0x0843, 0x1B4F];

pub struct SerialLink {
    port: Mutex<Option<Box<dyn SerialPort>>>,
    pub generation: AtomicU64,
    reconnect: AtomicBool,
    last_port: Mutex<String>,
}

impl Default for SerialLink {
    fn default() -> Self {
        Self {
            port: Mutex::new(None),
            generation: AtomicU64::new(0),
            reconnect: AtomicBool::new(false),
            last_port: Mutex::new(String::new()),
        }
    }
}

impl SerialLink {
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub fn is_connected(&self) -> bool {
        self.port.lock().map(|p| p.is_some()).unwrap_or(false)
    }

    pub fn request_reconnect(&self) {
        self.reconnect.store(true, Ordering::Relaxed);
    }

    fn close(&self) {
        if let Ok(mut guard) = self.port.lock() {
            if let Some(port) = guard.take() {
                let _ = port.clear(ClearBuffer::Input);
                let _ = port.clear(ClearBuffer::Output);
                drop(port);
            }
        }
    }

    /// Write one protocol line. On failure the port is dropped and the
    /// background thread reconnects without blocking the caller.
    pub fn send(&self, data: &str) -> bool {
        let mut guard = match self.port.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        let Some(port) = guard.as_mut() else {
            return false;
        };
        if port.write_all(data.as_bytes()).is_err() || port.flush().is_err() {
            guard.take();
            self.reconnect.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn adopt(&self, port: Box<dyn SerialPort>, name: &str) {
        if let Ok(mut guard) = self.port.lock() {
            *guard = Some(port);
        }
        if let Ok(mut last) = self.last_port.lock() {
            *last = name.to_string();
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

/// Ports that may be the Arduino, most likely first.
fn candidate_ports(last: &str) -> Vec<String> {
    let mut scored: Vec<(i32, String)> = Vec::new();
    if let Ok(ports) = serialport::available_ports() {
        for info in ports {
            let (vid, text, is_usb) = match &info.port_type {
                SerialPortType::UsbPort(usb) => {
                    let mut text = String::new();
                    if let Some(product) = &usb.product {
                        text.push_str(product);
                        text.push(' ');
                    }
                    if let Some(manufacturer) = &usb.manufacturer {
                        text.push_str(manufacturer);
                    }
                    (Some(usb.vid), text.to_lowercase(), true)
                }
                _ => continue,
            };
            if !is_usb {
                continue;
            }
            if text.contains("acpi") || text.contains("communications port") {
                continue;
            }

            let mut score = 0;
            if text.contains("arduino") {
                score -= 40;
            }
            if vid.map(|v| ARDUINO_VIDS.contains(&v)).unwrap_or(false) {
                score -= 20;
            }
            if text.contains("usb") {
                score -= 10;
            }
            if text.contains("serial") {
                score -= 5;
            }
            scored.push((score, info.port_name));
        }
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut ports: Vec<String> = scored.into_iter().map(|(_, name)| name).collect();
    if !last.is_empty() {
        if let Some(index) = ports.iter().position(|name| name == last) {
            let name = ports.remove(index);
            ports.insert(0, name);
        }
    }
    ports
}

fn wait_ready(port: &mut Box<dyn SerialPort>) -> bool {
    let _ = port.set_timeout(Duration::from_millis(50));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 256];
    while Instant::now() < deadline {
        match port.read(&mut chunk) {
            Ok(0) => {}
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.windows(5).any(|w| w == b"READY") {
                    let _ = port.set_timeout(Duration::from_millis(0));
                    return true;
                }
                if buffer.len() > 2048 {
                    let keep = buffer.len() - 2048;
                    buffer.drain(..keep);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    let _ = port.set_timeout(Duration::from_millis(0));
    false
}

fn try_open(name: &str) -> Option<Box<dyn SerialPort>> {
    let mut port = serialport::new(name, 115_200)
        .timeout(Duration::from_millis(0))
        .open()
        .ok()?;
    let _ = port.clear(ClearBuffer::Input);
    let _ = wait_ready(&mut port);
    Some(port)
}

fn connect_once(engine: &Engine) -> Option<String> {
    let configured = engine.settings.lock().ok()?.str("porta");
    let last = engine.serial.last_port.lock().map(|p| p.clone()).unwrap_or_default();

    let mut names: Vec<String> = Vec::new();
    if !configured.is_empty() {
        names.push(configured);
    }
    for name in candidate_ports(&last) {
        if !names.contains(&name) {
            names.push(name);
        }
    }

    for name in names {
        if let Some(port) = try_open(&name) {
            engine.serial.adopt(port, &name);
            if let Ok(mut state) = engine.state.lock() {
                state.arduino_ok = true;
                state.porta = name.clone();
            }
            return Some(name);
        }
    }
    None
}

pub fn spawn(engine: Arc<Engine>) {
    thread::spawn(move || {
        loop {
            if engine.serial.reconnect.swap(false, Ordering::Relaxed) {
                engine.serial.close();
                if let Ok(mut state) = engine.state.lock() {
                    state.arduino_ok = false;
                }
            }

            if !engine.serial.is_connected() {
                if connect_once(&engine).is_none() {
                    if let Ok(mut state) = engine.state.lock() {
                        state.arduino_ok = false;
                        state.porta.clear();
                    }
                    thread::sleep(Duration::from_secs(3));
                }
            } else {
                thread::sleep(Duration::from_millis(500));
            }
        }
    });
}
