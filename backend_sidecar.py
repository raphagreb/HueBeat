"""Headless backend for the HueBeat Tauri UI.

The Tauri shell starts this process and talks to it with newline-delimited
JSON:

  stdout -> {"type": "state", ...} at ~20 fps, plus settings/ready replies
  stdin  <- {"cmd": "playpause"} / {"cmd": "settings_set", "values": {...}}

Everything human-readable (the old console output) goes to stderr, so the
protocol on stdout stays clean.
"""

import base64
import io
import json
import os
import sys
import threading
import time

# ---- keep stdout for the protocol only -------------------------------------
_REAL_OUT = sys.stdout
try:
    _REAL_OUT.reconfigure(encoding="utf-8", newline="\n")
except Exception:
    pass
try:
    sys.stdin.reconfigure(encoding="utf-8")
except Exception:
    pass
if sys.stderr is not None:
    try:
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
if sys.stdout is not None:
    sys.stdout = sys.stderr          # music.py prints must not corrupt JSON

from PIL import Image, ImageDraw, ImageEnhance, ImageFilter, ImageOps  # noqa: E402

import music  # noqa: E402
from settings import DEFAULTS, cfg  # noqa: E402

APP_VERSION = "2.0.0"
FPS = 20

_SETTINGS_KEYS = (
    "porta", "dispositivo_audio", "usar_openrgb", "openrgb_host",
    "openrgb_port", "pausa_brilho_pc", "suavidade", "gamma_pc",
    "sensibilidade", "batida_min_pc", "batida_decaimento",
    "iniciar_minimizado", "fechar_para_bandeja", "batida_inicial",
    "tecla_batida", "tecla_play", "tecla_prox", "tecla_ant",
)

_out_lock = threading.Lock()


def send(obj):
    try:
        linha = json.dumps(obj, separators=(",", ":"), ensure_ascii=False)
    except Exception:
        return
    with _out_lock:
        try:
            _REAL_OUT.write(linha + "\n")
            _REAL_OUT.flush()
        except Exception:
            pass


# ----------------------------- artwork --------------------------------------

def _cortar_barras(img):
    """Remove uniform black bars (letterbox/pillarbox) from the cover."""
    rgb = img.convert("RGB")
    larg, alt = rgb.size
    if larg < 8 or alt < 8:
        return rgb

    px = rgb.load()
    limite = 26
    passo_x = max(1, larg // 80)
    passo_y = max(1, alt // 80)

    def linha_escura(y):
        return all(max(px[x, y]) <= limite for x in range(0, larg, passo_x))

    def coluna_escura(x):
        return all(max(px[x, y]) <= limite for y in range(0, alt, passo_y))

    topo = 0
    while topo < alt and linha_escura(topo):
        topo += 1
    base = alt - 1
    while base >= 0 and linha_escura(base):
        base -= 1
    esq = 0
    while esq < larg and coluna_escura(esq):
        esq += 1
    dir_ = larg - 1
    while dir_ >= 0 and coluna_escura(dir_):
        dir_ -= 1

    if topo > alt // 4:
        topo = 0
    if (alt - 1 - base) > alt // 4:
        base = alt - 1
    if esq > larg // 4:
        esq = 0
    if (larg - 1 - dir_) > larg // 4:
        dir_ = larg - 1
    if (base - topo + 1) < 32 or (dir_ - esq + 1) < 32:
        return rgb
    if topo == 0 and base == alt - 1 and esq == 0 and dir_ == larg - 1:
        return rgb
    return rgb.crop((esq, topo, dir_ + 1, base + 1))


def _quadrado(img, lado=640):
    """Square cover: cropped when it is already square-ish, blurred fill otherwise."""
    img = _cortar_barras(ImageOps.exif_transpose(img))
    larg, alt = img.size
    if abs(larg / max(1, alt) - 1.0) < 0.06:
        return ImageOps.fit(img, (lado, lado), Image.LANCZOS)

    fundo = ImageOps.fit(img, (lado, lado), Image.LANCZOS)
    fundo = fundo.filter(ImageFilter.GaussianBlur(22))
    fundo = ImageEnhance.Brightness(fundo).enhance(0.52)
    fundo = ImageEnhance.Color(fundo).enhance(0.85)

    escala = min(lado / larg, lado / alt)
    nl, na = max(1, int(larg * escala)), max(1, int(alt * escala))
    frente = img.resize((nl, na), Image.LANCZOS)
    fundo.paste(frente, ((lado - nl) // 2, (lado - na) // 2))
    return fundo.filter(ImageFilter.UnsharpMask(radius=1.2, percent=90, threshold=3))


def _capa_data_url(img):
    try:
        quadrada = _quadrado(img)
        buf = io.BytesIO()
        quadrada.save(buf, "JPEG", quality=88)
        return "data:image/jpeg;base64," + base64.b64encode(buf.getvalue()).decode()
    except Exception:
        return None


# ----------------------------- settings -------------------------------------

def _snapshot():
    valores = {k: cfg.get(k) for k in _SETTINGS_KEYS}
    try:
        from serial.tools import list_ports

        portas = [p.device for p in list_ports.comports()]
    except Exception:
        portas = []
    try:
        import soundcard as sc

        saidas = [s.name for s in sc.all_speakers()]
    except Exception:
        saidas = []
    return {
        "type": "settings",
        "values": valores,
        "ports": portas,
        "audio_outputs": saidas,
        "defaults": dict(DEFAULTS),
    }


def _aplicar_settings(novos):
    antigo = {k: cfg.get(k) for k in _SETTINGS_KEYS}
    limpos = {}
    for chave in _SETTINGS_KEYS:
        if chave not in novos:
            continue
        valor = novos[chave]
        try:
            if chave in ("usar_openrgb", "iniciar_minimizado",
                         "fechar_para_bandeja", "batida_inicial"):
                valor = bool(valor)
            elif chave == "openrgb_port":
                valor = int(str(valor).strip())
                if not 0 < valor < 65536:
                    raise ValueError
            elif chave in ("pausa_brilho_pc", "suavidade", "gamma_pc",
                           "sensibilidade", "batida_min_pc", "batida_decaimento"):
                valor = round(float(valor), 3)
            elif chave == "porta":
                valor = str(valor or "").strip()
                if valor.lower() in ("auto", "auto-detect", "default"):
                    valor = ""
            elif chave == "dispositivo_audio":
                valor = str(valor or "").strip()
                if valor.lower() in ("default", "default output"):
                    valor = ""
            else:
                valor = str(valor or "").strip()
        except Exception:
            return False, f"Invalid value for {chave}."
        limpos[chave] = valor

    if "openrgb_host" in limpos and not limpos["openrgb_host"]:
        limpos["openrgb_host"] = "127.0.0.1"

    for chave, valor in limpos.items():
        cfg.set(chave, valor)
    if not cfg.save():
        return False, "Could not save the settings file."

    est = music.est
    if limpos.get("porta", antigo["porta"]) != antigo["porta"]:
        est["reconectar_arduino"] = True
    if limpos.get("dispositivo_audio", antigo["dispositivo_audio"]) != antigo["dispositivo_audio"]:
        est["reconectar_audio"] = True
    if any(limpos.get(k, antigo[k]) != antigo[k]
           for k in ("usar_openrgb", "openrgb_host", "openrgb_port")):
        est["reconectar_openrgb"] = True
    if any(limpos.get(k, antigo[k]) != antigo[k]
           for k in ("tecla_batida", "tecla_play", "tecla_prox", "tecla_ant")):
        est["reload_hotkeys"] = True
    est["settings_seq"] = int(est.get("settings_seq", 0)) + 1
    return True, ""


def _reset_settings():
    cfg.reset()
    est = music.est
    est["reconectar_arduino"] = True
    est["reconectar_audio"] = True
    est["reconectar_openrgb"] = True
    est["reload_hotkeys"] = True
    est["settings_seq"] = int(est.get("settings_seq", 0)) + 1


# ----------------------------- state loop -----------------------------------

def _estado_loop():
    ultima_capa = -1
    while True:
        est = music.est
        seq = int(est.get("capa_seq", 0))
        quadro = {
            "type": "state",
            "tocando": bool(est.get("tocando")),
            "titulo": est.get("titulo") or "",
            "artista": est.get("artista") or "",
            "pos": int(est.get("pos", 0) or 0),
            "dur": int(est.get("dur", 0) or 0),
            "cor": [int(c) for c in est.get("cor_alvo", (255, 120, 0))],
            "flash": round(float(est.get("flash", 0.0) or 0.0), 4),
            "arduino_ok": bool(est.get("arduino_ok")),
            "openrgb_ok": bool(est.get("openrgb_ok")),
            "porta": est.get("porta") or "",
            "batida_ativa": bool(est.get("batida_ativa")),
            "batida_disponivel": bool(est.get("batida_disponivel", True)),
            "viu_musica": bool(est.get("viu_musica")),
            "capa_seq": seq,
        }
        espectro = est.get("espectro") or []
        quadro["espectro"] = [round(float(v), 3) for v in list(espectro)[:64]]

        if seq != ultima_capa:
            ultima_capa = seq
            capa = est.get("capa")
            quadro["capa"] = _capa_data_url(capa) if capa is not None else None

        send(quadro)
        time.sleep(1 / FPS)


# ----------------------------- commands -------------------------------------

def _comando(linha):
    try:
        pedido = json.loads(linha)
    except Exception:
        return
    if not isinstance(pedido, dict):
        return
    cmd = pedido.get("cmd")

    if cmd in ("playpause", "next", "prev"):
        music.controle_fila.put(cmd)
    elif cmd == "seek":
        try:
            music.controle_fila.put(f"seek:{max(0, int(float(pedido.get('value', 0))))}")
        except Exception:
            pass
    elif cmd == "beat_toggle":
        music.pedido_toggle.set()
    elif cmd == "beat_set":
        desejado = bool(pedido.get("value"))
        if desejado != bool(music.est.get("batida_ativa")):
            music.pedido_toggle.set()
    elif cmd == "settings_get":
        send(_snapshot())
    elif cmd == "settings_set":
        ok, erro = _aplicar_settings(pedido.get("values") or {})
        send({"type": "settings_saved", "ok": ok, "error": erro})
        send(_snapshot())
    elif cmd == "settings_reset":
        _reset_settings()
        send({"type": "settings_saved", "ok": True, "error": ""})
        send(_snapshot())
    elif cmd == "reconnect":
        alvo = pedido.get("target")
        if alvo == "arduino":
            music.est["reconectar_arduino"] = True
        elif alvo == "audio":
            music.est["reconectar_audio"] = True
        elif alvo == "openrgb":
            music.est["reconectar_openrgb"] = True
        music.est["settings_seq"] = int(music.est.get("settings_seq", 0)) + 1
    elif cmd == "ping":
        send({"type": "pong"})
    elif cmd == "quit":
        try:
            music.fechar_tudo()
        except Exception:
            pass
        send({"type": "bye"})
        os._exit(0)


def main():
    music.est["modo_console"] = False
    send({"type": "hello", "version": APP_VERSION})
    send(_snapshot())

    threading.Thread(target=music._rodar_asyncio, daemon=True).start()
    threading.Thread(target=_estado_loop, daemon=True).start()

    for linha in sys.stdin:
        linha = linha.strip()
        if linha:
            _comando(linha)

    # stdin closed: the Tauri shell is gone, do not stay alive
    try:
        music.fechar_tudo()
    except Exception:
        pass
    os._exit(0)


if __name__ == "__main__":
    main()
