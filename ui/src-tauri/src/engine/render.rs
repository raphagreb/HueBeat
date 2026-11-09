//! LED render loop: colour fade, gamma/calibration, beat flash and the
//! `M`/`B` commands sent to the Arduino, plus the OLED clock task.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Datelike;

use super::Engine;

const TICK: Duration = Duration::from_millis(16);
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub fn spawn(engine: Arc<Engine>) {
    thread::spawn(move || {
        let (mut batida_ativa, mut cor_atual) = {
            let settings = engine.settings.lock().ok();
            let start = settings
                .as_ref()
                .map(|settings| settings.bool("batida_inicial"))
                .unwrap_or(true);
            let state = engine.state.lock().ok();
            let color = state
                .as_ref()
                .map(|state| {
                    [
                        state.cor_alvo[0] as f64,
                        state.cor_alvo[1] as f64,
                        state.cor_alvo[2] as f64,
                    ]
                })
                .unwrap_or([255.0, 120.0, 0.0]);
            (start, color)
        };

        let mut modo_enviado: Option<bool> = None;
        let mut reforco: Option<Instant> = None;
        let mut ultimo_m = Instant::now() - Duration::from_secs(30);
        let mut geracao = engine.serial.generation();
        let mut bri_atual = 0.0f64;
        let mut flash = 0.0f64;
        let mut t_ant = Instant::now();

        loop {
            let agora = Instant::now();
            let dt = (agora - t_ant).as_secs_f64().min(0.25);
            t_ant = agora;

            let atual = engine.serial.generation();
            if atual != geracao {
                geracao = atual;
                modo_enviado = None;
                reforco = Some(agora + Duration::from_millis(1500));
                ultimo_m = agora - Duration::from_secs(10);
            }

            if engine.toggle_beat.swap(false, Ordering::Relaxed) {
                if engine.detector.disponivel.load(Ordering::Relaxed) {
                    batida_ativa = !batida_ativa;
                    flash = 0.0;
                }
            }

            let disponivel = engine.detector.disponivel.load(Ordering::Relaxed);
            let efetiva = batida_ativa && disponivel;
            engine.detector.ativo.store(efetiva, Ordering::Relaxed);

            let (tocando, alvo, viu_musica) = {
                let Ok(state) = engine.state.lock() else {
                    thread::sleep(TICK);
                    continue;
                };
                (state.tocando, state.cor_alvo, state.viu_musica)
            };
            if let Ok(mut state) = engine.state.lock() {
                state.batida_ativa = batida_ativa;
                state.batida_disponivel = disponivel;
            }

            // Keep the Arduino in the right mode. Reassert it after a
            // reconnect and every 5 s in case a redraw ate the line.
            let reforcar = reforco.map(|when| agora >= when).unwrap_or(false);
            let precisa = modo_enviado != Some(efetiva)
                || reforcar
                || agora.duration_since(ultimo_m) >= Duration::from_secs(5);
            if precisa && engine.serial.send(&format!("M,{}\n", if efetiva { 1 } else { 0 })) {
                modo_enviado = Some(efetiva);
                if reforcar {
                    reforco = None;
                }
                ultimo_m = agora;
            }

            if engine.detector.batida.swap(false, Ordering::Relaxed) {
                if efetiva && tocando {
                    flash = 1.0;
                    engine.serial.send("B\n");
                }
            }

            let (pausa, suavidade, gamma, cal, batida_min, decaimento) = {
                let Ok(settings) = engine.settings.lock() else {
                    thread::sleep(TICK);
                    continue;
                };
                let mut cal = [1.0f64; 3];
                let values = settings.numbers("cal_pc");
                for (index, value) in values.iter().take(3).enumerate() {
                    cal[index] = *value;
                }
                (
                    settings.f64("pausa_brilho_pc"),
                    settings.f64("suavidade").max(0.02),
                    settings.f64("gamma_pc").max(0.1),
                    cal,
                    settings.f64("batida_min_pc"),
                    settings.f64("batida_decaimento").max(0.01),
                )
            };

            let k = 1.0 - (-dt / suavidade).exp();
            for i in 0..3 {
                cor_atual[i] += (alvo[i] as f64 - cor_atual[i]) * k;
            }
            let bri_alvo = if tocando { 1.0 } else { pausa };
            bri_atual += (bri_alvo - bri_atual) * k;
            flash *= (-dt / decaimento).exp();

            if let Ok(mut state) = engine.state.lock() {
                state.flash = if efetiva && tocando {
                    flash as f32
                } else {
                    0.0
                };
            }

            if viu_musica {
                let mut mult = bri_atual;
                if efetiva && tocando {
                    mult *= batida_min + (1.0 - batida_min) * flash;
                }
                let mut rgb = [0u8; 3];
                for i in 0..3 {
                    let c = 255.0
                        * (cor_atual[i].max(0.0) / 255.0).powf(gamma)
                        * cal[i]
                        * mult
                        + 0.5;
                    rgb[i] = c.clamp(0.0, 255.0) as u8;
                }
                if let Ok(mut color) = engine.openrgb_color.lock() {
                    *color = rgb;
                }
            }

            thread::sleep(TICK);
        }
    });
}

/// The Arduino has no RTC: send the local time every 10 s and right after a
/// reconnect.
pub fn spawn_clock(engine: Arc<Engine>) {
    thread::spawn(move || {
        let mut geracao = u64::MAX;
        let mut ultimo = Instant::now() - Duration::from_secs(30);
        loop {
            if geracao != engine.serial.generation()
                || ultimo.elapsed() >= Duration::from_secs(10)
            {
                let agora = chrono::Local::now();
                let weekday = WEEKDAYS[agora.weekday().num_days_from_monday() as usize];
                let data = format!(
                    "K,{}\nJ,{}\nW,{}\n",
                    agora.format("%H:%M:%S"),
                    agora.format("%d/%m/%Y"),
                    weekday
                );
                if engine.serial.send(&data) {
                    geracao = engine.serial.generation();
                    ultimo = Instant::now();
                }
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}
