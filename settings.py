"""Configuração persistente do HueBeat.

Guarda as preferências em ``%APPDATA%\\HueBeat\\settings.json`` para que o
executável continue funcionando mesmo instalado em uma pasta somente leitura.
"""

import json
import os

APP = "HueBeat"

DEFAULTS = {
    # Arduino
    "porta": "",                 # "" = detectar automaticamente
    "baud": 115200,
    # OpenRGB
    "usar_openrgb": True,
    "openrgb_host": "127.0.0.1",
    "openrgb_port": 6742,
    # Cor
    "cor_padrao": [255, 120, 0],
    "sat_min": 0.85,
    "realce_sat": 1.4,
    # Brilho / suavidade
    "pausa_brilho_pc": 0.15,
    "suavidade": 0.12,
    "gamma_pc": 2.2,
    "cal_pc": [1.0, 1.0, 1.0],
    # Batida
    "batida_inicial": True,
    "batida_min_pc": 0.25,
    "batida_decaimento": 0.12,
    "graves_hz": [40, 150],
    "sensibilidade": 1.2,
    "piso": 0.003,
    "intervalo_min": 0.22,
    "dispositivo_audio": "",
    # Atalhos
    "tecla_batida": "ctrl+alt+b",
    "tecla_play": "ctrl+alt+space",
    "tecla_prox": "ctrl+alt+right",
    "tecla_ant": "ctrl+alt+left",
    # Janela / bandeja
    "iniciar_minimizado": False,
    "fechar_para_bandeja": True,
}


def pasta_config():
    base = os.environ.get("APPDATA") or os.path.expanduser("~")
    return os.path.join(base, APP)


class Settings:
    def __init__(self):
        self.path = os.path.join(pasta_config(), "settings.json")
        self.data = dict(DEFAULTS)
        self.load()

    def load(self):
        try:
            with open(self.path, "r", encoding="utf-8") as f:
                dados = json.load(f)
            if isinstance(dados, dict):
                for chave in DEFAULTS:
                    if chave in dados:
                        self.data[chave] = dados[chave]
        except Exception:
            pass

    def save(self):
        try:
            os.makedirs(os.path.dirname(self.path), exist_ok=True)
            tmp = self.path + ".tmp"
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(self.data, f, indent=2, ensure_ascii=False)
            os.replace(tmp, self.path)
            return True
        except Exception as e:
            print(f"Não consegui salvar a configuração: {e}")
            return False

    def get(self, chave, padrao=None):
        if chave in self.data:
            return self.data[chave]
        if chave in DEFAULTS:
            return DEFAULTS[chave]
        return padrao

    def set(self, chave, valor):
        self.data[chave] = valor

    def reset(self):
        self.data = dict(DEFAULTS)
        self.save()


cfg = Settings()
