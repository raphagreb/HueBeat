//! Global shortcut registration and reload (settings can change the keys).

use std::sync::Arc;

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use crate::engine::{Engine, HotkeyAction};

pub fn reload(engine: &Arc<Engine>, app: &AppHandle) {
    let manager = app.global_shortcut();
    let _ = manager.unregister_all();

    let entries = {
        let Ok(settings) = engine.settings.lock() else {
            return;
        };
        [
            (settings.str("tecla_batida"), HotkeyAction::Beat),
            (settings.str("tecla_play"), HotkeyAction::PlayPause),
            (settings.str("tecla_prox"), HotkeyAction::Next),
            (settings.str("tecla_ant"), HotkeyAction::Previous),
        ]
    };

    let mut registered = Vec::new();
    for (text, action) in entries {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let Ok(shortcut) = text.parse::<Shortcut>() else {
            continue;
        };
        if manager.register(shortcut).is_ok() {
            registered.push((shortcut.id(), action));
        }
    }

    if let Ok(mut map) = engine.hotkeys.lock() {
        *map = registered;
    }
}
