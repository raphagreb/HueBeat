const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const root = document.documentElement;

const SLIDERS = [
  ["cfg-pausa", "val-pausa", "pausa_brilho_pc", 2],
  ["cfg-suavidade", "val-suavidade", "suavidade", 2],
  ["cfg-gamma", "val-gamma", "gamma_pc", 2],
  ["cfg-sens", "val-sens", "sensibilidade", 2],
  ["cfg-batida-min", "val-batida-min", "batida_min_pc", 2],
  ["cfg-decaimento", "val-decaimento", "batida_decaimento", 2],
];

let state = null;
let lastRecv = 0;
let lastNow = performance.now();
let lastAccent = [-1, -1, -1];
let lastAccentAt = 0;
let accent = [255, 120, 0];
let accentTarget = [255, 120, 0];
let flash = 0;
let bootDone = false;
let bootError = false;
let dragging = false;
let dragPos = 0;
let pendingSeek = null;   // { pos, at } mostra o alvo até o backend confirmar
let currentCover = null;
let settingsData = null;
let settingsOpen = false;
let resetArmed = false;
let resetTimer = 0;
let toastTimer = 0;
let volDragging = false;
let volHoldUntil = 0;

const spectrumTarget = new Float32Array(64);
const spectrumCur = new Float32Array(64);

/* -------------------------------- helpers -------------------------------- */

const send = (obj) => {
  invoke("backend_send", { line: JSON.stringify(obj) }).catch(() => {});
};

function fmt(seconds) {
  const s = Math.max(0, Math.floor(Number(seconds) || 0));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function toast(message, isError = false) {
  const node = $("toast");
  node.textContent = message;
  node.classList.toggle("error", isError);
  node.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => node.classList.add("hidden"), 2600);
}

function hideBoot() {
  if (bootDone) return;
  bootDone = true;
  bootError = false;
  const boot = $("boot");
  boot.classList.remove("hidden");
  boot.classList.add("fade");
  setTimeout(() => boot.classList.add("hidden"), 480);
}

function bootErrorState(text) {
  if (bootError) return;
  bootError = true;
  $("boot").classList.remove("hidden", "fade");
  $("boot-sub").textContent = text;
  const spinner = document.querySelector(".boot-spinner");
  if (spinner) spinner.style.display = "none";
}

/* -------------------------------- covers --------------------------------- */

function applyCover(url) {
  const cover = $("cover");
  const img = $("cover-img");

  if (!url) {
    currentCover = null;
    img.removeAttribute("src");
    cover.classList.remove("has-cover");
    $("backdrop-img").classList.remove("show");
    return;
  }
  if (currentCover === url) return;
  currentCover = url;

  img.src = url;
  cover.classList.add("has-cover");
  $("backdrop-img").style.backgroundImage = `url("${url}")`;
  $("backdrop-img").classList.add("show");
  $("cover-glow").style.backgroundImage = `url("${url}")`;
}

/* -------------------------------- state ---------------------------------- */

function applyState(msg) {
  const first = state === null;
  const prev = state;
  state = msg;
  lastRecv = performance.now();
  if (first) hideBoot();

  // O YouTube Music às vezes zera a timeline no meio da faixa (ao buscar ou
  // ao trocar de estado). Nesse caso mantém a última duração/posição válidas
  // em vez de piscar "0:00 - 0:00"; o próximo quadro válido corrige tudo.
  if (prev && Number(prev.dur) > 0 && !(Number(msg.dur) > 0) &&
      (msg.titulo || "") === (prev.titulo || "")) {
    state = { ...msg, dur: prev.dur, pos: prev.pos };
  }

  // Faixa nova: o alvo otimista do seek anterior não vale mais.
  if (prev && (msg.titulo || "") !== (prev.titulo || "")) pendingSeek = null;

  // Quando o backend confirmar que pulou para perto do alvo, solta o
  // indicador otimista do seek.
  if (pendingSeek) {
    const pos = Number(msg.pos) || 0;
    if (Math.abs(pos - pendingSeek.pos) <= 4) pendingSeek = null;
  }

  $("app").classList.toggle("playing", !!msg.tocando);

  const hasTrack = !!(msg.titulo || msg.artista);
  $("title").textContent = msg.titulo || (hasTrack ? "Unknown title" : "Nothing playing");
  $("artist").textContent = msg.artista || (hasTrack ? "Unknown artist" : "Play something in your media app");
  $("state-label").textContent = msg.tocando ? "Playing" : hasTrack ? "Paused" : "Idle";
  $("port-label").textContent = msg.porta || "no device";

  accentTarget = (msg.cor || [255, 120, 0]).map(Number);
  flash = Math.max(flash, Number(msg.flash) || 0);

  const spec = msg.espectro || [];
  for (let i = 0; i < 64; i++) spectrumTarget[i] = Number(spec[i]) || 0;

  if ("capa" in msg) applyCover(msg.capa);

  // connection pills
  const pillA = $("pill-arduino");
  pillA.classList.toggle("ok", !!msg.arduino_ok);
  $("pill-arduino-text").textContent = msg.arduino_ok ? `Arduino · ${msg.porta || "com"}` : "Arduino";

  const pillO = $("pill-openrgb");
  pillO.classList.toggle("ok", !!msg.openrgb_ok);

  // beat
  const beatBtn = $("btn-beat");
  const available = msg.batida_disponivel !== false;
  beatBtn.disabled = !available;
  beatBtn.classList.toggle("active", !!msg.batida_ativa && available);
  $("beat-state").textContent = available ? (msg.batida_ativa ? "ON" : "OFF") : "N/A";

  // stats
  const arduino = $("stat-arduino");
  if (msg.arduino_ok) {
    arduino.textContent = `Connected · ${msg.porta || "COM"}`;
    arduino.className = "stat-value ok";
  } else {
    arduino.textContent = "Searching…";
    arduino.className = "stat-value off";
  }

  const openrgb = $("stat-openrgb");
  openrgb.textContent = msg.openrgb_ok ? "Connected" : "Offline";
  openrgb.className = `stat-value ${msg.openrgb_ok ? "ok" : "off"}`;

  const beat = $("stat-beat");
  if (!available) {
    beat.textContent = "Unavailable";
    beat.className = "stat-value off";
  } else {
    beat.textContent = msg.batida_ativa ? (msg.tocando ? "Pulsing" : "Standby") : "Disabled";
    beat.className = `stat-value ${msg.batida_ativa ? "ok" : "off"}`;
  }

  // player volume
  if ("vol" in msg) {
    if (
      !volDragging &&
      performance.now() >= volHoldUntil &&
      document.activeElement !== $("vol")
    ) {
      setVolumeUi(msg.vol);
    }
    $("vol-app").textContent = msg.vol_app || "No active player";
  }

  if (spec.length) $("eq-badge").textContent = `${spec.length} bands`;
}

/* ------------------------------- render loop ----------------------------- */

function loop(now) {
  requestAnimationFrame(loop);
  const dt = Math.min(0.06, Math.max(0.001, (now - lastNow) / 1000));
  lastNow = now;

  // accent interpolation (throttled: the variable repaints the backdrop)
  for (let i = 0; i < 3; i++) {
    accent[i] += (accentTarget[i] - accent[i]) * Math.min(1, dt * 6);
  }
  const ar = Math.round(accent[0]);
  const ag = Math.round(accent[1]);
  const ab = Math.round(accent[2]);
  if (
    now - lastAccentAt > 55 &&
    (ar !== lastAccent[0] || ag !== lastAccent[1] || ab !== lastAccent[2])
  ) {
    lastAccentAt = now;
    lastAccent = [ar, ag, ab];
    root.style.setProperty("--accent-rgb", `${ar}, ${ag}, ${ab}`);
  }

  // beat pulse
  flash += ((state ? Number(state.flash) || 0 : 0) - flash) * Math.min(1, dt * 20);
  if (flash < 0.001) flash = 0;
  root.style.setProperty("--beat", flash.toFixed(3));

  // progress
  if (state) {
    let pos = Number(state.pos) || 0;
    const dur = Number(state.dur) || 0;
    if (dragging) {
      pos = dragPos * dur;
    } else if (pendingSeek && now - pendingSeek.at < 15000) {
      pos = pendingSeek.pos + (state.tocando ? (now - pendingSeek.at) / 1000 : 0);
      pos = Math.min(pos, dur);
    } else if (state.tocando && dur > 0) {
      pos = Math.min(dur, pos + (now - lastRecv) / 1000);
    }
    const frac = dragging ? dragPos : dur > 0 ? Math.min(1, pos / dur) : 0;
    $("progress-fill").style.width = `${frac * 100}%`;
    $("progress-thumb").style.left = `${frac * 100}%`;
    $("pos").textContent = fmt(pos);
    $("dur").textContent = fmt(dur);
    $("progress").style.cursor = dur > 0 ? "pointer" : "default";
  }

  drawSpectrum(dt);
}

function drawSpectrum(dt) {
  const canvas = $("spectrum");
  const rect = canvas.getBoundingClientRect();
  if (rect.width < 2 || rect.height < 2) return;

  const dpr = window.devicePixelRatio || 1;
  const w = Math.round(rect.width);
  const h = Math.round(rect.height);
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }

  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const n = 64;
  const gap = Math.max(2, w * 0.0045);
  const bw = (w - gap * (n - 1)) / n;
  if (bw <= 0) return;
  const radius = Math.min(bw / 2, 3.5);

  const [r, g, b] = accent.map((v) => Math.round(v));
  const grad = ctx.createLinearGradient(0, h, 0, 0);
  grad.addColorStop(0, `rgba(${r}, ${g}, ${b}, 0.62)`);
  grad.addColorStop(0.55, `rgba(${r}, ${g}, ${b}, 0.92)`);
  grad.addColorStop(1, `rgba(${Math.min(255, r + 70)}, ${Math.min(255, g + 70)}, ${Math.min(255, b + 70)}, 1)`);

  const boost = 0.9 + flash * 0.45;
  const floor = 2;

  ctx.fillStyle = grad;
  for (let i = 0; i < n; i++) {
    const target = Math.pow(Math.max(0, Math.min(1, spectrumTarget[i])), 0.72);
    spectrumCur[i] += (target - spectrumCur[i]) * Math.min(1, dt * 15);
    const value = spectrumCur[i];
    if (value < 0.002) continue;
    const bh = Math.max(floor, Math.min(h - 4, value * (h - 4) * boost));
    const x = i * (bw + gap);
    const y = h - bh;
    ctx.beginPath();
    ctx.roundRect(x, y, bw, bh, radius);
    ctx.fill();
  }

  // baseline
  ctx.fillStyle = "rgba(255, 255, 255, 0.05)";
  ctx.fillRect(0, h - 1, w, 1);
}

/* -------------------------------- settings ------------------------------- */

function fillSelect(id, values, current, autoLabel) {
  const select = $(id);
  select.innerHTML = "";
  const add = (value, label) => {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = label;
    select.appendChild(option);
  };
  add("", autoLabel);
  for (const value of values || []) if (value) add(value, value);
  if (current && ![...select.options].some((o) => o.value === current)) {
    add(current, current);
  }
  select.value = current || "";
}

function syncRange(input, label, digits) {
  const min = Number(input.min);
  const max = Number(input.max);
  const value = Number(input.value);
  const percent = max > min ? ((value - min) / (max - min)) * 100 : 0;
  input.style.setProperty("--p", `${percent}%`);
  const out = $(label);
  if (out) out.textContent = value.toFixed(digits);
}

function setVolumeUi(value) {
  const input = $("vol");
  const clamped = Math.max(0, Math.min(100, Math.round(Number(value) || 0)));
  input.value = String(clamped);
  input.style.setProperty("--p", `${clamped}%`);
  $("vol-value").textContent = `${clamped}%`;
}

function renderSettings(data) {
  const values = data.values || {};

  fillSelect("cfg-port", data.ports, values.porta || "", "Auto-detect");
  fillSelect("cfg-audio", data.audio_outputs, values.dispositivo_audio || "", "Default output");

  $("cfg-openrgb").checked = !!values.usar_openrgb;
  $("cfg-host").value = values.openrgb_host || "127.0.0.1";
  $("cfg-openrgb-port").value = values.openrgb_port ?? 6742;

  $("cfg-tecla-batida").value = values.tecla_batida || "";
  $("cfg-tecla-play").value = values.tecla_play || "";
  $("cfg-tecla-prox").value = values.tecla_prox || "";
  $("cfg-tecla-ant").value = values.tecla_ant || "";

  for (const [inputId, labelId, key, digits] of SLIDERS) {
    const input = $(inputId);
    input.value = values[key] ?? input.value;
    syncRange(input, labelId, digits);
  }

  $("cfg-start-min").checked = !!values.iniciar_minimizado;
  $("cfg-close-tray").checked = !!values.fechar_para_bandeja;
  $("cfg-beat-start").checked = !!values.batida_inicial;

  settingsData = data;
}

function openSettings() {
  settingsOpen = true;
  resetArmed = false;
  $("settings-reset").textContent = "Reset";
  $("settings").classList.remove("hidden");
  send({ cmd: "settings_get" });
  if (settingsData) renderSettings(settingsData);
}

function closeSettings() {
  settingsOpen = false;
  $("settings").classList.add("hidden");
}

function collectSettings() {
  const values = {
    porta: $("cfg-port").value,
    dispositivo_audio: $("cfg-audio").value,
    usar_openrgb: $("cfg-openrgb").checked,
    openrgb_host: $("cfg-host").value.trim() || "127.0.0.1",
    openrgb_port: $("cfg-openrgb-port").value.trim(),
    tecla_batida: $("cfg-tecla-batida").value.trim(),
    tecla_play: $("cfg-tecla-play").value.trim(),
    tecla_prox: $("cfg-tecla-prox").value.trim(),
    tecla_ant: $("cfg-tecla-ant").value.trim(),
    iniciar_minimizado: $("cfg-start-min").checked,
    fechar_para_bandeja: $("cfg-close-tray").checked,
    batida_inicial: $("cfg-beat-start").checked,
  };
  for (const [inputId, , key] of SLIDERS) {
    values[key] = Number($(inputId).value);
  }
  return values;
}

function onSettingsSaved(msg) {
  if (!msg.ok) {
    toast(msg.error || "Could not save the settings.", true);
    return;
  }
  closeSettings();
  toast("Settings saved");
}

/* -------------------------------- wiring --------------------------------- */

function wireUi() {
  $("btn-play").addEventListener("click", () => send({ cmd: "playpause" }));
  $("btn-prev").addEventListener("click", () => send({ cmd: "prev" }));
  $("btn-next").addEventListener("click", () => send({ cmd: "next" }));
  $("btn-beat").addEventListener("click", () => send({ cmd: "beat_toggle" }));

  $("btn-settings").addEventListener("click", openSettings);
  $("btn-min").addEventListener("click", () => invoke("window_minimize"));
  $("btn-close").addEventListener("click", () => invoke("window_close"));

  $("settings-close").addEventListener("click", closeSettings);
  $("settings-cancel").addEventListener("click", closeSettings);
  $("settings-scrim").addEventListener("click", closeSettings);

  $("settings-save").addEventListener("click", () => {
    send({ cmd: "settings_set", values: collectSettings() });
  });

  $("settings-reset").addEventListener("click", () => {
    const button = $("settings-reset");
    if (!resetArmed) {
      resetArmed = true;
      button.textContent = "Confirm reset";
      clearTimeout(resetTimer);
      resetTimer = setTimeout(() => {
        resetArmed = false;
        button.textContent = "Reset";
      }, 3200);
      return;
    }
    resetArmed = false;
    button.textContent = "Reset";
    send({ cmd: "settings_reset" });
  });

  for (const [inputId, labelId, , digits] of SLIDERS) {
    const input = $(inputId);
    input.addEventListener("input", () => syncRange(input, labelId, digits));
  }

  // player volume (per-app WASAPI session)
  const volume = $("vol");
  setVolumeUi(volume.value);
  volume.addEventListener("input", () => {
    const value = Number(volume.value);
    setVolumeUi(value);
    volHoldUntil = performance.now() + 900;
    send({ cmd: "volume", value });
  });
  volume.addEventListener("pointerdown", () => {
    volDragging = true;
  });
  const releaseVolume = () => {
    volDragging = false;
    volHoldUntil = performance.now() + 900;
  };
  volume.addEventListener("pointerup", releaseVolume);
  volume.addEventListener("pointercancel", releaseVolume);
  volume.addEventListener("change", releaseVolume);

  // seek
  const progress = $("progress");
  const fraction = (event) => {
    const rect = progress.getBoundingClientRect();
    return Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
  };

  progress.addEventListener("pointerdown", (event) => {
    if (!state || !(Number(state.dur) > 0)) return;
    dragging = true;
    dragPos = fraction(event);
    progress.classList.add("dragging");
    progress.setPointerCapture(event.pointerId);
  });
  progress.addEventListener("pointermove", (event) => {
    if (dragging) dragPos = fraction(event);
  });
  progress.addEventListener("pointerup", (event) => {
    if (!dragging) return;
    dragging = false;
    progress.classList.remove("dragging");
    if (state && state.dur > 0) {
      const alvo = Math.round(dragPos * Number(state.dur));
      pendingSeek = { pos: alvo, at: performance.now() };
      send({ cmd: "seek", value: alvo });
    }
  });
  progress.addEventListener("pointercancel", () => {
    dragging = false;
    progress.classList.remove("dragging");
  });

  // keyboard (in-window shortcuts; global Ctrl+Alt hotkeys live in the backend)
  window.addEventListener("keydown", (event) => {
    const target = event.target;
    const typing =
      !!target &&
      typeof target.closest === "function" &&
      !!target.closest("input, select, textarea");
    if (event.key === "Escape" && settingsOpen) {
      event.preventDefault();
      closeSettings();
      return;
    }
    if (typing) return;

    if (event.ctrlKey && !event.altKey && event.key.toLowerCase() === "s") {
      event.preventDefault();
      settingsOpen ? closeSettings() : openSettings();
      return;
    }
    if (event.ctrlKey || event.altKey || event.metaKey) return;

    if (event.code === "Space") {
      event.preventDefault();
      send({ cmd: "playpause" });
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      send({ cmd: "next" });
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      send({ cmd: "prev" });
    }
  });

  document.addEventListener("contextmenu", (event) => event.preventDefault());
}

/* --------------------------------- boot ---------------------------------- */

async function boot() {
  await listen("backend-line", (event) => {
    let msg;
    try {
      msg = JSON.parse(event.payload);
    } catch {
      return;
    }
    if (msg.type === "state") applyState(msg);
    else if (msg.type === "settings") {
      settingsData = msg;
      const values = msg.values || {};
      invoke("set_close_to_tray", { value: !!values.fechar_para_bandeja }).catch(() => {});
      invoke("set_start_minimized", { value: !!values.iniciar_minimizado }).catch(() => {});
      if (settingsOpen) renderSettings(msg);
    } else if (msg.type === "settings_saved") onSettingsSaved(msg);
    else if (msg.type === "bye") bootErrorState("The engine stopped.");
  });

  await listen("backend-closed", () => {
    if (!bootDone) bootErrorState("The audio engine could not start. Check backend.log.");
    else bootErrorState("The audio engine stopped. Restart HueBeat.");
  });

  await listen("backend-error", (event) => {
    bootErrorState(`The audio engine could not start: ${event.payload}`);
  });

  await listen("open-settings", () => openSettings());

  wireUi();
  requestAnimationFrame(loop);
  requestAnimationFrame(() => requestAnimationFrame(() => invoke("ui_ready").catch(() => {})));

  setTimeout(() => {
    if (!state) bootErrorState("The engine is taking longer than expected…");
  }, 15000);
}

boot();
