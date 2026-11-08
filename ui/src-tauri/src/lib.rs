use serde_json::Value;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

/// Handle to the headless Python backend (newline-delimited JSON on stdio).
struct Backend {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
}

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

fn log_path() -> PathBuf {
    settings_path()
        .and_then(|p| p.parent().map(|d| d.join("backend.log")))
        .unwrap_or_else(|| PathBuf::from("huebeat-backend.log"))
}

fn backend_command(app: &AppHandle) -> Command {
    if let Ok(explicit) = std::env::var("HUEBEAT_BACKEND") {
        return Command::new(explicit);
    }

    if let Ok(dir) = app.path().resource_dir() {
        let exe = dir.join("huebeat-backend.exe");
        if exe.exists() {
            return Command::new(exe);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sidecar = dir.join("huebeat-backend.exe");
            if sidecar.exists() {
                return Command::new(sidecar);
            }
        }
    }

    // Development fallback: run the Python source next to the ui/ folder.
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("backend_sidecar.py");
    let mut cmd = Command::new("python");
    cmd.arg("-u").arg(script);
    cmd
}

fn spawn_backend(app: &AppHandle) -> Result<Backend, String> {
    let mut cmd = backend_command(app);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or("backend has no stdout")?;
    let stderr = child.stderr.take().ok_or("backend has no stderr")?;

    let app_out = app.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(text) => {
                    let _ = app_out.emit("backend-line", text);
                }
                Err(_) => break,
            }
        }
        let _ = app_out.emit("backend-closed", ());
    });

    let log = log_path();
    // Keep the log from growing forever across sessions.
    if let Ok(meta) = std::fs::metadata(&log) {
        if meta.len() > 4 * 1024 * 1024 {
            let _ = std::fs::remove_file(&log);
        }
    }
    std::thread::spawn(move || {
        let mut file = OpenOptions::new().create(true).append(true).open(log).ok();
        for line in BufReader::new(stderr).lines().flatten() {
            if let Some(f) = file.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    });

    Ok(Backend {
        stdin: Mutex::new(stdin),
        child: Mutex::new(Some(child)),
    })
}

fn backend_write(app: &AppHandle, value: Value) {
    let Some(state) = app.try_state::<Backend>() else {
        return;
    };
    let Ok(mut guard) = state.stdin.lock() else {
        return;
    };
    if let Some(stdin) = guard.as_mut() {
        let _ = writeln!(stdin, "{value}");
        let _ = stdin.flush();
    }
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn backend_send(state: State<'_, Backend>, line: String) -> Result<(), String> {
    let _: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    let mut guard = state.stdin.lock().map_err(|_| "backend lock poisoned")?;
    let stdin = guard.as_mut().ok_or("backend is not running")?;
    writeln!(stdin, "{line}").map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())
}

#[tauri::command]
fn ui_ready(window: tauri::WebviewWindow, state: State<'_, UiState>) {
    if !state.start_minimized.load(Ordering::Relaxed) {
        let _ = window.show();
        let _ = window.set_focus();
    }
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let start_minimized = setting_bool("iniciar_minimizado", false);
    let close_to_tray = setting_bool("fechar_para_bandeja", true);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
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

            match spawn_backend(&handle) {
                Ok(backend) => {
                    app.manage(backend);
                }
                Err(err) => {
                    eprintln!("could not start backend: {err}");
                    let _ = handle.emit("backend-error", err);
                }
            }

            // Safety net: if the webview never calls ui_ready (JS error), still
            // show the window so the problem is visible instead of silent.
            let guard = app.handle().clone();
            let start_min = start_minimized;
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(4));
                if start_min {
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

    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = app_handle.try_state::<Backend>() {
                if let Ok(mut guard) = state.child.lock() {
                    if let Some(child) = guard.as_mut() {
                        let _ = child.kill();
                    }
                }
            }
        }
    });
}
