# Builds the HueBeat Tauri app: Python sidecar + Rust shell + web UI.
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

Push-Location $root
try {
    Write-Host "==> Building the Python sidecar"
    python -m PyInstaller --noconfirm --clean `
        --distpath "$root\ui\src-tauri\binaries" `
        --workpath "$root\build_backend" `
        "$root\HueBeatBackend.spec"
    if ($LASTEXITCODE -ne 0) { throw "sidecar build failed" }

    Push-Location "$root\ui"
    try {
        Write-Host "==> Installing UI dependencies"
        npm install
        if ($LASTEXITCODE -ne 0) { throw "npm install failed" }

        Write-Host "==> Building the Tauri app"
        npx tauri build
        if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }
    }
    finally {
        Pop-Location
    }
}
finally {
    Pop-Location
}

Write-Host "Done. Installers are in ui\src-tauri\target\release\bundle\nsis"
