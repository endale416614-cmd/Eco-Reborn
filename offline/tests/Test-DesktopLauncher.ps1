$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$launcher = Join-Path $root 'Eco Reborn.exe'
if (-not (Test-Path -LiteralPath $launcher)) { throw 'Build the desktop launcher first.' }
$guard = [Threading.Mutex]::new($false, 'Local\EcoReborn.Launcher')
try {
    try { $free = $guard.WaitOne(0) } catch [Threading.AbandonedMutexException] { $free = $true }
    if (-not $free) { throw 'Close Eco Reborn before running the launcher fixture.' }
    $guard.ReleaseMutex()
} finally { $guard.Dispose() }
$fixture = Join-Path $root ("offline\target\launcher check 'space' " + [Guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path (Join-Path $fixture 'offline\target\release'), (Join-Path $fixture 'eco_jp'))
$fixtureExe = Join-Path $fixture 'Eco Reborn.exe'
Copy-Item -LiteralPath $launcher -Destination $fixtureExe
[IO.File]::WriteAllText((Join-Path $fixture 'eco_jp\eco.exe'), 'test placeholder')
[IO.File]::WriteAllText((Join-Path $fixture 'offline\target\release\eco-reborn.exe'), 'test placeholder')
[IO.File]::WriteAllText((Join-Path $fixture 'offline\Start-EcoReborn.ps1'), @'
Write-Output 'ECO_LAUNCHER_CLIENT_STARTED'
Start-Sleep -Seconds 1
Add-Content -LiteralPath (Join-Path $PSScriptRoot 'fixture-complete') -Value 'done'
exit 0
'@)
foreach ($run in 1..3) {
    $process = Start-Process -FilePath $fixtureExe -WorkingDirectory $env:WINDIR -WindowStyle Hidden -PassThru
    try {
        if (-not $process.WaitForExit(10000)) {
            $process.Kill()
            [void]$process.WaitForExit(5000)
            throw 'The isolated desktop launcher did not finish after its fake session ended.'
        }
        if ($process.ExitCode -ne 0) { throw 'The isolated desktop launcher exited with an error.' }
        if (@(Get-Content -LiteralPath (Join-Path $fixture 'offline\fixture-complete')).Count -ne $run) {
            throw 'The fake session did not reach its normal completion.'
        }
    } finally { $process.Dispose() }
}
Write-Output 'Desktop launcher lifecycle check passed (three fake sessions, spaced path, unrelated working directory).'
