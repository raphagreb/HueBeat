//! Per-application volume through WASAPI session `ISimpleAudioVolume`.
//!
//! The Python engine never had this; the Rust engine exposes it as `vol` /
//! `vol_app` in the state frame and accepts `{"cmd":"volume","value":0..100}`.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{GUID, Interface, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eRender, AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2,
    IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::Engine;

struct Session {
    volume: ISimpleAudioVolume,
    meter: IAudioMeterInformation,
    exe: String,
}

fn process_exe(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        result.ok()?;
        let path = String::from_utf16_lossy(&buffer[..size as usize]);
        Path::new(&path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
    }
}

fn collect_sessions() -> Result<(Vec<Session>, usize), String> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| e.to_string())?;
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .map_err(|e| e.to_string())?;
    let device_count = unsafe { devices.GetCount() }.map_err(|e| e.to_string())?;

    let own_pid = std::process::id();
    let mut raw_count = 0usize;
    let mut list = Vec::new();
    for device_index in 0..device_count {
        let Ok(device) = (unsafe { devices.Item(device_index) }) else {
            continue;
        };
        let Ok(manager) = (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) })
        else {
            continue;
        };
        let Ok(sessions) = (unsafe { manager.GetSessionEnumerator() }) else {
            continue;
        };
        let Ok(count) = (unsafe { sessions.GetCount() }) else {
            continue;
        };
        raw_count += count as usize;

        for index in 0..count {
            let Ok(control) = (unsafe { sessions.GetSession(index) }) else {
                continue;
            };
            let Ok(state) = (unsafe { control.GetState() }) else {
                continue;
            };
            if state != AudioSessionStateActive {
                continue;
            }
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            let Ok(pid) = (unsafe { control2.GetProcessId() }) else {
                continue;
            };
            if pid == 0 || pid == own_pid {
                continue;
            }
            let Some(exe) = process_exe(pid) else {
                continue;
            };
            let lower = exe.to_lowercase();
            if lower.contains("audiodg") || lower == "system" || lower == "svchost" {
                continue;
            }
            let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
                continue;
            };
            let Ok(meter) = control.cast::<IAudioMeterInformation>() else {
                continue;
            };
            list.push(Session {
                volume,
                meter,
                exe,
            });
        }
    }
    Ok((list, raw_count))
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Match the SMTC app id to a session, falling back to the loudest session.
fn choose(sessions: &[Session], aumid: &str) -> Option<usize> {
    let aumid_norm = normalize(aumid);
    let mut best: Option<(u32, usize)> = None;
    if !aumid_norm.is_empty() {
        for (index, session) in sessions.iter().enumerate() {
            let exe = normalize(&session.exe);
            if exe.is_empty() {
                continue;
            }
            let mut score = 0;
            if aumid_norm == exe {
                score = 100;
            } else if aumid_norm.contains(&exe) || exe.contains(&aumid_norm) {
                score = 80;
            } else if (aumid_norm.contains("zunemusic") || aumid_norm.contains("mediaplayer"))
                && (exe.contains("music") || exe.contains("media"))
            {
                score = 60;
            } else if aumid_norm.contains("spotify") && exe.contains("spotify") {
                score = 60;
            }
            if score > 0 && best.map(|(top, _)| score > top).unwrap_or(true) {
                best = Some((score, index));
            }
        }
    }
    if let Some((_, index)) = best {
        return Some(index);
    }

    let mut loudest: Option<(f32, usize)> = None;
    for (index, session) in sessions.iter().enumerate() {
        let peak = unsafe { session.meter.GetPeakValue() }.unwrap_or(0.0);
        if loudest.map(|(top, _)| peak > top).unwrap_or(true) {
            loudest = Some((peak, index));
        }
    }
    loudest.map(|(_, index)| index)
}

pub fn spawn(engine: Arc<Engine>) {
    thread::spawn(move || {
        let _ = wasapi::initialize_mta();
        let trace = std::env::var_os("HUEBEAT_TRACE").is_some();
        let mut last_seen = String::new();
        loop {
            let pending = engine
                .vol_pending
                .lock()
                .ok()
                .and_then(|mut pending| pending.take());
            let aumid = engine
                .current_aumid
                .lock()
                .map(|aumid| aumid.clone())
                .unwrap_or_default();

            let Ok((sessions, raw)) = collect_sessions() else {
                if trace {
                    eprintln!("[vol] could not enumerate audio sessions");
                }
                thread::sleep(Duration::from_millis(500));
                continue;
            };
            if trace {
                let seen = format!(
                    "{raw}|{}",
                    sessions
                        .iter()
                        .map(|session| session.exe.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                );
                if seen != last_seen {
                    eprintln!(
                        "[vol] raw={raw} eligible=[{}] aumid={aumid:?}",
                        sessions
                            .iter()
                            .map(|session| session.exe.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    last_seen = seen;
                }
            }

            match choose(&sessions, &aumid) {
                Some(index) => {
                    let session = &sessions[index];
                    if let Some(value) = pending {
                        let level = value.min(100) as f32 / 100.0;
                        unsafe {
                            let _ = session.volume.SetMasterVolume(level, &GUID::zeroed());
                        }
                        engine.vol.store(value.min(100), Ordering::Relaxed);
                    } else {
                        let held = engine
                            .vol_hold_until
                            .lock()
                            .ok()
                            .and_then(|hold| *hold)
                            .map(|until| Instant::now() < until)
                            .unwrap_or(false);
                        if !held {
                            if let Ok(level) = unsafe { session.volume.GetMasterVolume() } {
                                let value = (level.clamp(0.0, 1.0) * 100.0).round() as u32;
                                engine.vol.store(value, Ordering::Relaxed);
                            }
                        }
                    }
                    if let Ok(mut name) = engine.vol_app.lock() {
                        *name = session.exe.clone();
                    }
                }
                None => {
                    if let Some(value) = pending {
                        // No player yet: keep the value so it applies when one appears.
                        if let Ok(mut pending) = engine.vol_pending.lock() {
                            *pending = Some(value);
                        }
                        engine.vol.store(value, Ordering::Relaxed);
                    }
                    if let Ok(mut name) = engine.vol_app.lock() {
                        name.clear();
                    }
                }
            }

            thread::sleep(Duration::from_millis(300));
        }
    });
}
