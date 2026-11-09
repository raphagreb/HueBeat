//! Windows media session (SMTC): track metadata, timeline, seek and controls.

use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as MediaManager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as MediaProperties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use super::artwork;
use super::text;
use super::{ControlMsg, Engine};

const DEFAULT_COLOR: [u8; 3] = [255, 120, 0];

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn filetime_to_unix(value: i64) -> f64 {
    value as f64 / 10_000_000.0 - 11_644_473_600.0
}

fn ticks_to_secs(value: i64) -> f64 {
    value as f64 / 10_000_000.0
}

fn init_winrt() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

fn request_manager() -> MediaManager {
    loop {
        if let Ok(operation) = MediaManager::RequestAsync() {
            if let Ok(manager) = operation.join() {
                return manager;
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn is_playing(session: &Session) -> bool {
    session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .map(|status| status == PlaybackStatus::Playing)
        .unwrap_or(false)
}

fn current_session(manager: &MediaManager) -> Option<Session> {
    if let Ok(session) = manager.GetCurrentSession() {
        return Some(session);
    }
    let sessions = manager.GetSessions().ok()?;
    let count = sessions.Size().unwrap_or(0);
    for index in 0..count {
        if let Ok(session) = sessions.GetAt(index) {
            if is_playing(&session) {
                return Some(session);
            }
        }
    }
    None
}

fn read_properties(session: &Session) -> Option<MediaProperties> {
    session.TryGetMediaPropertiesAsync().ok()?.join().ok()
}

fn raw_titles(props: &MediaProperties) -> (String, String) {
    let title = props
        .Title()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();
    let artist = props
        .Artist()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();
    (title.trim().to_string(), artist.trim().to_string())
}

fn read_thumbnail(props: &MediaProperties) -> Option<Vec<u8>> {
    let thumbnail = props.Thumbnail().ok()?;
    let stream = thumbnail.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > 32 * 1024 * 1024 {
        return None;
    }
    let input = stream.GetInputStreamAt(0).ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    reader.LoadAsync(size as u32).ok()?.join().ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    Some(bytes)
}

fn artwork_for(engine: &Engine, props: &MediaProperties) -> ([u8; 3], Option<String>) {
    let (sat_min, realce, default) = engine
        .settings
        .lock()
        .map(|settings| {
            let mut default = DEFAULT_COLOR;
            let values = settings.numbers("cor_padrao");
            if values.len() >= 3 {
                default = [
                    values[0].clamp(0.0, 255.0) as u8,
                    values[1].clamp(0.0, 255.0) as u8,
                    values[2].clamp(0.0, 255.0) as u8,
                ];
            }
            (
                settings.f32("sat_min"),
                settings.f32("realce_sat"),
                default,
            )
        })
        .unwrap_or((0.85, 1.4, DEFAULT_COLOR));

    let Some(bytes) = read_thumbnail(props) else {
        return (default, None);
    };
    let Some(image) = artwork::decode(&bytes) else {
        return (default, None);
    };
    let color = artwork::dominant_color(&image, sat_min, realce);
    let url = artwork::data_url(&image);
    (color, url)
}

fn position_duration(session: &Session, playing: bool) -> (u64, u64) {
    let Ok(timeline) = session.GetTimelineProperties() else {
        return (0, 0);
    };
    let start = timeline
        .StartTime()
        .map(|value| ticks_to_secs(value.Duration))
        .unwrap_or(0.0);
    let mut end = timeline
        .EndTime()
        .map(|value| ticks_to_secs(value.Duration))
        .unwrap_or(0.0);
    if end <= start {
        end = timeline
            .MaxSeekTime()
            .map(|value| ticks_to_secs(value.Duration))
            .unwrap_or(0.0);
    }
    let duration = end - start;
    if duration <= 0.0 {
        return (0, 0);
    }
    let mut position = timeline
        .Position()
        .map(|value| ticks_to_secs(value.Duration))
        .unwrap_or(0.0)
        - start;
    if playing {
        if let Ok(updated) = timeline.LastUpdatedTime() {
            let delay = now_unix() - filetime_to_unix(updated.UniversalTime);
            if (0.0..=600.0).contains(&delay) {
                position += delay;
            }
        }
    }
    position = position.clamp(0.0, duration);
    (position as u64, duration as u64)
}

fn timeline_stale(session: &Session, reference: Option<f64>) -> bool {
    let Some(reference) = reference else {
        return false;
    };
    let Ok(timeline) = session.GetTimelineProperties() else {
        return false;
    };
    let start = timeline
        .StartTime()
        .map(|value| ticks_to_secs(value.Duration))
        .unwrap_or(0.0);
    let end = timeline
        .EndTime()
        .map(|value| ticks_to_secs(value.Duration))
        .unwrap_or(0.0);
    if end - start <= 0.0 {
        return true;
    }
    timeline
        .LastUpdatedTime()
        .map(|value| filetime_to_unix(value.UniversalTime) < reference - 5.0)
        .unwrap_or(false)
}

fn refresh_timeline(session: &Session) {
    if !is_playing(session) {
        return;
    }
    if let Ok(operation) = session.TryTogglePlayPauseAsync() {
        let _ = operation.join();
    }
    let mut paused = false;
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(100));
        if !is_playing(session) {
            paused = true;
            break;
        }
    }
    if !paused {
        return;
    }
    for _ in 0..5 {
        if let Ok(operation) = session.TryTogglePlayPauseAsync() {
            let _ = operation.join();
        }
        thread::sleep(Duration::from_millis(150));
        if is_playing(session) {
            break;
        }
    }
}

fn send_track(engine: &Engine, title: &str, artist: &str, color: [u8; 3]) {
    let title = text::romanize(title);
    let title = if title.is_empty() {
        "Tocando".to_string()
    } else {
        title
    };
    let line = format!(
        "T,{}\nA,{}\nC,{},{},{}\nI,0\n",
        title,
        text::romanize(artist),
        color[0],
        color[1],
        color[2]
    );
    engine.serial.send(&line);
}

fn send_idle(engine: &Engine, last_sent: &mut Option<bool>, last_time: &mut f64) {
    if *last_sent != Some(false) {
        if engine.serial.send("T,\nA,\nS,0\nP,0,0\nI,1\n") {
            *last_sent = Some(false);
            *last_time = now_unix();
        }
    }
}

fn media_loop(engine: Arc<Engine>) {
    init_winrt();
    text::warm_romanizer();
    let manager = request_manager();

    let mut generation = engine.serial.generation();
    let mut last_track: Option<(String, String)> = None;
    let mut track_started: Option<f64> = None;
    let mut refreshed_track: Option<(String, String)> = None;
    let mut last_status: Option<bool> = None;
    let mut last_status_time = 0.0f64;

    loop {
        let current_generation = engine.serial.generation();
        if current_generation != generation {
            generation = current_generation;
            last_track = None;
            refreshed_track = None;
            last_status = None;
            last_status_time = 0.0;
        }

        let Some(session) = current_session(&manager) else {
            if let Ok(mut state) = engine.state.lock() {
                state.tocando = false;
                state.titulo.clear();
                state.artista.clear();
                state.pos = 0;
                state.dur = 0;
                if state.capa_url.is_some() {
                    state.capa_url = None;
                    state.capa_seq += 1;
                }
            }
            if let Ok(mut aumid) = engine.current_aumid.lock() {
                aumid.clear();
            }
            // Re-send metadata and artwork if the same session comes back.
            last_track = None;
            refreshed_track = None;
            send_idle(&engine, &mut last_status, &mut last_status_time);
            thread::sleep(Duration::from_secs(1));
            continue;
        };

        if let Ok(mut state) = engine.state.lock() {
            state.viu_musica = true;
        }
        if let Ok(aumid) = session.SourceAppUserModelId() {
            if let Ok(mut current) = engine.current_aumid.lock() {
                *current = aumid.to_string_lossy();
            }
        }

        let Some(props) = read_properties(&session) else {
            thread::sleep(Duration::from_secs(1));
            continue;
        };
        let (mut raw_title, mut raw_artist) = raw_titles(&props);
        let mut title = if raw_title.is_empty() {
            "Untitled".to_string()
        } else {
            raw_title.clone()
        };
        let mut artist = raw_artist.clone();
        let playing = is_playing(&session);

        if let Ok(mut state) = engine.state.lock() {
            state.tocando = playing;
            state.titulo = title.clone();
            state.artista = artist.clone();
        }

        let mut track = (raw_title.clone(), raw_artist.clone());
        if last_track.as_ref() != Some(&track) {
            track_started = Some(now_unix());
            refreshed_track = None;

            // Give the player a moment to publish the new cover/timeline.
            thread::sleep(Duration::from_secs(1));
            if let Some(fresh) = read_properties(&session) {
                let (new_title, new_artist) = raw_titles(&fresh);
                raw_title = new_title;
                raw_artist = new_artist;
                title = if raw_title.is_empty() {
                    "Untitled".to_string()
                } else {
                    raw_title.clone()
                };
                artist = raw_artist.clone();
                if let Ok(mut state) = engine.state.lock() {
                    state.titulo = title.clone();
                    state.artista = artist.clone();
                }
                let (color, cover) = artwork_for(&engine, &fresh);
                if let Ok(mut state) = engine.state.lock() {
                    state.cor_alvo = color;
                    state.capa_url = cover;
                    state.capa_seq += 1;
                }
                send_track(&engine, &raw_title, &raw_artist, color);
            }
            track = (raw_title, raw_artist);
            last_track = Some(track.clone());
            last_status = None;
            last_status_time = 0.0;
        }

        if playing
            && refreshed_track.as_ref() != Some(&track)
            && track_started.is_some()
        {
            for _ in 0..2 {
                if !timeline_stale(&session, track_started) {
                    break;
                }
                refresh_timeline(&session);
                thread::sleep(Duration::from_millis(300));
            }
            refreshed_track = Some(track.clone());
        }

        let (pos, dur) = position_duration(&session, playing);
        if let Ok(mut state) = engine.state.lock() {
            state.pos = pos;
            state.dur = dur;
        }
        engine.serial.send(&format!("P,{pos},{dur}\n"));

        let now = now_unix();
        if last_status != Some(playing) || now - last_status_time >= 1.5 {
            let line = if playing { "S,1\n" } else { "S,0\n" };
            if engine.serial.send(line) {
                last_status = Some(playing);
                last_status_time = now;
            }
        }

        thread::sleep(Duration::from_secs(1));
    }
}

fn control_loop(receiver: Receiver<ControlMsg>) {
    init_winrt();
    let manager = request_manager();
    while let Ok(message) = receiver.recv() {
        let Some(session) = current_session(&manager) else {
            continue;
        };
        match message {
            ControlMsg::PlayPause => {
                if let Ok(operation) = session.TryTogglePlayPauseAsync() {
                    let _ = operation.join();
                }
            }
            ControlMsg::Next => {
                if let Ok(operation) = session.TrySkipNextAsync() {
                    let _ = operation.join();
                }
            }
            ControlMsg::Previous => {
                if let Ok(operation) = session.TrySkipPreviousAsync() {
                    let _ = operation.join();
                }
            }
            ControlMsg::Seek(seconds) => {
                let Ok(timeline) = session.GetTimelineProperties() else {
                    continue;
                };
                let start = timeline
                    .StartTime()
                    .map(|value| ticks_to_secs(value.Duration))
                    .unwrap_or(0.0);
                let mut end = timeline
                    .EndTime()
                    .map(|value| ticks_to_secs(value.Duration))
                    .unwrap_or(0.0);
                if end <= start {
                    end = timeline
                        .MaxSeekTime()
                        .map(|value| ticks_to_secs(value.Duration))
                        .unwrap_or(0.0);
                }
                let minimum = timeline
                    .MinSeekTime()
                    .map(|value| ticks_to_secs(value.Duration) - start)
                    .unwrap_or(0.0)
                    .max(0.0);
                let maximum = (end - start).max(0.0);
                let mut target = seconds as f64;
                if maximum > 0.0 {
                    target = target.clamp(minimum, maximum);
                }
                let ticks = ((start + target) * 10_000_000.0).round() as i64;
                if let Ok(operation) = session.TryChangePlaybackPositionAsync(ticks) {
                    let _ = operation.join();
                }
            }
        }
    }
}

pub fn spawn(engine: Arc<Engine>) {
    let media_engine = engine.clone();
    thread::spawn(move || media_loop(media_engine));

    if let Ok(receiver) = control_channel(&engine) {
        thread::spawn(move || control_loop(receiver));
    }
}

fn control_channel(engine: &Engine) -> Result<Receiver<ControlMsg>, ()> {
    let mut guard = engine.control_rx.lock().map_err(|_| ())?;
    guard.take().ok_or(())
}
