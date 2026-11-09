# HueBeat 2.0 (Tauri UI)

The window and controls are Tauri + a web frontend. The engine is native Rust
and runs in-process: media (SMTC), WASAPI loopback spectrum and beat detection,
Arduino serial, OpenRGB and per-app player volume. There is no Python sidecar.

```
WebView2 UI  <->  Tauri commands/events  <->  Rust engine (src-tauri/src/engine)
```

## Layout

- `src/` — the UI (`index.html`, `styles.css`, `main.js`, `assets/`)
- `src-tauri/` — the Rust shell and engine
  - `src/lib.rs` — Tauri setup, tray, hotkeys, window commands, single instance
  - `src/engine/` — audio, media, serial, OpenRGB, artwork, render and volume
  - `src/hotkeys.rs` — global shortcut registration

## Protocol

The engine emits one JSON object per line through the `backend-line` Tauri
event:

- `{"type":"hello","version":"2.0.0"}` — sent when the UI is ready
- `{"type":"state",...}` at 20 fps — playback, accent color, 64 spectrum
  bands, beat flash, connection status and player volume; the album cover is
  included only when it changes
- `{"type":"settings",...}` — saved values, defaults, serial ports and audio
  outputs
- `{"type":"settings_saved","ok":true|false,"error":""}`
- `{"type":"pong"}` — reply to `ping`

The UI sends commands through the `backend_send` Tauri command:

```json
{"cmd":"playpause"}
{"cmd":"prev"} / {"cmd":"next"}
{"cmd":"seek","value":42}
{"cmd":"beat_toggle"} / {"cmd":"beat_set","value":true}
{"cmd":"volume","value":0..100}
{"cmd":"settings_get"} / {"cmd":"settings_set","values":{...}} / {"cmd":"settings_reset"}
{"cmd":"reconnect","target":"arduino|audio|openrgb"}
{"cmd":"ping"}
```

## Development

```powershell
.\run_tauri_dev.ps1
```

Rust must be on `PATH` (`%USERPROFILE%\.cargo\bin`).

Set `HUEBEAT_TRACE=1` to print engine frames and volume-session diagnostics to
stderr in debug builds.

## Release

```powershell
.\build_tauri.ps1
```

This builds the NSIS installer in
`ui/src-tauri/target/release/bundle/nsis`.

## Notes

- Settings live in `%APPDATA%\HueBeat\settings.json`.
- The tray menu and global hotkeys are provided by Tauri, so media/beat
  control keeps working while the window is hidden.
- Player volume is applied to the active app's WASAPI session
  (`ISimpleAudioVolume`), detected from the current media session or the
  loudest active session. It is attenuation only (0–100%).
