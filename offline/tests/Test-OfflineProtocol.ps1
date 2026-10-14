$ErrorActionPreference = 'Stop'
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$manifest = Join-Path $workspace 'offline\Cargo.toml'
Push-Location $workspace
try {
    & cargo test --offline --locked --manifest-path $manifest headless_three_service_flow
    if ($LASTEXITCODE -ne 0) { throw "Headless three-service protocol test failed (exit code $LASTEXITCODE)." }
} finally {
    Pop-Location
}
