mod engine;
mod hotkeys;

use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_global_shortcut::ShortcutState;

use engine::{ControlMsg, Engine, HotkeyAction};

struct UiState {
    start_minimized: AtomicBool,
    close_to_tray: AtomicBool,
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("HueBeat").join("settings.json"))
}

fn setting_bool(key: &str, default: bool) -> bool {
    let Some(path) = settings_path() else {
        return default;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return default;
    };
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    json.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn backend_write(app: &AppHandle, value: Value) {
    if let Some(engine) = app.try_state::<Arc<Engine>>() {
        engine.handle_line(&value.to_string());
    }
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Sends one command line to the engine.
#[tauri::command]
fn backend_send(engine: State<'_, Arc<Engine>>, line: String) -> Result<(), String> {
    let _: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    engine.handle_line(&line);
    Ok(())
}

#[tauri::command]
fn ui_ready(
    window: tauri::WebviewWindow,
    state: State<'_, UiState>,
    engine: State<'_, Arc<Engine>>,
) {
    if !state.start_minimized.load(Ordering::Relaxed) {
        let _ = window.show();
        let _ = window.set_focus();
    }
    engine.announce();
}

#[tauri::command]
fn window_minimize(window: tauri::WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn window_hide(window: tauri::WebviewWindow) {
    let _ = window.hide();
}

#[tauri::command]
fn window_close(window: tauri::WebviewWindow, state: State<'_, UiState>) {
    if state.close_to_tray.load(Ordering::Relaxed) {
        let _ = window.hide();
    } else {
        window.app_handle().exit(0);
    }
}

#[tauri::command]
fn set_close_to_tray(state: State<'_, UiState>, value: bool) {
    state.close_to_tray.store(value, Ordering::Relaxed);
}

#[tauri::command]
fn set_start_minimized(state: State<'_, UiState>, value: bool) {
    state.start_minimized.store(value, Ordering::Relaxed);
}

#[tauri::command]
fn app_quit(app: AppHandle) {
    app.exit(0);
}

fn handle_shortcut(app: &AppHandle, shortcut: &tauri_plugin_global_shortcut::Shortcut) {
    let Some(engine) = app.try_state::<Arc<Engine>>() else {
        return;
    };
    let action = engine.hotkeys.lock().ok().and_then(|map| {
        map.iter()
            .find(|(id, _)| *id == shortcut.id())
            .map(|(_, action)| *action)
    });
    match action {
        Some(HotkeyAction::Beat) => engine.toggle_beat.store(true, Ordering::Relaxed),
        Some(HotkeyAction::PlayPause) => engine.send_control(ControlMsg::PlayPause),
        Some(HotkeyAction::Next) => engine.send_control(ControlMsg::Next),
        Some(HotkeyAction::Previous) => engine.send_control(ControlMsg::Previous),
        None => {}
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let start_minimized = setting_bool("iniciar_minimizado", false);
    let close_to_tray = setting_bool("fechar_para_bandeja", true);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        handle_shortcut(app, shortcut);
                    }
                })
                .build(),
        )
        .manage(UiState {
            start_minimized: AtomicBool::new(start_minimized),
            close_to_tray: AtomicBool::new(close_to_tray),
        })
        .invoke_handler(tauri::generate_handler![
            backend_send,
            ui_ready,
            window_minimize,
            window_hide,
            window_close,
            set_close_to_tray,
            set_start_minimized,
            app_quit
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let engine = engine::start(handle.clone());
            hotkeys::reload(&engine, &handle);
            app.manage(engine);

            // Safety net: if the webview never calls ui_ready (JS error), still
            // show the window so the problem is visible instead of silent.
            let guard = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(4));
                if start_minimized {
                    return;
                }
                if let Some(window) = guard.get_webview_window("main") {
                    if !window.is_visible().unwrap_or(false) {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            });

            let show = MenuItem::with_id(app, "show", "Show HueBeat", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "Hide window", true, None::<&str>)?;
            let play = MenuItem::with_id(app, "playpause", "Play / Pause", true, None::<&str>)?;
            let prev = MenuItem::with_id(app, "prev", "Previous track", true, None::<&str>)?;
            let next = MenuItem::with_id(app, "next", "Next track", true, None::<&str>)?;
            let beat = MenuItem::with_id(app, "beat", "Toggle beat", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Settings...", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit HueBeat", true, None::<&str>)?;
            let gap1 = PredefinedMenuItem::separator(app)?;
            let gap2 = PredefinedMenuItem::separator(app)?;
            let gap3 = PredefinedMenuItem::separator(app)?;

            let menu = Menu::with_items(
                app,
                &[
                    &show, &hide, &gap1, &play, &prev, &next, &gap2, &beat, &settings, &gap3, &quit,
                ],
            )?;

            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or("missing default window icon")?;

            TrayIconBuilder::with_id("main")
                .icon(icon)
                .tooltip("HueBeat")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "hide" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.hide();
                        }
                    }
                    "playpause" => backend_write(app, serde_json::json!({"cmd": "playpause"})),
                    "prev" => backend_write(app, serde_json::json!({"cmd": "prev"})),
                    "next" => backend_write(app, serde_json::json!({"cmd": "next"})),
                    "beat" => backend_write(app, serde_json::json!({"cmd": "beat_toggle"})),
                    "settings" => {
                        show_main(app);
                        let _ = app.emit("open-settings", ());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let close_to_tray = window
                    .app_handle()
                    .state::<UiState>()
                    .close_to_tray
                    .load(Ordering::Relaxed);
                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    window.app_handle().exit(0);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building HueBeat");

    app.run(|_, _| {});
}
