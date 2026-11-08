import asyncio
import colorsys
import io
import math
import msvcrt
import os
import queue
import sys
import threading
import time
import unicodedata
import warnings
from collections import deque
from datetime import datetime, timedelta, timezone

import serial
from serial.tools import list_ports
from PIL import Image

from settings import cfg
from winrt.windows.media.control import (
    GlobalSystemMediaTransportControlsSessionManager as MediaManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
)
from winrt.windows.storage.streams import DataReader

warnings.filterwarnings("ignore", module="soundcard")

# ======================= CONFIG =======================
# Os padrões ficam em settings.py e podem ser editados na interface.
PORTA = cfg.get("porta") or ""       # "" = detectar automaticamente
BAUD = int(cfg.get("baud", 115200))

# ---- Cor da capa ----
SAT_MIN = float(cfg.get("sat_min"))
REALCE_SAT = float(cfg.get("realce_sat"))
COR_SEM_COR = (255, 255, 255)   # capa preto e branco/cinza
COR_PADRAO = tuple(cfg.get("cor_padrao"))

# ---- OpenRGB (RGB do PC) ----
USAR_OPENRGB = bool(cfg.get("usar_openrgb"))
OPENRGB_HOST = cfg.get("openrgb_host") or "127.0.0.1"
OPENRGB_PORT = int(cfg.get("openrgb_port"))
PAUSA_BRILHO_PC = float(cfg.get("pausa_brilho_pc"))
SUAVIDADE = float(cfg.get("suavidade"))
GAMMA_PC = float(cfg.get("gamma_pc"))
CAL_PC = tuple(cfg.get("cal_pc"))

# ---- Batida ----
BATIDA_INICIAL = bool(cfg.get("batida_inicial"))
TECLA_BATIDA = cfg.get("tecla_batida")
TECLA_PLAY = cfg.get("tecla_play")
TECLA_PROX = cfg.get("tecla_prox")
TECLA_ANT = cfg.get("tecla_ant")
BATIDA_MIN_PC = float(cfg.get("batida_min_pc"))
BATIDA_DECAIMENTO = float(cfg.get("batida_decaimento"))
GRAVES_HZ = tuple(cfg.get("graves_hz"))
SENSIBILIDADE = float(cfg.get("sensibilidade"))
PISO = float(cfg.get("piso"))
INTERVALO_MIN = float(cfg.get("intervalo_min"))
DEBUG_BATIDA = os.environ.get("HUEBEAT_DEBUG_BATIDA") == "1"   # True: mostra os valores no terminal para calibrar
DEBUG_SERIAL = os.environ.get("HUEBEAT_DEBUG_SERIAL") == "1"   # True: mostra tudo que vai para o Arduino
DISPOSITIVO_AUDIO = cfg.get("dispositivo_audio") or ""

TICK = 1 / 60                   # taxa de atualização do LED
# ======================================================


def aplicar_config():
    """Recarrega os valores que podem mudar em tempo real."""
    global PORTA, BAUD, SAT_MIN, REALCE_SAT, COR_PADRAO
    global USAR_OPENRGB, OPENRGB_HOST, OPENRGB_PORT
    global PAUSA_BRILHO_PC, SUAVIDADE, GAMMA_PC, CAL_PC
    global BATIDA_INICIAL, TECLA_BATIDA, TECLA_PLAY, TECLA_PROX, TECLA_ANT
    global BATIDA_MIN_PC, BATIDA_DECAIMENTO, GRAVES_HZ
    global SENSIBILIDADE, PISO, INTERVALO_MIN, DISPOSITIVO_AUDIO

    PORTA = cfg.get("porta") or ""
    BAUD = int(cfg.get("baud", 115200))
    SAT_MIN = float(cfg.get("sat_min"))
    REALCE_SAT = float(cfg.get("realce_sat"))
    COR_PADRAO = tuple(cfg.get("cor_padrao"))
    USAR_OPENRGB = bool(cfg.get("usar_openrgb"))
    OPENRGB_HOST = cfg.get("openrgb_host") or "127.0.0.1"
    OPENRGB_PORT = int(cfg.get("openrgb_port"))
    PAUSA_BRILHO_PC = float(cfg.get("pausa_brilho_pc"))
    SUAVIDADE = float(cfg.get("suavidade"))
    GAMMA_PC = float(cfg.get("gamma_pc"))
    CAL_PC = tuple(cfg.get("cal_pc"))
    BATIDA_INICIAL = bool(cfg.get("batida_inicial"))
    TECLA_BATIDA = cfg.get("tecla_batida")
    TECLA_PLAY = cfg.get("tecla_play")
    TECLA_PROX = cfg.get("tecla_prox")
    TECLA_ANT = cfg.get("tecla_ant")
    BATIDA_MIN_PC = float(cfg.get("batida_min_pc"))
    BATIDA_DECAIMENTO = float(cfg.get("batida_decaimento"))
    GRAVES_HZ = tuple(cfg.get("graves_hz"))
    SENSIBILIDADE = float(cfg.get("sensibilidade"))
    PISO = float(cfg.get("piso"))
    INTERVALO_MIN = float(cfg.get("intervalo_min"))
    DISPOSITIVO_AUDIO = cfg.get("dispositivo_audio") or ""

arduino = None
geracao = 0                     # aumenta a cada (re)conexão com o Arduino
pedido_toggle = threading.Event()
controle_fila = queue.Queue()   # play/pause, next, prev, seek...
saida_pc = None                 # saída OpenRGB atual (pode reconectar)
detector_ref = None             # detector de batida/áudio atual
tarefa_arduino_ref = None       # tarefa de conexão serial em andamento

# Estado compartilhado entre a lógica (asyncio) e a interface (Tkinter).
est = {
    "tocando": False,
    "cor_alvo": COR_PADRAO,
    "viu_musica": False,
    "titulo": "",
    "artista": "",
    "pos": 0,
    "dur": 0,
    "capa": None,               # PIL.Image da capa (para a interface)
    "capa_seq": 0,              # muda quando chega uma capa nova
    "flash": 0.0,               # 0..1, pulso da batida
    "espectro": [],             # níveis reais das bandas de áudio (0..1)
    "arduino_ok": False,
    "openrgb_ok": False,
    "batida_ativa": BATIDA_INICIAL,
    "batida_disponivel": True,
    "modo_console": True,
    "porta": "",
    "controle_fila": controle_fila,
    "settings_seq": 0,
}


# ---------------------- Texto e capa ----------------------

def so_ascii(texto):
    """O OLED não tem acentos: converte 'Música' em 'Musica'."""
    t = unicodedata.normalize("NFKD", texto or "")
    return t.encode("ascii", "ignore").decode().replace("\n", " ").strip()[:60]


# ---- Romanização (só para o OLED; a interface mantém o texto original) ----
_kakasi = None
_kakasi_tentou = False

_PARTICULAS = {
    "に", "の", "は", "を", "が", "で", "と", "へ", "も", "や", "か", "な",
    "ね", "よ", "だ", "です", "ます", "から", "まで", "より", "って", "だけ",
    "しか", "ほど", "ばかり", "こそ", "さえ", "でも", "では", "には", "とは",
    "への", "からは",
}


def _eh_japones(c):
    o = ord(c)
    return (
        0x3040 <= o <= 0x30FF          # hiragana + katakana
        or 0x3400 <= o <= 0x4DBF       # kanji (extensão A)
        or 0x4E00 <= o <= 0x9FFF       # kanji
        or o == 0x30FC                 # ー
    )


def _tem_kanji(texto):
    return any(
        0x3400 <= ord(c) <= 0x4DBF or 0x4E00 <= ord(c) <= 0x9FFF
        for c in texto
    )


def _so_kana(texto):
    if not texto:
        return False
    return all(
        0x3040 <= ord(c) <= 0x30FF or c == "ー" for c in texto
    )


def _preparar_romanizador():
    """Carrega o pykakasi uma vez (os dicionários são grandes)."""
    global _kakasi, _kakasi_tentou
    if _kakasi is not None or _kakasi_tentou:
        return
    _kakasi_tentou = True
    try:
        from pykakasi import kakasi

        _kakasi = kakasi()
        _kakasi.convert("テスト")       # força o carregamento dos dicionários
    except Exception as e:
        print(f"Romanização indisponível ({e}). Usando ASCII.")
        _kakasi = None


def _romanizar_trecho(trecho):
    """Romaniza um trecho que só tem caracteres japoneses."""
    try:
        partes = _kakasi.convert(trecho)
    except Exception:
        return so_ascii(trecho)

    saida = []
    anterior = ""
    for seg in partes:
        h = (seg.get("hepburn") or "").strip()
        o = seg.get("orig") or ""
        if not h:
            continue
        if saida:
            # "駆け" + "る" -> "kakeru"; "知ら" + "ない" -> "shiranai"
            if _so_kana(o) and o not in _PARTICULAS and _tem_kanji(anterior):
                saida[-1] += h
                anterior = o
                continue
            # "米津" + "玄" + "師" -> "yonetsugenshi"
            if _tem_kanji(o) and _tem_kanji(anterior) and len(o) <= 2:
                saida[-1] += h
                anterior = o
                continue
        saida.append(h)
        anterior = o

    resultado = " ".join(saida)
    if resultado and resultado[0].islower():
        resultado = resultado[0].upper() + resultado[1:]
    return resultado


def romanizar(texto):
    """Japonês -> romaji para o OLED. Texto latino passa direto."""
    texto = (texto or "").strip()
    if not texto:
        return ""
    if not any(_eh_japones(c) for c in texto):
        return so_ascii(texto)

    _preparar_romanizador()
    if _kakasi is None:
        return so_ascii(texto)

    # separa os trechos japoneses dos latinos para não quebrar palavras
    trechos = []
    atual = ""
    atual_jp = None
    for c in texto:
        jp = _eh_japones(c)
        if atual_jp is None or jp == atual_jp:
            atual += c
            atual_jp = jp
        else:
            trechos.append((atual_jp, atual))
            atual = c
            atual_jp = jp
    if atual:
        trechos.append((atual_jp, atual))

    resultado = ""
    for jp, trecho in trechos:
        pedaco = _romanizar_trecho(trecho) if jp else trecho
        if (
            resultado and pedaco
            and resultado[-1].isalnum() and pedaco[0].isalnum()
        ):
            resultado += " "
        resultado += pedaco

    resultado = " ".join(resultado.split())
    if resultado and resultado[0].islower():
        resultado = resultado[0].upper() + resultado[1:]
    return so_ascii(resultado)


def cor_dominante(img):
    """Escolhe a cor mais viva da capa e a deixa bem saturada."""
    img = img.convert("RGB").resize((48, 48))
    baldes = {}

    pixels = (
        img.get_flattened_data()
        if hasattr(img, "get_flattened_data")
        else img.getdata()
    )
    for r, g, b in pixels:
        h, s, v = colorsys.rgb_to_hsv(r / 255, g / 255, b / 255)
        if v < 0.2 or s < 0.15:
            continue
        peso = s * s * v
        a = baldes.setdefault(int(h * 12) % 12, [0.0, 0.0, 0.0, 0.0])
        a[0] += peso
        a[1] += r * peso
        a[2] += g * peso
        a[3] += b * peso

    melhor = max(baldes.values(), key=lambda a: a[0], default=None)
    if melhor is None or melhor[0] < 3:
        return COR_SEM_COR

    r, g, b = melhor[1] / melhor[0], melhor[2] / melhor[0], melhor[3] / melhor[0]
    h, s, v = colorsys.rgb_to_hsv(r / 255, g / 255, b / 255)
    s = min(1.0, max(s * REALCE_SAT, SAT_MIN))
    r, g, b = colorsys.hsv_to_rgb(h, s, 1.0)
    return int(r * 255), int(g * 255), int(b * 255)


async def cor_do_album(thumb):
    """Devolve (cor dominante, imagem da capa ou None)."""
    if thumb is None:
        return COR_PADRAO, None
    try:
        stream = await thumb.open_read_async()
        tamanho = int(stream.size)
        leitor = DataReader(stream)
        await leitor.load_async(tamanho)
        dados = bytearray(tamanho)
        leitor.read_bytes(dados)
        imagem = Image.open(io.BytesIO(bytes(dados)))
        imagem.load()
        return cor_dominante(imagem), imagem
    except Exception as e:
        print(f"Não consegui ler a capa: {e}")
        return COR_PADRAO, None


def posicao_e_duracao(sessao, tocando):
    """Devolve (posição, duração) em segundos; (0, 0) se o player não informar.

    Alguns players (YouTube Music, por exemplo) só atualizam a timeline de
    vez em quando; enquanto toca, extrapolamos com o tempo desde a última
    atualização. Quando o player não informa ``end_time``, usamos
    ``max_seek_time`` como duração.
    """
    try:
        tl = sessao.get_timeline_properties()
        inicio = tl.start_time.total_seconds()
        fim = tl.end_time.total_seconds()
        if fim <= inicio:
            fim = tl.max_seek_time.total_seconds()
        dur = fim - inicio
        if dur <= 0:
            return 0, 0

        pos = tl.position.total_seconds() - inicio
        if tocando:
            try:
                # Ignora relógios malucos (datas anteriores/adiantadas demais).
                atraso = (datetime.now(timezone.utc) - tl.last_updated_time).total_seconds()
                if 0 <= atraso <= 600:
                    pos += atraso
            except Exception:
                pass
        pos = max(0, min(pos, dur))
        return int(pos), int(dur)
    except Exception:
        return 0, 0


def sessao_de_midia(gerente):
    """Sessão atual; se vazia, tenta alguma que esteja tocando."""
    sessao = gerente.get_current_session()
    if sessao is not None:
        return sessao
    for alternativa in gerente.get_sessions():
        try:
            if alternativa.get_playback_info().playback_status == Status.PLAYING:
                return alternativa
        except Exception:
            continue
    return None


# ---------------------- Serial com reconexão ----------------------

# VIDs comuns de placas/adaptadores Arduino (Arduino, CH340, FTDI, CP210x).
_VIDS_ARDUINO = {0x2341, 0x2A03, 0x1A86, 0x0403, 0x10C4, 0x0843, 0x1B4F}


def portas_candidatas():
    """Lista as portas que podem ser o Arduino, as mais prováveis primeiro.

    Portas legadas sem VID USB (COM1, por exemplo) ficam de fora: abrir uma
    delas "funciona", mas não existe placa nenhuma do outro lado. Era isso
    que fazia o app largar a COM3 e conectar na COM1 após um timeout.
    """
    candidatas = []
    for p in list_ports.comports():
        texto = f"{p.description or ''} {p.manufacturer or ''}".lower()
        hwid = (p.hwid or "").lower()
        if p.vid is None:
            continue
        if "acpi" in hwid or "communications port" in texto:
            continue

        nota = 0
        if "arduino" in texto:
            nota -= 40
        if p.vid in _VIDS_ARDUINO:
            nota -= 20
        if "usb" in texto:
            nota -= 10
        if "serial" in texto:
            nota -= 5
        candidatas.append((nota, p.device))

    candidatas.sort(key=lambda item: (item[0], item[1]))
    return [device for _, device in candidatas]


def _descartar_arduino():
    """Fecha a porta atual liberando o handle do Windows.

    Limpa os buffers e fecha mesmo que o handle esteja em mau estado: sem
    isso, após um timeout a COM pode continuar presa e nenhuma reconexão
    consegue abrir a porta de novo.
    """
    global arduino
    porta = arduino
    arduino = None
    est["arduino_ok"] = False
    if porta is None:
        return
    for acao in (
        getattr(porta, "reset_output_buffer", None),
        getattr(porta, "reset_input_buffer", None),
        getattr(porta, "close", None),
    ):
        try:
            if acao is not None:
                acao()
        except Exception:
            pass


def fechar_tudo():
    """Encerra a conexão serial antes de sair (libera a COM na hora)."""
    _descartar_arduino()


def _agendar_arduino():
    """Garante que exista uma tarefa de reconexão rodando em segundo plano."""
    global tarefa_arduino_ref
    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        return
    if tarefa_arduino_ref is None or tarefa_arduino_ref.done():
        tarefa_arduino_ref = loop.create_task(abrir_arduino())


async def _esperar_pronto(porta, timeout=10.0):
    """Espera o sketch terminar o setup (linha READY).

    O Arduino reinicia quando a porta serial abre e o setup atual leva ~3 s
    (tela de boot). Sem esperar, os primeiros comandos enchem o buffer e o
    app fecha/reabre a porta, reiniciando a placa em loop.
    """
    limite = time.monotonic() + timeout
    buf = b""
    while time.monotonic() < limite:
        try:
            n = porta.in_waiting
            if n:
                buf += porta.read(n)
                if b"READY" in buf:
                    return True
                if len(buf) > 2048:
                    buf = buf[-2048:]
        except (serial.SerialException, OSError):
            return False
        await asyncio.sleep(0.05)
    return False


async def abrir_arduino():
    """Abre (ou reabre) a porta, detectando o Arduino automaticamente."""
    global arduino, geracao, PORTA
    _descartar_arduino()
    # Dá tempo do Windows/driver liberar o handle após um timeout.
    await asyncio.sleep(0.5)

    while arduino is None:
        candidatas = []
        # Só tenta uma porta configurada manualmente ou que ainda exista.
        configurada = cfg.get("porta") or ""
        if configurada:
            candidatas.append(configurada)
        for porta in portas_candidatas():
            if porta not in candidatas:
                candidatas.append(porta)
        if PORTA and PORTA in candidatas:
            candidatas.remove(PORTA)
            candidatas.insert(0, PORTA)

        for nome in candidatas:
            porta = None
            try:
                porta = serial.Serial(nome, BAUD, timeout=0, write_timeout=1)
                try:
                    porta.reset_input_buffer()
                    pronto = await _esperar_pronto(porta)
                except asyncio.CancelledError:
                    raise
                if not pronto:
                    print(f"{nome}: READY não recebido; conectando mesmo assim.")
                arduino = porta
                porta = None            # adotada: não fechar no finally
                PORTA = nome
                est["porta"] = nome
                geracao += 1
                est["arduino_ok"] = True
                print(f"Arduino conectado em {nome}.")
                break
            except (serial.SerialException, OSError):
                continue
            finally:
                # Fecha qualquer porta que abriu mas não foi adotada. Era
                # isto que deixava a COM presa quando o reset do buffer ou a
                # espera pelo READY falhava.
                if porta is not None:
                    try:
                        porta.close()
                    except Exception:
                        pass

        if arduino is None:
            est["porta"] = ""
            print("Sem Arduino. Tentando de novo em 3 s...")
            await asyncio.sleep(3)


async def enviar(dados):
    """Envia dados. Se a porta falhar, agenda a reconexão e devolve False.

    Não espera a reconexão terminar: assim a interface e as tarefas de mídia
    continuam funcionando mesmo com o Arduino desconectado.
    """
    if arduino is None:
        _agendar_arduino()
        return False
    try:
        arduino.write(dados)
        if DEBUG_SERIAL:
            print(f"  -> {dados!r}")
        return True
    except (serial.SerialException, OSError) as e:
        print(f"Falha ao enviar ({e}). Reconectando em segundo plano...")
        _descartar_arduino()
        _agendar_arduino()
        return False


# ---------------------- OpenRGB ----------------------

def conectar_openrgb():
    from openrgb import OpenRGBClient
    cliente = OpenRGBClient(address=OPENRGB_HOST, port=OPENRGB_PORT, name="Musica LED")
    devs = []
    for d in cliente.devices:
        try:
            d.set_mode("direct")
        except Exception:
            pass
        devs.append(d)
    return devs


class SaidaOpenRGB(threading.Thread):
    """Envia a cor mais recente aos dispositivos, sem travar o script principal."""

    def __init__(self, devs):
        super().__init__(daemon=True)
        self.devs = devs
        self.cor = (0, 0, 0)
        self.evento = threading.Event()
        self.parar_evento = threading.Event()
        self.falhou = False

    def enviar(self, cor):
        self.cor = cor
        self.evento.set()

    def parar(self):
        self.parar_evento.set()
        self.evento.set()

    def run(self):
        from openrgb.utils import RGBColor
        ultima = None
        while not self.parar_evento.is_set():
            self.evento.wait(timeout=0.5)
            self.evento.clear()
            if self.parar_evento.is_set():
                return
            cor = self.cor
            if cor == ultima:
                continue
            ultima = cor
            rgb = RGBColor(*cor)
            for d in self.devs:
                try:
                    d.set_color(rgb, fast=True)
                except Exception as e:
                    print(f"OpenRGB caiu ({e}). Vou tentar reconectar.")
                    self.falhou = True
                    return


async def tarefa_openrgb(est):
    """Conecta ao servidor OpenRGB e reconecta sozinho se ele cair."""
    global saida_pc
    avisou = False
    while True:
        ativo = bool(cfg.get("usar_openrgb"))

        if not ativo:
            if saida_pc is not None:
                saida_pc.parar()
                saida_pc = None
            est["openrgb_ok"] = False
            avisou = False
        elif saida_pc is None or saida_pc.falhou:
            if saida_pc is not None:
                saida_pc.parar()
                saida_pc = None
            try:
                devs = await asyncio.to_thread(conectar_openrgb)
                saida_pc = SaidaOpenRGB(devs)
                saida_pc.start()
                est["openrgb_ok"] = True
                avisou = False
                print(f"OpenRGB conectado ({len(devs)} dispositivos).")
            except Exception as e:
                est["openrgb_ok"] = False
                if not avisou:
                    print(f"OpenRGB indisponível ({e}). Tentando de novo...")
                    avisou = True

        await asyncio.sleep(5)


# ---------------------- Detector de batida ----------------------

def escolher_saida(sc):
    """Escolhe a saída de áudio pelo nome; se não achar, usa a padrão."""
    alvo = DISPOSITIVO_AUDIO.strip().lower()
    if alvo:
        for s in sc.all_speakers():
            if alvo in s.name.lower():
                return s
        print(f"Batida: não achei '{DISPOSITIVO_AUDIO}'. Usando a saída padrão.")
    return sc.default_speaker()

class DetectorBatida(threading.Thread):
    SR = 44100
    BLOCO = 1024                    # ~23 ms por bloco
    N_BANDAS = 64

    def __init__(self):
        super().__init__(daemon=True)
        self.batida = threading.Event()
        self.ativo = False          # só detecta batidas quando True
        self.disponivel = True      # False se faltarem as bibliotecas
        self.espectro = [0.0] * self.N_BANDAS
        self.reiniciar = False      # força reabrir o dispositivo de áudio

    def run(self):
        try:
            import numpy as np
            import soundcard as sc
        except Exception as e:
            print(f"Batida indisponível ({e}). Instale com: pip install numpy soundcard")
            self.disponivel = False
            return

        print("Saídas de áudio disponíveis:")
        for s in sc.all_speakers():
            print(f"  - {s.name}")

        janela = np.hanning(self.BLOCO)
        freqs = np.fft.rfftfreq(self.BLOCO, 1.0 / self.SR)

        # bandas logarítmicas para o espectro visual
        bordas = np.geomspace(40.0, min(16000.0, self.SR / 2), self.N_BANDAS + 1)
        indices = []
        for a, b in zip(bordas[:-1], bordas[1:]):
            i0 = int(np.searchsorted(freqs, a))
            i1 = max(i0 + 1, int(np.searchsorted(freqs, b)))
            indices.append((i0, i1))

        hist = deque(maxlen=int(self.SR / self.BLOCO))      # ~1 s de histórico
        anterior = 0.0
        ultima = 0.0
        contador = 0
        suave = np.zeros(self.N_BANDAS)

        while True:
            self.reiniciar = False
            try:
                alto_falante = escolher_saida(sc)
                mic = sc.get_microphone(id=str(alto_falante.name), include_loopback=True)
                print(f"Batida: ouvindo '{alto_falante.name}'")
                with mic.recorder(samplerate=self.SR, channels=2, blocksize=self.BLOCO) as rec:
                    while not self.reiniciar:
                        dados = rec.record(numframes=self.BLOCO)
                        mono = dados.mean(axis=1)
                        esp = np.abs(np.fft.rfft(mono * janela)) / self.BLOCO

                        # --- espectro real (sempre atualizado) ---
                        bandas = np.array([
                            np.sqrt(np.mean(esp[i0:i1] ** 2)) for i0, i1 in indices
                        ])
                        db = 20.0 * np.log10(bandas + 1e-9)
                        nivel = np.clip((db + 72.0) / 46.0, 0.0, 1.0)
                        suave = np.maximum(nivel, suave * 0.86)   # ataque rápido/queda suave
                        self.espectro = [float(v) for v in suave]

                        # --- batida (só quando ligada) ---
                        if not self.ativo:
                            continue

                        faixa = (freqs >= GRAVES_HZ[0]) & (freqs <= GRAVES_HZ[1])
                        energia = float(esp[faixa].sum())

                        media = None
                        if len(hist) >= 10:
                            media = sum(hist) / len(hist)
                        hist.append(energia)

                        agora = time.perf_counter()
                        if media is not None:
                            if (energia > media * SENSIBILIDADE
                                    and energia > PISO
                                    and energia > anterior
                                    and agora - ultima > INTERVALO_MIN):
                                ultima = agora
                                self.batida.set()
                                if DEBUG_BATIDA:
                                    print(f"  batida  energia={energia:.4f}  media={media:.4f}")
                        anterior = energia

                        if DEBUG_BATIDA:
                            contador += 1
                            if contador % 43 == 0:
                                print(f"  nível dos graves: {energia:.4f}")
            except Exception as e:
                print(f"Batida: erro de áudio ({e}). Tentando de novo em 3 s...")
                time.sleep(3)


# ---------------------- Tarefa: música ----------------------

def timeline_desatualizada(sessao, referencia):
    """True quando a timeline ainda é a da faixa anterior (ou vazia).

    O YouTube Music (Chromium) publica a posição/duração no SMTC só em
    eventos de play/pause/seek. Ao trocar de faixa, a timeline antiga fica
    congelada e a barra de progresso mostraria os valores da música anterior.
    """
    try:
        tl = sessao.get_timeline_properties()
        dur = (tl.end_time - tl.start_time).total_seconds()
        if dur <= 0:
            return True
        return tl.last_updated_time < referencia - timedelta(seconds=5)
    except Exception:
        return False


async def refrescar_timeline(sessao):
    """Dá um toque de play/pause para o player publicar a timeline da faixa."""
    try:
        if sessao.get_playback_info().playback_status != Status.PLAYING:
            return
        await sessao.try_toggle_play_pause_async()
        pausou = False
        for _ in range(20):                 # espera até 2 s pela pausa
            await asyncio.sleep(0.1)
            if sessao.get_playback_info().playback_status != Status.PLAYING:
                pausou = True
                break
        if not pausou:
            return                          # o player ignorou; nada a desfazer
        for _ in range(5):                  # retoma conferindo o estado
            await sessao.try_toggle_play_pause_async()
            await asyncio.sleep(0.15)
            if sessao.get_playback_info().playback_status == Status.PLAYING:
                break
    except Exception as e:
        print(f"Não consegui atualizar a timeline: {e}")


async def tarefa_midia(est):
    gerente = await MediaManager.request_async()
    ultima_faixa = None
    momento_faixa = None
    faixa_refrescada = None
    ultimo_status = None
    ultimo_s = 0.0
    gen = geracao

    while True:
        if gen != geracao:                  # Arduino reconectou: reenvia tudo
            gen = geracao
            ultima_faixa = None
            ultimo_status = None
            ultimo_s = 0.0

        sessao = sessao_de_midia(gerente)

        if sessao is None:
            est["tocando"] = False
            est["titulo"] = ""
            est["artista"] = ""
            est["pos"] = est["dur"] = 0
            if est["capa"] is not None:
                est["capa"] = None
                est["capa_seq"] += 1
            if ultimo_status != "nada":
                if await enviar(b"T,\nA,\nS,0\nP,0,0\nI,1\n"):
                    ultimo_status = "nada"
                    ultima_faixa = None
                    ultimo_s = time.monotonic()
            await asyncio.sleep(1)
            continue

        est["viu_musica"] = True
        props = await sessao.try_get_media_properties_async()
        bruto_titulo = (props.title or "").strip()
        bruto_artista = (props.artist or "").strip()
        titulo = bruto_titulo or "Untitled"         # a interface mostra o original
        artista = bruto_artista
        # O OLED só entende ASCII: romanizamos japonês/coreano só para ele.
        titulo_oled = romanizar(bruto_titulo) or "Tocando"
        artista_oled = romanizar(bruto_artista)
        tocando = sessao.get_playback_info().playback_status == Status.PLAYING
        est["tocando"] = tocando
        est["titulo"] = titulo
        est["artista"] = artista

        faixa = (bruto_titulo, bruto_artista)
        if faixa != ultima_faixa:
            momento_faixa = datetime.now(timezone.utc)
            faixa_refrescada = None
            await asyncio.sleep(1)          # dá tempo da capa nova carregar
            props = await sessao.try_get_media_properties_async()
            bruto_titulo = (props.title or "").strip()
            bruto_artista = (props.artist or "").strip()
            titulo = bruto_titulo or "Untitled"
            artista = bruto_artista
            titulo_oled = romanizar(bruto_titulo) or "Tocando"
            artista_oled = romanizar(bruto_artista)
            faixa = (bruto_titulo, bruto_artista)
            est["titulo"] = titulo
            est["artista"] = artista
            cor, capa = await cor_do_album(props.thumbnail)
            r, g, b = cor
            # A capa e a cor também valem para a interface quando o Arduino
            # está desconectado; o OLED recebe tudo de novo ao reconectar.
            est["cor_alvo"] = (r, g, b)
            est["capa"] = capa
            est["capa_seq"] += 1
            ultima_faixa = faixa
            ultimo_status = None
            ultimo_s = 0.0
            await enviar(f"T,{titulo_oled}\nA,{artista_oled}\nC,{r},{g},{b}\nI,0\n".encode())
            print(f"{artista_oled or '-'} - {titulo_oled}  ->  cor ({r}, {g}, {b})")

        # Se a timeline ainda for a da faixa anterior, um toque de
        # play/pause faz o player publicar posição e duração corretas.
        if tocando and faixa != faixa_refrescada and momento_faixa is not None:
            for _ in range(2):
                if not timeline_desatualizada(sessao, momento_faixa):
                    break
                await refrescar_timeline(sessao)
                await asyncio.sleep(0.3)
            faixa_refrescada = faixa

        pos, dur = posicao_e_duracao(sessao, tocando)
        est["pos"], est["dur"] = pos, dur
        await enviar(f"P,{pos},{dur}\n".encode())

        # Reenvia S de tempos em tempos, não só quando muda: se o Arduino
        # perder a linha durante um redesenho do OLED, ele ficaria achando
        # que está pausado e a batida não acenderia o LED.
        if tocando != ultimo_status or time.monotonic() - ultimo_s >= 1.5:
            if await enviar(f"S,{1 if tocando else 0}\n".encode()):
                ultimo_status = tocando
                ultimo_s = time.monotonic()

        await asyncio.sleep(1)


# ---------------------- Tarefas auxiliares ----------------------

DIAS_SEMANA = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")


async def tarefa_relogio(est):
    """Mantém o relógio do OLED sincronizado (o Arduino não tem RTC)."""
    gen = -1
    ultimo = 0.0
    while True:
        # Reenvia logo que o Arduino reconecta e depois a cada 10 s.
        if gen != geracao or time.monotonic() - ultimo >= 10:
            agora = datetime.now()
            dados = (
                f"K,{agora:%H:%M:%S}\n"
                f"J,{agora:%d/%m/%Y}\n"
                f"W,{DIAS_SEMANA[agora.weekday()]}\n"
            )
            if await enviar(dados.encode()):
                gen = geracao
                ultimo = time.monotonic()
        await asyncio.sleep(1)


async def tarefa_controle(est):
    """Executa play/pause, próxima e anterior pedidos pela interface."""
    gerente = await MediaManager.request_async()
    while True:
        try:
            acao = await asyncio.to_thread(controle_fila.get)
        except Exception:
            await asyncio.sleep(0.2)
            continue

        sessao = sessao_de_midia(gerente)
        if sessao is None:
            print(f"Controle '{acao}': nenhuma sessão de mídia.")
            continue
        try:
            resultado = None
            antes = sessao.get_playback_info().playback_status
            if acao == "playpause":
                resultado = await sessao.try_toggle_play_pause_async()
            elif acao == "next":
                resultado = await sessao.try_skip_next_async()
            elif acao == "prev":
                resultado = await sessao.try_skip_previous_async()
            elif isinstance(acao, str) and acao.startswith("seek:"):
                # A projeção Python do WinRT recebe TimeSpan como Int64 em
                # ticks de 100 ns (não como timedelta nem como segundos).
                # O YT Music expõe start_time=0, mas outros players podem
                # usar outra origem, então somamos o início da timeline.
                tl = sessao.get_timeline_properties()
                inicio = tl.start_time.total_seconds()
                fim = tl.end_time.total_seconds()
                if fim <= inicio:
                    fim = tl.max_seek_time.total_seconds()
                alvo = float(acao[5:])
                minimo = max(0.0, tl.min_seek_time.total_seconds() - inicio)
                maximo = max(0.0, fim - inicio)
                alvo = min(max(alvo, minimo), maximo) if maximo > 0 else alvo
                ticks = int(round((inicio + alvo) * 10_000_000))
                resultado = await sessao.try_change_playback_position_async(ticks)
            await asyncio.sleep(0.35)
            depois = sessao.get_playback_info().playback_status
            print(f"Controle '{acao}': {resultado} ({antes} -> {depois})")
        except Exception as e:
            print(f"Controle de mídia falhou ({e}).")


async def tarefa_config(est, detector):
    """Aplica mudanças feitas no painel de configurações."""
    global saida_pc, tarefa_arduino_ref
    seq = 0
    while True:
        if est.get("settings_seq", 0) != seq:
            seq = est["settings_seq"]
            aplicar_config()
            if est.pop("reconectar_arduino", False):
                if tarefa_arduino_ref and not tarefa_arduino_ref.done():
                    tarefa_arduino_ref.cancel()
                    try:
                        await tarefa_arduino_ref
                    except asyncio.CancelledError:
                        pass
                _agendar_arduino()
            if est.pop("reconectar_audio", False):
                detector.reiniciar = True
            if est.pop("reconectar_openrgb", False):
                if saida_pc is not None:
                    saida_pc.parar()
                    saida_pc = None
            if est.pop("reload_hotkeys", False):
                registrar_atalhos()
        await asyncio.sleep(0.4)


_atalhos = []


def _atalho_batida():
    print("Atalho: alternar batida")
    pedido_toggle.set()


def _atalho_controle(acao):
    print(f"Atalho: {acao}")
    controle_fila.put(acao)


def registrar_atalhos():
    """(Re)registra os atalhos globais conforme a configuração."""
    global _atalhos
    try:
        import keyboard
    except Exception:
        print("Atalhos globais indisponíveis (pip install keyboard).")
        return

    for handle in _atalhos:
        try:
            keyboard.remove_hotkey(handle)
        except Exception:
            pass
    _atalhos = []

    pares = (
        (cfg.get("tecla_batida"), _atalho_batida, ()),
        (cfg.get("tecla_play"), _atalho_controle, ("playpause",)),
        (cfg.get("tecla_prox"), _atalho_controle, ("next",)),
        (cfg.get("tecla_ant"), _atalho_controle, ("prev",)),
    )
    for tecla, funcao, args in pares:
        if not tecla:
            continue
        try:
            _atalhos.append(keyboard.add_hotkey(tecla, funcao, args=args))
            print(f"Atalho registrado: {tecla}")
        except Exception as e:
            print(f"Atalho '{tecla}' indisponível ({e}).")


# ---------------------- Tarefa: LED (fade, gama e batida) ----------------------

async def tarefa_render(est, detector):
    global saida_pc
    batida_ativa = BATIDA_INICIAL
    modo_enviado = None
    reforco_modo = 0.0
    ultimo_m = 0.0
    gen = geracao

    cor_atual = [float(c) for c in est["cor_alvo"]]
    bri_atual = 0.0
    flash = 0.0
    t_ant = time.perf_counter()

    while True:
        agora = time.perf_counter()
        dt = min(agora - t_ant, 0.25)
        t_ant = agora

        if gen != geracao:
            gen = geracao
            modo_enviado = None
            # Reforça o modo de batida 1,5 s depois da conexão. Se o primeiro
            # M,1 se perdeu durante o boot, este segundo chega com a placa pronta.
            reforco_modo = time.monotonic() + 1.5
            ultimo_m = 0.0

        # --- liga/desliga a batida (atalho global ou tecla B no terminal) ---
        if est.get("modo_console", True):
            while msvcrt.kbhit():
                if msvcrt.getwch().lower() == "b":
                    pedido_toggle.set()
        if pedido_toggle.is_set():
            pedido_toggle.clear()
            if detector.disponivel:
                batida_ativa = not batida_ativa
                flash = 0.0
                print(f"Batida {'LIGADA' if batida_ativa else 'DESLIGADA'}")
            else:
                print("Batida indisponível: instale numpy e soundcard.")

        efetiva = batida_ativa and detector.disponivel
        detector.ativo = efetiva
        est["batida_ativa"] = batida_ativa
        est["batida_disponivel"] = detector.disponivel
        est["espectro"] = detector.espectro
        if efetiva != modo_enviado:
            if await enviar(f"M,{1 if efetiva else 0}\n".encode()):
                modo_enviado = efetiva
                ultimo_m = time.monotonic()
                print(f"Batida no Arduino: {'ON' if efetiva else 'OFF'}")
        elif reforco_modo and time.monotonic() >= reforco_modo:
            if await enviar(f"M,{1 if efetiva else 0}\n".encode()):
                reforco_modo = 0.0
                ultimo_m = time.monotonic()
                print(f"Batida no Arduino (reforço): {'ON' if efetiva else 'OFF'}")
        elif time.monotonic() - ultimo_m >= 5.0:
            # Reafirma o modo de tempos em tempos caso a placa tenha perdido
            # a linha (buffer cheio durante o redesenho do OLED).
            if await enviar(f"M,{1 if efetiva else 0}\n".encode()):
                ultimo_m = time.monotonic()

        tocando = est["tocando"]

        # --- batida detectada ---
        if detector.batida.is_set():
            detector.batida.clear()
            if efetiva and tocando:
                flash = 1.0
                ok = await enviar(b"B\n")
                if DEBUG_BATIDA:
                    print(f"  B -> Arduino: {ok}")
            elif DEBUG_BATIDA:
                print(f"  batida ignorada (efetiva={efetiva}, tocando={tocando})")

        # --- fade de cor e de brilho (mesma suavidade do Arduino) ---
        k = 1.0 - math.exp(-dt / SUAVIDADE)
        alvo = est["cor_alvo"]
        for i in range(3):
            cor_atual[i] += (alvo[i] - cor_atual[i]) * k
        bri_alvo = 1.0 if tocando else PAUSA_BRILHO_PC
        bri_atual += (bri_alvo - bri_atual) * k
        flash *= math.exp(-dt / BATIDA_DECAIMENTO)
        est["flash"] = flash if (efetiva and tocando) else 0.0

        # --- cor final para o OpenRGB: gama + calibração + brilho ---
        if saida_pc is not None and est["viu_musica"]:
            mult = bri_atual
            if efetiva and tocando:
                mult *= BATIDA_MIN_PC + (1.0 - BATIDA_MIN_PC) * flash
            cor = tuple(
                max(0, min(255, int(255.0 * (max(cor_atual[i], 0.0) / 255.0) ** GAMMA_PC
                                    * CAL_PC[i] * mult + 0.5)))
                for i in range(3)
            )
            saida_pc.enviar(cor)

        await asyncio.sleep(TICK)


# ---------------------- Principal ----------------------

async def principal():
    global detector_ref, tarefa_arduino_ref
    # Carrega o romanizador em segundo plano (não trava o loop de LED).
    await asyncio.to_thread(_preparar_romanizador)

    # A conexão com o Arduino roda em segundo plano para a interface abrir já.
    _agendar_arduino()

    detector = DetectorBatida()
    detector.start()
    est["batida_disponivel"] = detector.disponivel
    detector_ref = detector

    registrar_atalhos()
    print("Atalhos globais registrados.")

    await asyncio.gather(
        tarefa_midia(est),
        tarefa_render(est, detector),
        tarefa_openrgb(est),
        tarefa_relogio(est),
        tarefa_controle(est),
        tarefa_config(est, detector),
    )


def _log_em_arquivo():
    """Em modo janela não existe console: manda os prints para um log."""
    if sys.stdout is not None and sys.stderr is not None:
        return
    try:
        pasta = os.path.dirname(os.path.abspath(sys.executable))
        arquivo = open(
            os.path.join(pasta, "huebeat.log"), "a", encoding="utf-8", buffering=1
        )
    except Exception:
        try:
            arquivo = open(
                os.path.join(os.path.expanduser("~"), "huebeat.log"),
                "a", encoding="utf-8", buffering=1,
            )
        except Exception:
            return
    if sys.stdout is None:
        sys.stdout = arquivo
    if sys.stderr is None:
        sys.stderr = arquivo


def _rodar_asyncio():
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    try:
        loop.run_until_complete(principal())
    except Exception as e:
        import traceback

        traceback.print_exc()
        print(f"Loop principal parou: {e}")


def _iniciar_console():
    est["modo_console"] = True
    try:
        asyncio.run(principal())
    except KeyboardInterrupt:
        print("Encerrado.")


def _iniciar_interface():
    """A interface gráfica agora é o app Tauri (pasta ``ui/``).

    Rodar este módulo diretamente cai no modo console, útil para depurar o
    motor sem abrir a janela.
    """
    _iniciar_console()


_mutex_instancia = None


def instancia_unica():
    """Garante uma só cópia do app: duas instâncias brigariam pela COM3 e
    pelos atalhos globais."""
    global _mutex_instancia
    if os.name != "nt":
        return True
    try:
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.windll.kernel32
        kernel32.CreateMutexW.restype = wintypes.HANDLE
        kernel32.CreateMutexW.argtypes = [
            wintypes.LPVOID, wintypes.BOOL, wintypes.LPCWSTR
        ]
        _mutex_instancia = kernel32.CreateMutexW(
            None, False, "HueBeat_SingleInstance"
        )
        if not _mutex_instancia:
            return True
        if kernel32.GetLastError() == 183:      # ERROR_ALREADY_EXISTS
            return False
        return True
    except Exception:
        return True


def _trazer_instancia_existente():
    try:
        import ctypes

        u = ctypes.windll.user32
        h = u.FindWindowW(None, "HueBeat")
        if h:
            u.ShowWindow(h, 9)
            u.SetForegroundWindow(h)
    except Exception:
        pass


if __name__ == "__main__":
    _log_em_arquivo()
    if "--console" in sys.argv or os.environ.get("MUSICLED_CONSOLE") == "1":
        _iniciar_console()
    else:
        if not instancia_unica():
            _trazer_instancia_existente()
            sys.exit(0)
        _iniciar_interface()