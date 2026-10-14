[CmdletBinding()]
param([switch] $Check)

$compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$source = Join-Path $PSScriptRoot 'Program.cs'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$output = Join-Path $root 'Eco Reborn.exe'
$icon = Join-Path $PSScriptRoot 'app.ico'
$client = Join-Path $root 'eco_jp\eco.exe'

if (-not (Test-Path -LiteralPath $compiler -PathType Leaf)) {
    throw 'The installed .NET Framework C# compiler was not found.'
}
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw 'The launcher source file was not found.'
}

if (Test-Path -LiteralPath $client -PathType Leaf) {
    Add-Type -AssemblyName System.Drawing
    $clientIcon = [Drawing.Icon]::ExtractAssociatedIcon($client)
    if ($null -ne $clientIcon) {
        $stream = [IO.File]::Create($icon)
        try { $clientIcon.Save($stream) }
        finally { $stream.Dispose(); $clientIcon.Dispose() }
    }
}

if (Test-Path -LiteralPath $icon -PathType Leaf) {
    & $compiler /nologo /target:winexe "/out:$output" /reference:System.Windows.Forms.dll /reference:System.Drawing.dll "/win32icon:$icon" $source
} else {
    & $compiler /nologo /target:winexe "/out:$output" /reference:System.Windows.Forms.dll /reference:System.Drawing.dll $source
}
if ($LASTEXITCODE -ne 0) { throw 'The Eco Reborn launcher did not compile.' }

# Game-window helper: borderless display mode and UI-test screenshots.
$windowTool = Join-Path $PSScriptRoot '..\tools\uitest\EcoWindow.cs'
$windowOut = Join-Path $PSScriptRoot '..\target\eco-window.exe'
& $compiler /nologo /target:exe "/out:$windowOut" /reference:System.Drawing.dll $windowTool
if ($LASTEXITCODE -ne 0) { throw 'The game-window helper did not compile.' }

if ($Check) {
    $checks = Join-Path $PSScriptRoot 'LauncherChecks.cs'
    $checkOutput = Join-Path $env:TEMP ("ECO-Launcher-Checks-{0}.exe" -f $PID)
    try {
        & $compiler /nologo /target:exe /main:LauncherChecks /out:$checkOutput `
            /reference:System.Windows.Forms.dll /reference:System.Drawing.dll $source $checks
        if ($LASTEXITCODE -ne 0) { throw 'The Eco Reborn launcher checks did not compile.' }
        & $checkOutput
        if ($LASTEXITCODE -ne 0) { throw 'The Eco Reborn launcher checks failed.' }
    } finally {
        Remove-Item -LiteralPath $checkOutput -Force -ErrorAction SilentlyContinue
    }
}
