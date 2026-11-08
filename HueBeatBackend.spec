# -*- mode: python ; coding: utf-8 -*-
"""Builds the headless backend used as a Tauri sidecar."""

from PyInstaller.utils.hooks import collect_all

datas = []
binaries = []
hiddenimports = [
    "keyboard",
    "openrgb",
    "openrgb.utils",
    "soundcard",
    "soundcard.mediafoundation",
    "numpy",
    "serial",
    "serial.tools",
    "serial.tools.list_ports",
]

for pacote in ("pykakasi",):
    tmp = collect_all(pacote)
    datas += tmp[0]
    binaries += tmp[1]
    hiddenimports += tmp[2]

a = Analysis(
    ["backend_sidecar.py"],
    pathex=[],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=["tkinter", "pystray"],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name="huebeat-backend",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=True,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=["assets/icon.ico"],
)
