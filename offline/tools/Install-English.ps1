[CmdletBinding()]
param([string] $ClientRoot = (Join-Path $PSScriptRoot '..\..\eco_jp'))

# Builds the English table archive from the reviewed batches and installs it into the client.
# The originals are copied to offline/backups/english-<time> first; copy them back to undo.
$ErrorActionPreference = 'Stop'
$offline = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$ClientRoot = [IO.Path]::GetFullPath($ClientRoot)
$xls = Join-Path $ClientRoot 'data\xls'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$build = Join-Path $offline "translations\build\english-$stamp"
$backup = Join-Path $offline "backups\english-$stamp"

if (Get-Process -Name eco -ErrorAction SilentlyContinue) { throw 'Close the game first.' }
$exporter = Join-Path $offline 'target\export-client-text.exe'
if (-not (Test-Path -LiteralPath $exporter -PathType Leaf)) {
    & rustc --edition=2021 -O (Join-Path $PSScriptRoot 'Export-ClientText.rs') -o $exporter
    if ($LASTEXITCODE -ne 0) { throw 'Could not compile Export-ClientText.rs (is Rust installed?).' }
}

$batches = Get-Content -Raw (Join-Path $offline 'translations\reviewed-batches.json') | ConvertFrom-Json
$inputs = @($batches.files | ForEach-Object { Join-Path (Split-Path $offline) $_.path })
$inputs += Join-Path $offline 'translations\font-overrides.jsonl'
$arguments = @((Join-Path $PSScriptRoot 'Build-EnglishUi.py'), $ClientRoot, '--output', $build)
foreach ($file in $inputs) { $arguments += '--translations'; $arguments += $file }
& python @arguments
if ($LASTEXITCODE -ne 0) { throw 'The English build failed; the client was not changed.' }

New-Item -ItemType Directory -Path $backup | Out-Null
foreach ($name in 'table.hed', 'table.dat') {
    $target = Join-Path $xls $name
    Copy-Item -LiteralPath $target -Destination $backup
    Set-ItemProperty -LiteralPath $target -Name IsReadOnly -Value $false
    Copy-Item -LiteralPath (Join-Path $build $name) -Destination $target -Force
}
Write-Host "English installed. Originals backed up to $backup"
