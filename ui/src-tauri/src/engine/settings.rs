//! Persistent settings, mirroring the old Python `settings.py`.
//!
//! The file lives in `%APPDATA%\HueBeat\settings.json` so the installed app
//! keeps working even when its own folder is read-only.

use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// Keys exposed to the settings dialog (same list as the Python sidecar).
pub const SETTINGS_KEYS: &[&str] = &[
    "porta",
    "dispositivo_audio",
    "usar_openrgb",
    "openrgb_host",
    "openrgb_port",
    "pausa_brilho_pc",
    "suavidade",
    "gamma_pc",
    "sensibilidade",
    "batida_min_pc",
    "batida_decaimento",
    "iniciar_minimizado",
    "fechar_para_bandeja",
    "batida_inicial",
    "tecla_batida",
    "tecla_play",
    "tecla_prox",
    "tecla_ant",
];

pub fn defaults() -> Map<String, Value> {
    let value = json!({
        // Arduino
        "porta": "",
        "baud": 115200,
        // OpenRGB
        "usar_openrgb": true,
        "openrgb_host": "127.0.0.1",
        "openrgb_port": 6742,
        // Album colour
        "cor_padrao": [255, 120, 0],
        "sat_min": 0.85,
        "realce_sat": 1.4,
        // Brightness / smoothing
        "pausa_brilho_pc": 0.15,
        "suavidade": 0.12,
        "gamma_pc": 2.2,
        "cal_pc": [1.0, 1.0, 1.0],
        // Beat
        "batida_inicial": true,
        "batida_min_pc": 0.25,
        "batida_decaimento": 0.12,
        "graves_hz": [40, 150],
        "sensibilidade": 1.2,
        "piso": 0.003,
        "intervalo_min": 0.22,
        "dispositivo_audio": "",
        // Hotkeys
        "tecla_batida": "ctrl+alt+b",
        "tecla_play": "ctrl+alt+space",
        "tecla_prox": "ctrl+alt+right",
        "tecla_ant": "ctrl+alt+left",
        // Window / tray
        "iniciar_minimizado": false,
        "fechar_para_bandeja": true,
    });
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

pub struct Settings {
    pub path: PathBuf,
    pub data: Map<String, Value>,
}

pub struct ApplyOutcome {
    pub ok: bool,
    pub error: String,
    pub reconnect_arduino: bool,
    pub reconnect_audio: bool,
    pub reconnect_openrgb: bool,
    pub reload_hotkeys: bool,
}

fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("HueBeat")
}

impl Settings {
    pub fn load() -> Self {
        let path = config_dir().join("settings.json");
        let mut data = defaults();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(Value::Object(saved)) = serde_json::from_str::<Value>(&text) {
                for key in data.keys().cloned().collect::<Vec<_>>() {
                    if let Some(value) = saved.get(&key) {
                        data.insert(key, value.clone());
                    }
                }
            }
        }
        Self { path, data }
    }

    pub fn save(&self) -> bool {
        let Some(dir) = self.path.parent() else {
            return false;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        let Ok(text) = serde_json::to_string_pretty(&Value::Object(self.data.clone())) else {
            return false;
        };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_err() {
            return false;
        }
        std::fs::rename(&tmp, &self.path).is_ok()
    }

    pub fn reset(&mut self) {
        self.data = defaults();
        let _ = self.save();
    }

    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    #[allow(dead_code)]
    pub fn set(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_string(), value);
    }

    pub fn str(&self, key: &str) -> String {
        self.data
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    pub fn bool(&self, key: &str) -> bool {
        self.data
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub fn f64(&self, key: &str) -> f64 {
        self.data
            .get(key)
            .and_then(Value::as_f64)
            .unwrap_or_else(|| {
                defaults()
                    .get(key)
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
            })
    }

    pub fn f32(&self, key: &str) -> f32 {
        self.f64(key) as f32
    }

    pub fn i64(&self, key: &str) -> i64 {
        self.data
            .get(key)
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                defaults()
                    .get(key)
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            })
    }

    pub fn numbers(&self, key: &str) -> Vec<f64> {
        self.data
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default()
    }

    /// Snapshot of the keys exposed to the UI (same shape as the sidecar).
    pub fn snapshot_values(&self) -> Value {
        let mut values = Map::new();
        for key in SETTINGS_KEYS {
            if let Some(value) = self.data.get(*key) {
                values.insert((*key).to_string(), value.clone());
            }
        }
        Value::Object(values)
    }

    /// Validate and merge values coming from the settings dialog.
    pub fn apply(&mut self, incoming: &Map<String, Value>) -> ApplyOutcome {
        let before: Vec<(&str, Value)> = SETTINGS_KEYS
            .iter()
            .map(|key| (*key, self.data.get(*key).cloned().unwrap_or(Value::Null)))
            .collect();

        let mut cleaned: Vec<(String, Value)> = Vec::new();
        for key in SETTINGS_KEYS {
            let Some(value) = incoming.get(*key) else {
                continue;
            };
            let parsed = match *key {
                "usar_openrgb" | "iniciar_minimizado" | "fechar_para_bandeja"
                | "batida_inicial" => Value::Bool(value.as_bool().unwrap_or_else(|| {
                    matches!(value, Value::Number(n) if n.as_f64().unwrap_or(0.0) != 0.0)
                })),
                "openrgb_port" => {
                    let port = value
                        .as_i64()
                        .or_else(|| {
                            value
                                .as_str()
                                .and_then(|text| text.trim().parse::<i64>().ok())
                        })
                        .unwrap_or(-1);
                    if !(1..=65535).contains(&port) {
                        return ApplyOutcome::error(format!("Invalid value for {key}."));
                    }
                    Value::from(port)
                }
                "pausa_brilho_pc" | "suavidade" | "gamma_pc" | "sensibilidade"
                | "batida_min_pc" | "batida_decaimento" => {
                    let Some(number) = value.as_f64() else {
                        return ApplyOutcome::error(format!("Invalid value for {key}."));
                    };
                    let rounded = (number * 1000.0).round() / 1000.0;
                    json!(rounded)
                }
                "porta" => {
                    let text = value.as_str().unwrap_or("").trim().to_string();
                    let lower = text.to_lowercase();
                    if matches!(lower.as_str(), "auto" | "auto-detect" | "default") {
                        Value::String(String::new())
                    } else {
                        Value::String(text)
                    }
                }
                "dispositivo_audio" => {
                    let text = value.as_str().unwrap_or("").trim().to_string();
                    let lower = text.to_lowercase();
                    if matches!(lower.as_str(), "default" | "default output") {
                        Value::String(String::new())
                    } else {
                        Value::String(text)
                    }
                }
                _ => Value::String(value.as_str().unwrap_or("").trim().to_string()),
            };
            cleaned.push(((*key).to_string(), parsed));
        }

        if cleaned
            .iter()
            .any(|(key, value)| key == "openrgb_host" && value.as_str() == Some(""))
        {
            if let Some((_, value)) = cleaned.iter_mut().find(|(key, _)| key == "openrgb_host") {
                *value = Value::String("127.0.0.1".to_string());
            }
        }

        for (key, value) in cleaned {
            self.data.insert(key, value);
        }
        if !self.save() {
            return ApplyOutcome::error("Could not save the settings file.");
        }

        let now = |key: &str| self.data.get(key).cloned().unwrap_or(Value::Null);
        let changed = |key: &str| {
            before
                .iter()
                .find(|(old_key, _)| *old_key == key)
                .map(|(_, old)| old != &now(key))
                .unwrap_or(false)
        };

        ApplyOutcome {
            ok: true,
            error: String::new(),
            reconnect_arduino: changed("porta"),
            reconnect_audio: changed("dispositivo_audio"),
            reconnect_openrgb: changed("usar_openrgb")
                || changed("openrgb_host")
                || changed("openrgb_port"),
            reload_hotkeys: changed("tecla_batida")
                || changed("tecla_play")
                || changed("tecla_prox")
                || changed("tecla_ant"),
        }
    }
}

impl ApplyOutcome {
    fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: message.into(),
            reconnect_arduino: false,
            reconnect_audio: false,
            reconnect_openrgb: false,
            reload_hotkeys: false,
        }
    }
}
