# Development mode: runs the Tauri shell, which starts backend_sidecar.py.
$ErrorActionPreference = "Stop"

$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
Set-Location (Join-Path $PSScriptRoot "ui")
npx tauri dev
