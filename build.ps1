$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

Push-Location (Join-Path $root "web")
try {
    npm ci
    npm run build
} finally {
    Pop-Location
}

Push-Location $root
try {
    cargo build --release
    New-Item -ItemType Directory -Force -Path "dist" | Out-Null
    Copy-Item "target/release/openllm.exe" "dist/openllm.exe" -Force
    Write-Host ""
    Write-Host "Built: $root\dist\openllm.exe"
} finally {
    Pop-Location
}

