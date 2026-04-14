# install.ps1 — oxsh installer for Windows
# Run: powershell -ExecutionPolicy Bypass -File install.ps1
$ErrorActionPreference = "Stop"

$BinaryName = "oxsh.exe"
$InstallDir = "$env:USERPROFILE\.cargo\bin"  # Same as cargo install

Write-Host "`n==> Building oxsh (release)..." -ForegroundColor Cyan

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host " ✗  cargo not found. Install Rust first: https://rustup.rs" -ForegroundColor Red
    exit 1
}

cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Host " ✗  Build failed" -ForegroundColor Red
    exit 1
}

$Binary = "target\release\$BinaryName"
if (-not (Test-Path $Binary)) {
    Write-Host " ✗  $Binary not found" -ForegroundColor Red
    exit 1
}

$size = (Get-Item $Binary).Length / 1MB
Write-Host " ✓  Build successful ($([math]::Round($size, 1))MB binary)" -ForegroundColor Green

Write-Host "`n==> Installing to $InstallDir..." -ForegroundColor Cyan
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}
Copy-Item $Binary "$InstallDir\$BinaryName" -Force
Write-Host " ✓  Installed $InstallDir\$BinaryName" -ForegroundColor Green

Write-Host "`n==> Running first-time setup..." -ForegroundColor Cyan
& "$InstallDir\$BinaryName" --setup

Write-Host "`n" -NoNewline
Write-Host "oxsh installed successfully!" -ForegroundColor Green
Write-Host ""
Write-Host "  Binary:  $InstallDir\$BinaryName"
Write-Host "  Config:  $env:APPDATA\oxsh\config.toml"
Write-Host ""
Write-Host "To use oxsh, run: oxsh"
Write-Host ""
