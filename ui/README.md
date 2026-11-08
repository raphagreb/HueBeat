# HueBeat 2.0 (Tauri UI)

The window, tray icon and all controls live in Tauri + a web frontend. The
original Python engine (media, beat detection, serial, OpenRGB) still runs,
headless, as a sidecar process.

```
backend_sidecar.py  <- JSON over stdin/stdout ->  src-tauri (Rust)  ->  WebView2 UI
```

## Layout

- `src/` — the new UI (`index.html`, `styles.css`, `main.js`)
- `src-tauri/` — the Rust shell: spawns the backend, tray, window control,
  single-instance guard, JSON lines to the frontend
- `src-tauri/binaries/huebeat-backend.exe` — PyInstaller build of the backend
  (created by `../build_tauri.ps1`)

## Protocol

The backend writes one JSON object per line:

- `{"type":"state", ...}` at 20 fps — playback, accent color, 64 spectrum
  bands, beat flash, connection status; the album cover is included only when
  it changes.
- `{"type":"settings", ...}` — saved values, available serial ports and audio
  outputs.
- `{"type":"settings_saved", ok, error}`.

The UI sends commands through `backend_send`:

```json
{"cmd":"playpause"}
{"cmd":"prev"} / {"cmd":"next"}
{"cmd":"seek","value":42}
{"cmd":"beat_toggle"} / {"cmd":"beat_set","value":true}
{"cmd":"settings_get"} / {"cmd":"settings_set","values":{...}} / {"cmd":"settings_reset"}
{"cmd":"quit"}
```

## Development

```powershell
.\run_tauri_dev.ps1
```

Rust must be on PATH (`%USERPROFILE%\.cargo\bin`). In dev the Rust shell runs
`python -u ../backend_sidecar.py`, so Python dependencies must be installed.

## Release

```powershell
.\build_tauri.ps1
```

This builds the sidecar, then the NSIS installer. The installer is written to
`src-tauri/target/release/bundle/nsis`.

## Notes

- Global hotkeys and the media/beat logic live in the Python sidecar so they
  keep working while the window is hidden. The bundled backend must include
  the `keyboard` package (the spec lists it as a hidden import) or the hotkeys
  are silently disabled.
- The tray menu is provided by Tauri.
- Settings live in `%APPDATA%\HueBeat\settings.json`.
- The sidecar's console output is appended to
  `%APPDATA%\HueBeat\backend.log` (truncated when it passes 4 MB).
- `HUEBEAT_BACKEND` can point at an alternate backend executable (handy for
  testing a different PyInstaller build).
