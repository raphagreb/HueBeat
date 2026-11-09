# Development mode: runs the Tauri shell with the native Rust engine.
$ErrorActionPreference = "Stop"

$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
Set-Location (Join-Path $PSScriptRoot "ui")
npx tauri dev
