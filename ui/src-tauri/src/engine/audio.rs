//! WASAPI loopback capture -> 64-band spectrum + bass beat detector.
//!
//! This replaces the Python `DetectorBatida` (soundcard + numpy). The maths
//! (Hann window, log-spaced bands, dB mapping, smoothing and the beat
//! threshold) is kept identical so the Arduino sees the same values.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

use super::Engine;

const BLOCK: usize = 1024;
const BANDS: usize = 64;

pub struct BeatDetector {
    pub disponivel: AtomicBool,
    pub reiniciar: AtomicBool,
    pub ativo: AtomicBool,
    pub batida: AtomicBool,
}

impl Default for BeatDetector {
    fn default() -> Self {
        Self {
            disponivel: AtomicBool::new(true),
            reiniciar: AtomicBool::new(false),
            ativo: AtomicBool::new(false),
            batida: AtomicBool::new(false),
        }
    }
}

struct Bands {
    /// Inclusive start / exclusive end bin ranges.
    ranges: Vec<(usize, usize)>,
    /// Low / high bin range used by the beat detector.
    graves: (usize, usize),
    freqs: Vec<f32>,
}

impl Bands {
    fn new(sample_rate: u32, graves_hz: (f64, f64)) -> Self {
        let bins = BLOCK / 2 + 1;
        let freqs: Vec<f32> = (0..bins)
            .map(|i| i as f32 * sample_rate as f32 / BLOCK as f32)
            .collect();
        let high = (sample_rate as f64 / 2.0).min(16_000.0);
        let low = 40.0f64.min(high / 2.0);
        let mut ranges = Vec::with_capacity(BANDS);
        for band in 0..BANDS {
            let a = low * (high / low).powf(band as f64 / BANDS as f64);
            let b = low * (high / low).powf((band + 1) as f64 / BANDS as f64);
            let i0 = freqs.partition_point(|f| (*f as f64) < a);
            let i1 = freqs.partition_point(|f| (*f as f64) < b).max(i0 + 1);
            ranges.push((i0.min(bins - 1), i1.min(bins)));
        }
        let lo = freqs.partition_point(|f| (*f as f64) < graves_hz.0);
        let hi = freqs
            .partition_point(|f| (*f as f64) <= graves_hz.1)
            .max(lo + 1)
            .min(bins);
        Self {
            ranges,
            graves: (lo.min(bins - 1), hi),
            freqs,
        }
    }
}

fn choose_device(engine: &Engine) -> Option<wasapi::Device> {
    let enumerator = DeviceEnumerator::new().ok()?;
    let collection = enumerator.get_device_collection(Direction::Render).ok()?;
    let count = collection.get_nbr_devices().ok()?;

    let mut names = Vec::new();
    let mut devices = Vec::new();
    for index in 0..count {
        let Ok(device) = collection.get_device_at_index(index) else {
            continue;
        };
        let name = device.get_friendlyname().unwrap_or_default();
        names.push(name);
        devices.push(device);
    }
    if let Ok(mut list) = engine.audio_outputs.lock() {
        *list = names.clone();
    }

    let target = engine
        .settings
        .lock()
        .map(|s| s.str("dispositivo_audio").to_lowercase())
        .unwrap_or_default();
    if !target.is_empty() {
        for (index, name) in names.iter().enumerate() {
            if name.to_lowercase().contains(&target) {
                return devices.into_iter().nth(index);
            }
        }
    }
    enumerator.get_default_device(Direction::Render).ok()
}

#[derive(Clone, Copy)]
enum Sample {
    F32,
    F64,
    I16,
    I24,
    I32,
}

fn sample_kind(format: &WaveFormat) -> Option<Sample> {
    let bits = format.get_bitspersample();
    match format.get_subformat().ok()? {
        SampleType::Float => {
            if bits == 64 {
                Some(Sample::F64)
            } else {
                Some(Sample::F32)
            }
        }
        SampleType::Int => match bits {
            16 => Some(Sample::I16),
            24 => Some(Sample::I24),
            32 => Some(Sample::I32),
            _ => None,
        },
    }
}

fn push_frames(
    data: &[u8],
    frames: usize,
    channels: usize,
    block_align: usize,
    kind: Sample,
    out: &mut Vec<f32>,
) {
    let channels = channels.max(1);
    for frame in 0..frames {
        let base = frame * block_align;
        if base + block_align > data.len() {
            break;
        }
        let mut sum = 0.0f32;
        for channel in 0..channels {
            let offset = base + channel * (block_align / channels);
            let value = match kind {
                Sample::F32 => {
                    let b = [data[offset], data[offset + 1], data[offset + 2], data[offset + 3]];
                    f32::from_le_bytes(b)
                }
                Sample::F64 => {
                    let mut b = [0u8; 8];
                    b.copy_from_slice(&data[offset..offset + 8]);
                    f64::from_le_bytes(b) as f32
                }
                Sample::I16 => {
                    let b = [data[offset], data[offset + 1]];
                    i16::from_le_bytes(b) as f32 / 32_768.0
                }
                Sample::I24 => {
                    let raw = (data[offset] as i32)
                        | ((data[offset + 1] as i32) << 8)
                        | ((data[offset + 2] as i32) << 16);
                    let signed = (raw << 8) >> 8;
                    signed as f32 / 8_388_608.0
                }
                Sample::I32 => {
                    let b = [
                        data[offset],
                        data[offset + 1],
                        data[offset + 2],
                        data[offset + 3],
                    ];
                    i32::from_le_bytes(b) as f32 / 2_147_483_648.0
                }
            };
            sum += value;
        }
        out.push(sum / channels as f32);
    }
}

fn analyse(
    samples: &[f32],
    bands: &Bands,
    fft: &dyn rustfft::Fft<f32>,
    window: &[f32],
    suave: &mut [f32; BANDS],
) -> Option<(f32, f32)> {
    if samples.len() < BLOCK {
        return None;
    }
    let mut buffer: Vec<Complex<f32>> = samples[..BLOCK]
        .iter()
        .enumerate()
        .map(|(index, value)| Complex::new(value * window[index], 0.0))
        .collect();
    fft.process(&mut buffer);

    // Magnitude spectrum, matching numpy's abs(rfft(x)) / BLOCK.
    let scale = 1.0 / BLOCK as f32;
    let magnitude: Vec<f32> = buffer[..BLOCK / 2 + 1]
        .iter()
        .map(|c| c.norm() * scale)
        .collect();

    for (band, (start, end)) in bands.ranges.iter().enumerate() {
        let slice = &magnitude[*start..*end];
        let rms = if slice.is_empty() {
            0.0
        } else {
            (slice.iter().map(|v| v * v).sum::<f32>() / slice.len() as f32).sqrt()
        };
        let db = 20.0 * (rms + 1e-9).log10();
        let level = ((db + 72.0) / 46.0).clamp(0.0, 1.0);
        suave[band] = level.max(suave[band] * 0.86);
    }

    let (lo, hi) = bands.graves;
    let energy: f32 = magnitude[lo.min(magnitude.len())..hi.min(magnitude.len())]
        .iter()
        .sum();

    // Frequency of the strongest bin inside the bass range, used only for the
    // caller's optional debug log.
    let peak = bands.freqs.get(lo).copied().unwrap_or(0.0);
    Some((energy, peak))
}

fn run_capture(engine: &Engine) -> Result<(), String> {
    let device = choose_device(engine).ok_or("no audio output")?;
    let mut client = device.get_iaudioclient().map_err(|e| e.to_string())?;
    let format = client.get_mixformat().map_err(|e| e.to_string())?;

    let sample_rate = format.get_samplespersec();
    let channels = format.get_nchannels() as usize;
    let block_align = format.get_blockalign() as usize;
    let kind = sample_kind(&format).ok_or("unsupported audio format")?;

    client
        .initialize_client(
            &format,
            Direction::Capture,
            StreamMode::PollingShared {
                autoconvert: true,
                buffer_duration_hns: 200_000,
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    client.start_stream().map_err(|e| e.to_string())?;

    let graves = engine
        .settings
        .lock()
        .map(|s| {
            let values = s.numbers("graves_hz");
            if values.len() >= 2 {
                (values[0], values[1])
            } else {
                (40.0, 150.0)
            }
        })
        .unwrap_or((40.0, 150.0));
    let bands = Bands::new(sample_rate, graves);

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(BLOCK);
    let window: Vec<f32> = (0..BLOCK)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / (BLOCK - 1) as f32).cos())
        .collect();

    let mut pending: Vec<f32> = Vec::with_capacity(BLOCK * 4);
    let mut suave = [0.0f32; BANDS];
    let history_len = (sample_rate as usize / BLOCK).max(1);
    let mut history: VecDeque<f32> = VecDeque::with_capacity(history_len + 1);
    let mut previous = 0.0f32;
    let mut last_beat = Instant::now() - Duration::from_secs(3600);

    let mut buffer: Vec<u8> = Vec::new();

    while !engine.detector.reiniciar.load(Ordering::Relaxed) {
        let next = match capture.get_next_packet_size() {
            Ok(value) => value,
            Err(_) => break,
        };
        let Some(frames) = next else {
            thread::sleep(Duration::from_millis(5));
            continue;
        };
        if frames == 0 {
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        let needed = frames as usize * block_align;
        if buffer.len() < needed {
            buffer.resize(needed, 0);
        }
        let (read, _info) = match capture.read_from_device(&mut buffer[..needed]) {
            Ok(value) => value,
            Err(_) => break,
        };
        push_frames(
            &buffer[..needed],
            read as usize,
            channels,
            block_align,
            kind,
            &mut pending,
        );

        while pending.len() >= BLOCK {
            let chunk: Vec<f32> = pending.drain(..BLOCK).collect();
            let Some((energy, _peak)) = analyse(&chunk, &bands, fft.as_ref(), &window, &mut suave)
            else {
                continue;
            };
            if let Ok(mut spectrum) = engine.spectrum.lock() {
                *spectrum = suave;
            }

            if !engine.detector.ativo.load(Ordering::Relaxed) {
                continue;
            }

            let media = if history.len() >= 10 {
                Some(history.iter().sum::<f32>() / history.len() as f32)
            } else {
                None
            };
            history.push_back(energy);
            if history.len() > history_len {
                history.pop_front();
            }

            let sensitivity = engine
                .settings
                .lock()
                .map(|s| s.f32("sensibilidade"))
                .unwrap_or(1.2);
            let floor = engine
                .settings
                .lock()
                .map(|s| s.f32("piso"))
                .unwrap_or(0.003);
            let interval = engine
                .settings
                .lock()
                .map(|s| s.f64("intervalo_min"))
                .unwrap_or(0.22);

            if let Some(media) = media {
                if energy > media * sensitivity
                    && energy > floor
                    && energy > previous
                    && last_beat.elapsed().as_secs_f64() > interval
                {
                    last_beat = Instant::now();
                    engine.detector.batida.store(true, Ordering::Relaxed);
                }
            }
            previous = energy;
        }
    }

    let _ = client.stop_stream();
    Ok(())
}

pub fn spawn(engine: Arc<Engine>) {
    thread::spawn(move || {
        let _ = wasapi::initialize_mta();
        loop {
            engine.detector.reiniciar.store(false, Ordering::Relaxed);
            let result = run_capture(&engine);
            if let Ok(mut spectrum) = engine.spectrum.lock() {
                *spectrum = [0.0; BANDS];
            }
            let requested = engine.detector.reiniciar.load(Ordering::Relaxed);
            if let Err(error) = result {
                let _ = error;
                thread::sleep(Duration::from_secs(3));
            } else if requested {
                thread::sleep(Duration::from_millis(300));
            } else {
                thread::sleep(Duration::from_secs(3));
            }
            engine.detector.disponivel.store(true, Ordering::Relaxed);
        }
    });
}
