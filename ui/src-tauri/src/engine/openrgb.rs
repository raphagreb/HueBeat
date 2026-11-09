//! Minimal OpenRGB SDK client (protocol 4) without the GPL/Tokio crate.
//!
//! The handshake and packet layout follow the official OpenRGB SDK and the
//! `openrgb-python` client the old engine used.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::Engine;

const HEADER_SIZE: usize = 16;
const PROTOCOL_VERSION: u32 = 4;
const MAX_PAYLOAD: u32 = 64 * 1024 * 1024;

// Packet types.
const REQUEST_CONTROLLER_COUNT: u32 = 0;
const REQUEST_CONTROLLER_DATA: u32 = 1;
const REQUEST_PROTOCOL_VERSION: u32 = 40;
const SET_CLIENT_NAME: u32 = 50;
const RGBCONTROLLER_UPDATELEDS: u32 = 1050;
const RGBCONTROLLER_SETCUSTOMMODE: u32 = 1100;

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(count)?;
        if end > self.data.len() {
            return None;
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.take(count).map(|_| ())
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|value| value as i32)
    }

    fn string(&mut self) -> Option<String> {
        let length = self.u16()? as usize;
        let bytes = self.take(length)?;
        let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
        Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }
}

fn send_packet(
    stream: &mut TcpStream,
    device: u32,
    kind: u32,
    data: &[u8],
) -> Result<(), String> {
    let mut header = [0u8; HEADER_SIZE];
    header[0..4].copy_from_slice(b"ORGB");
    header[4..8].copy_from_slice(&device.to_le_bytes());
    header[8..12].copy_from_slice(&kind.to_le_bytes());
    header[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
    stream.write_all(&header).map_err(|e| e.to_string())?;
    if !data.is_empty() {
        stream.write_all(data).map_err(|e| e.to_string())?;
    }
    stream.flush().map_err(|e| e.to_string())
}

fn read_packet(stream: &mut TcpStream) -> Result<(u32, u32, Vec<u8>), String> {
    let mut header = [0u8; HEADER_SIZE];
    stream.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[0..4] != b"ORGB" {
        return Err("bad OpenRGB packet".to_string());
    }
    let device = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let kind = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let size = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    if size > MAX_PAYLOAD {
        return Err("OpenRGB packet too large".to_string());
    }
    let mut data = vec![0u8; size as usize];
    if size > 0 {
        stream.read_exact(&mut data).map_err(|e| e.to_string())?;
    }
    Ok((device, kind, data))
}

/// Walk the controller data far enough to read the total LED count.
fn parse_led_count(data: &[u8], protocol: u32) -> Option<u32> {
    let mut cursor = Cursor::new(data);
    let _size = cursor.u32()?;
    let _device_type = cursor.i32()?;
    let _name = cursor.string()?;
    if protocol >= 1 {
        let _vendor = cursor.string()?;
    }
    let _description = cursor.string()?;
    let _version = cursor.string()?;
    let _serial = cursor.string()?;
    let _location = cursor.string()?;

    let modes = cursor.u16()?;
    let _active_mode = cursor.i32()?;
    for _ in 0..modes {
        let _name = cursor.string()?;
        let _value = cursor.i32()?;
        let _flags = cursor.u32()?;
        let _speed_min = cursor.u32()?;
        let _speed_max = cursor.u32()?;
        if protocol >= 3 {
            let _brightness_min = cursor.u32()?;
            let _brightness_max = cursor.u32()?;
        }
        let _colors_min = cursor.u32()?;
        let _colors_max = cursor.u32()?;
        let _speed = cursor.u32()?;
        if protocol >= 3 {
            let _brightness = cursor.u32()?;
        }
        let _direction = cursor.u32()?;
        let _color_mode = cursor.u32()?;
        let colors = cursor.u16()? as usize;
        cursor.skip(colors.checked_mul(4)?)?;
    }

    let zones = cursor.u16()?;
    for _ in 0..zones {
        let _name = cursor.string()?;
        let _zone_type = cursor.i32()?;
        let _leds_min = cursor.u32()?;
        let _leds_max = cursor.u32()?;
        let _num_leds = cursor.u32()?;
        let matrix = cursor.u16()? as usize;
        if matrix > 0 {
            let height = cursor.u32()? as usize;
            let width = cursor.u32()? as usize;
            cursor.skip(height.checked_mul(width)?.checked_mul(4)?)?;
        }
        if protocol >= 4 {
            let segments = cursor.u16()?;
            for _ in 0..segments {
                let _name = cursor.string()?;
                let _segment_type = cursor.i32()?;
                let _start = cursor.u32()?;
                let _count = cursor.u32()?;
            }
        }
    }

    let leds = cursor.u16()?;
    for _ in 0..leds {
        let _name = cursor.string()?;
        let _value = cursor.u32()?;
    }
    Some(leds as u32)
}

struct Client {
    stream: TcpStream,
    devices: Vec<u32>,
}

impl Client {
    fn connect(host: &str, port: u16) -> Result<Self, String> {
        let mut stream =
            TcpStream::connect((host, port)).map_err(|e| e.to_string())?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_nodelay(true);

        send_packet(
            &mut stream,
            0,
            REQUEST_PROTOCOL_VERSION,
            &PROTOCOL_VERSION.to_le_bytes(),
        )?;
        let (_device, _kind, data) = read_packet(&mut stream)?;
        let server = data
            .get(0..4)
            .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .unwrap_or(0);
        let protocol = server.min(PROTOCOL_VERSION).max(1);

        send_packet(&mut stream, 0, SET_CLIENT_NAME, b"HueBeat\0")?;

        send_packet(&mut stream, 0, REQUEST_CONTROLLER_COUNT, &[])?;
        let (_device, _kind, data) = read_packet(&mut stream)?;
        let count = data
            .get(0..4)
            .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .unwrap_or(0);

        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            send_packet(
                &mut stream,
                index,
                REQUEST_CONTROLLER_DATA,
                &protocol.to_le_bytes(),
            )?;
            let (_device, _kind, data) = read_packet(&mut stream)?;
            devices.push(parse_led_count(&data, protocol).unwrap_or(0));
        }

        // Put every controller in direct/custom mode so per-LED updates stick.
        for index in 0..count {
            send_packet(&mut stream, index, RGBCONTROLLER_SETCUSTOMMODE, &[])?;
        }

        Ok(Self { stream, devices })
    }

    fn update(&mut self, color: [u8; 3]) -> Result<(), String> {
        for (index, leds) in self.devices.iter().enumerate() {
            let count = *leds;
            if count == 0 {
                continue;
            }
            let size = 6 + 4 * count;
            let mut data = Vec::with_capacity(size as usize);
            data.extend_from_slice(&size.to_le_bytes());
            data.extend_from_slice(&(count as u16).to_le_bytes());
            for _ in 0..count {
                data.push(color[0]);
                data.push(color[1]);
                data.push(color[2]);
                data.push(0);
            }
            send_packet(
                &mut self.stream,
                index as u32,
                RGBCONTROLLER_UPDATELEDS,
                &data,
            )?;
        }
        Ok(())
    }
}

fn set_openrgb(engine: &Engine, ok: bool) {
    if let Ok(mut state) = engine.state.lock() {
        state.openrgb_ok = ok;
    }
}

fn settings(engine: &Engine) -> (bool, String, u16) {
    let Ok(settings) = engine.settings.lock() else {
        return (false, "127.0.0.1".to_string(), 6742);
    };
    (
        settings.bool("usar_openrgb"),
        settings.str("openrgb_host"),
        (settings.i64("openrgb_port").clamp(1, 65535)) as u16,
    )
}

pub fn spawn(engine: Arc<Engine>) {
    thread::spawn(move || loop {
        let (enabled, host, port) = settings(&engine);
        if !enabled {
            engine.openrgb_reconnect.store(false, Ordering::Relaxed);
            set_openrgb(&engine, false);
            thread::sleep(Duration::from_secs(2));
            continue;
        }

        engine.openrgb_reconnect.store(false, Ordering::Relaxed);
        match Client::connect(&host, port) {
            Ok(mut client) => {
                set_openrgb(&engine, true);
                let mut last = [0u8; 3];
                let mut checked = Instant::now();
                loop {
                    if engine.openrgb_reconnect.swap(false, Ordering::Relaxed) {
                        break;
                    }
                    if checked.elapsed() >= Duration::from_secs(2) {
                        checked = Instant::now();
                        let (still, new_host, new_port) = settings(&engine);
                        if !still || new_host != host || new_port != port {
                            break;
                        }
                    }

                    let color = engine
                        .openrgb_color
                        .lock()
                        .map(|color| *color)
                        .unwrap_or([0, 0, 0]);
                    if color != last {
                        if client.update(color).is_err() {
                            break;
                        }
                        last = color;
                    }
                    thread::sleep(Duration::from_millis(40));
                }
                set_openrgb(&engine, false);
                thread::sleep(Duration::from_secs(1));
            }
            Err(_) => {
                set_openrgb(&engine, false);
                thread::sleep(Duration::from_secs(5));
            }
        }
    });
}
