//! Native Rust engine: audio capture, media sessions, serial, OpenRGB, hotkeys.
//!
//! It speaks exactly the same newline-delimited JSON as the old Python
//! sidecar, so the webview (`main.js`) does not need to know which engine is
//! running.

pub mod artwork;
pub mod audio;
pub mod media;
pub mod openrgb;
pub mod render;
pub mod serial;
pub mod settings;
pub mod text;
pub mod volume;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use tauri::{AppHandle, Emitter};

use audio::BeatDetector;
use serial::SerialLink;
use settings::Settings;

pub const VERSION: &str = "2.0.0";
const FPS: u64 = 20;

#[derive(Clone, Copy, Debug)]
pub enum ControlMsg {
    PlayPause,
    Next,
    Previous,
    Seek(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    Beat,
    PlayPause,
    Next,
    Previous,
}

/// Everything the UI sees in a `state` frame.
#[derive(Clone)]
pub struct EngineState {
    pub tocando: bool,
    pub titulo: String,
    pub artista: String,
    pub pos: u64,
    pub dur: u64,
    pub cor_alvo: [u8; 3],
    pub flash: f32,
    pub arduino_ok: bool,
    pub openrgb_ok: bool,
    pub porta: String,
    pub batida_ativa: bool,
    pub batida_disponivel: bool,
    pub viu_musica: bool,
    pub capa_seq: u64,
    pub capa_url: Option<String>,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            tocando: false,
            titulo: String::new(),
            artista: String::new(),
            pos: 0,
            dur: 0,
            cor_alvo: [255, 120, 0],
            flash: 0.0,
            arduino_ok: false,
            openrgb_ok: false,
            porta: String::new(),
            batida_ativa: true,
            batida_disponivel: true,
            viu_musica: false,
            capa_seq: 0,
            capa_url: None,
        }
    }
}

pub struct Engine {
    pub app: AppHandle,
    pub settings: Mutex<Settings>,
    pub state: Mutex<EngineState>,
    pub serial: SerialLink,
    pub detector: BeatDetector,
    pub spectrum: Mutex<[f32; 64]>,
    pub audio_outputs: Mutex<Vec<String>>,
    pub current_aumid: Mutex<String>,

    control_tx: Mutex<Sender<ControlMsg>>,
    pub control_rx: Mutex<Option<Receiver<ControlMsg>>>,

    /// Set by the UI, the tray or a global hotkey; consumed by the render loop.
    pub toggle_beat: AtomicBool,

    /// Colour last prepared for OpenRGB (already gamma/calibration corrected).
    pub openrgb_color: Mutex<[u8; 3]>,
    pub openrgb_reconnect: AtomicBool,

    /// Per-app volume (0-100).
    pub vol: AtomicU32,
    pub vol_app: Mutex<String>,
    pub vol_pending: Mutex<Option<u32>>,
    /// Ignore polled values briefly after a slider change so it does not jump.
    pub vol_hold_until: Mutex<Option<Instant>>,

    /// Shortcut id -> action, used by the global shortcut handler.
    pub hotkeys: Mutex<Vec<(u32, HotkeyAction)>>,

    self_ref: OnceLock<Weak<Engine>>,
}

impl Engine {
    pub fn new(app: AppHandle) -> Arc<Self> {
        let settings = Settings::load();
        let color = {
            let values = settings.numbers("cor_padrao");
            if values.len() >= 3 {
                [
                    values[0].clamp(0.0, 255.0) as u8,
                    values[1].clamp(0.0, 255.0) as u8,
                    values[2].clamp(0.0, 255.0) as u8,
                ]
            } else {
                [255, 120, 0]
            }
        };
        let beat_start = settings.bool("batida_inicial");

        let state = EngineState {
            cor_alvo: color,
            batida_ativa: beat_start,
            ..EngineState::default()
        };
        let (tx, rx) = mpsc::channel();

        let engine = Arc::new(Self {
            app,
            settings: Mutex::new(settings),
            state: Mutex::new(state),
            serial: SerialLink::default(),
            detector: BeatDetector::default(),
            spectrum: Mutex::new([0.0; 64]),
            audio_outputs: Mutex::new(Vec::new()),
            current_aumid: Mutex::new(String::new()),
            control_tx: Mutex::new(tx),
            control_rx: Mutex::new(Some(rx)),
            toggle_beat: AtomicBool::new(false),
            openrgb_color: Mutex::new(color),
            openrgb_reconnect: AtomicBool::new(false),
            vol: AtomicU32::new(100),
            vol_app: Mutex::new(String::new()),
            vol_pending: Mutex::new(None),
            vol_hold_until: Mutex::new(None),
            hotkeys: Mutex::new(Vec::new()),
            self_ref: OnceLock::new(),
        });
        let _ = engine.self_ref.set(Arc::downgrade(&engine));
        engine
    }

    pub fn send_control(&self, message: ControlMsg) {
        if let Ok(sender) = self.control_tx.lock() {
            let _ = sender.send(message);
        }
    }

    pub fn emit_json(&self, value: Value) {
        if std::env::var_os("HUEBEAT_TRACE").is_some() {
            eprintln!("[huebeat] {value}");
        }
        let _ = self.app.emit("backend-line", value.to_string());
    }

    pub fn announce(&self) {
        self.emit_json(json!({ "type": "hello", "version": VERSION }));
        self.emit_settings();
    }

    pub fn emit_settings(&self) {
        let (values, defaults) = {
            let Ok(settings) = self.settings.lock() else {
                return;
            };
            (
                settings.snapshot_values(),
                Value::Object(settings::defaults()),
            )
        };
        let ports: Vec<String> = serialport::available_ports()
            .map(|list| list.into_iter().map(|info| info.port_name).collect())
            .unwrap_or_default();
        let audio_outputs = self
            .audio_outputs
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default();
        self.emit_json(json!({
            "type": "settings",
            "values": values,
            "ports": ports,
            "audio_outputs": audio_outputs,
            "defaults": defaults,
        }));
    }

    pub fn reload_hotkeys(&self) {
        let Some(weak) = self.self_ref.get() else {
            return;
        };
        let Some(engine) = weak.upgrade() else {
            return;
        };
        let app = self.app.clone();
        let _ = self.app.run_on_main_thread(move || {
            crate::hotkeys::reload(&engine, &app);
        });
    }

    fn set_beat(&self, value: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.batida_ativa = value;
        }
    }

    fn apply_settings(&self, incoming: &Map<String, Value>) {
        let outcome = {
            let Ok(mut settings) = self.settings.lock() else {
                return;
            };
            settings.apply(incoming)
        };
        if outcome.reconnect_arduino {
            self.serial.request_reconnect();
        }
        if outcome.reconnect_audio {
            self.detector.reiniciar.store(true, Ordering::Relaxed);
        }
        if outcome.reconnect_openrgb {
            self.openrgb_reconnect.store(true, Ordering::Relaxed);
        }
        if outcome.reload_hotkeys {
            self.reload_hotkeys();
        }
        self.emit_json(json!({
            "type": "settings_saved",
            "ok": outcome.ok,
            "error": outcome.error,
        }));
        self.emit_settings();
    }

    fn reset_settings(&self) {
        if let Ok(mut settings) = self.settings.lock() {
            settings.reset();
        }
        self.serial.request_reconnect();
        self.detector.reiniciar.store(true, Ordering::Relaxed);
        self.openrgb_reconnect.store(true, Ordering::Relaxed);
        self.reload_hotkeys();
        self.emit_json(json!({ "type": "settings_saved", "ok": true, "error": "" }));
        self.emit_settings();
    }

    /// Handles one JSON command line from the webview or the tray.
    pub fn handle_line(&self, line: &str) {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            return;
        };
        let Some(object) = value.as_object() else {
            return;
        };
        let Some(cmd) = object.get("cmd").and_then(Value::as_str) else {
            return;
        };

        match cmd {
            "playpause" => self.send_control(ControlMsg::PlayPause),
            "next" => self.send_control(ControlMsg::Next),
            "prev" => self.send_control(ControlMsg::Previous),
            "seek" => {
                let target = object
                    .get("value")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .max(0.0) as u64;
                self.send_control(ControlMsg::Seek(target));
            }
            "beat_toggle" => {
                self.toggle_beat.store(true, Ordering::Relaxed);
            }
            "beat_set" => {
                let desired = object
                    .get("value")
                    .map(|value| match value {
                        Value::Bool(flag) => *flag,
                        Value::Number(number) => number.as_f64().unwrap_or(0.0) != 0.0,
                        _ => false,
                    })
                    .unwrap_or(false);
                let current = self
                    .state
                    .lock()
                    .map(|state| state.batida_ativa)
                    .unwrap_or(false);
                self.set_beat(desired);
                if desired != current {
                    self.toggle_beat.store(true, Ordering::Relaxed);
                }
            }
            "volume" => {
                let value = object
                    .get("value")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .clamp(0.0, 100.0)
                    .round() as u32;
                self.vol.store(value, Ordering::Relaxed);
                if let Ok(mut pending) = self.vol_pending.lock() {
                    *pending = Some(value);
                }
                if let Ok(mut hold) = self.vol_hold_until.lock() {
                    *hold = Some(Instant::now() + Duration::from_millis(700));
                }
            }
            "settings_get" => self.emit_settings(),
            "settings_set" => {
                let incoming = object
                    .get("values")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                self.apply_settings(&incoming);
            }
            "settings_reset" => self.reset_settings(),
            "reconnect" => match object.get("target").and_then(Value::as_str) {
                Some("arduino") => self.serial.request_reconnect(),
                Some("audio") => self.detector.reiniciar.store(true, Ordering::Relaxed),
                Some("openrgb") => self.openrgb_reconnect.store(true, Ordering::Relaxed),
                _ => {}
            },
            "ping" => self.emit_json(json!({ "type": "pong" })),
            _ => {}
        }
    }
}

fn emitter_loop(engine: Arc<Engine>) {
    let mut last_cover: i64 = -1;
    loop {
        let (mut frame, seq, cover) = {
            let Ok(state) = engine.state.lock() else {
                thread::sleep(Duration::from_millis(50));
                continue;
            };
            let spectrum = engine
                .spectrum
                .lock()
                .map(|values| {
                    values
                        .iter()
                        .map(|value| (value * 1000.0).round() / 1000.0)
                        .collect::<Vec<f32>>()
                })
                .unwrap_or_else(|_| vec![0.0; 64]);
            let vol = engine.vol.load(Ordering::Relaxed);
            let vol_app = engine
                .vol_app
                .lock()
                .map(|name| name.clone())
                .unwrap_or_default();
            let frame = json!({
                "type": "state",
                "tocando": state.tocando,
                "titulo": state.titulo,
                "artista": state.artista,
                "pos": state.pos,
                "dur": state.dur,
                "cor": [
                    state.cor_alvo[0] as i64,
                    state.cor_alvo[1] as i64,
                    state.cor_alvo[2] as i64,
                ],
                "flash": ((state.flash * 10_000.0).round() / 10_000.0),
                "arduino_ok": state.arduino_ok,
                "openrgb_ok": state.openrgb_ok,
                "porta": state.porta,
                "batida_ativa": state.batida_ativa,
                "batida_disponivel": state.batida_disponivel,
                "viu_musica": state.viu_musica,
                "capa_seq": state.capa_seq,
                "espectro": spectrum,
                "vol": vol,
                "vol_app": vol_app,
            });
            (frame, state.capa_seq as i64, state.capa_url.clone())
        };

        if seq != last_cover {
            last_cover = seq;
            if let Some(object) = frame.as_object_mut() {
                object.insert(
                    "capa".to_string(),
                    match cover {
                        Some(url) => Value::String(url),
                        None => Value::Null,
                    },
                );
            }
        }

        engine.emit_json(frame);
        thread::sleep(Duration::from_millis(1000 / FPS));
    }
}

/// Starts every engine thread. Call once, from Tauri's `setup`.
pub fn start(app: AppHandle) -> Arc<Engine> {
    let engine = Engine::new(app);

    serial::spawn(engine.clone());
    audio::spawn(engine.clone());
    media::spawn(engine.clone());
    render::spawn(engine.clone());
    openrgb::spawn(engine.clone());
    volume::spawn(engine.clone());
    render::spawn_clock(engine.clone());

    let emitter_engine = engine.clone();
    thread::spawn(move || emitter_loop(emitter_engine));

    engine
}
