$ErrorActionPreference = 'Stop'
$scriptPath = Join-Path $PSScriptRoot '..\Start-EcoReborn.ps1'
$tokens = $null
$parseErrors = $null
[System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$parseErrors) | Out-Null
if ($parseErrors.Count) { throw ('Launcher parse failed: ' + ($parseErrors -join '; ')) }
. $scriptPath
$script:LauncherMarkers = @()
function Write-Output {
    [CmdletBinding()]
    param([Parameter(ValueFromPipeline = $true)] [object] $InputObject)
    process {
        if ($InputObject -ceq 'ECO_LAUNCHER_CLIENT_STARTED') { $script:LauncherMarkers += $InputObject }
        Microsoft.PowerShell.Utility\Write-Output $InputObject
    }
}

if (-not (Test-OfflineServerListText -Text "Emil Chronicle Online,127.0.0.1:17831,`r`n" -Port 17831)) {
    throw 'The gate endpoint at loopback:17831 was rejected.'
}
if (Test-OfflineServerListText -Text 'Emil Chronicle Online,203.0.113.60:12000' -Port 17831) {
    throw 'A remote endpoint must be rejected.'
}
if (Test-OfflineListenerTable -Listeners @([pscustomobject]@{ Address = [Net.IPAddress]::Any; Port = 17832 }) -Port 17832) {
    throw 'A wildcard listener on the login port must be treated as a collision.'
}
if (Test-OfflineListenerTable -Listeners @([pscustomobject]@{ Address = [Net.IPAddress]::IPv6Any; Port = 17831 }) -Port 17831) {
    throw 'A wildcard listener on the world/gate port must be treated as a collision.'
}
$logPaths = foreach ($service in @('map', 'login', 'world')) { Get-OfflineServerLogPaths -ServiceName $service -SessionId ('a' * 32) }
if ($logPaths.Count -ne 3 -or @($logPaths.Stdout | Sort-Object -Unique).Count -ne 3 -or
    ($logPaths[0].Stdout -notmatch '\\map-a{32}\.out\.log$') -or ($logPaths[1].Stderr -notmatch '\\login-a{32}\.err\.log$')) {
    throw 'Each service must have distinct session-scoped stdout/stderr files, matching report import paths.'
}

$script:StartCount = 0
$script:IsolationFails = $false
$script:BlockedPort = 0
$script:PortChecks = @()
$script:RemoteEndpoint = $false
$script:FailGameStart = $false
$script:FailKill = $false
$script:LoginNotReady = $false
$script:WorldNotReady = $false
$script:MapNotReady = $false
$script:ServerProcesses = @{}
$script:LaunchSpecs = @()
$script:ReadyPorts = @()
$script:ClientWasVisible = $false
$script:ClientExitCode = 0
$script:ServiceStopsDuringGame = $false
$script:DiagnosticStages = @()
$script:LastClientProcess = $null
function Get-OfflineClient {
    [pscustomobject]@{
        Name = 'eco_jp'; Directory = 'C:\offline\eco_jp'; Executable = 'C:\offline\eco_jp\eco.exe'
        ServerList = if ($script:RemoteEndpoint) { 'ECO,203.0.113.10:17831' } else { 'ECO,127.0.0.1:17831,' }
    }
}
function Get-OfflineServerExecutable { 'C:\offline\eco-reborn.exe' }
function Set-OfflineClientResolution { param([string] $Directory) }
function Start-OfflineDisplayWatcher { $null }
function New-OfflineDiagnosticReport { $null }
function Write-OfflineDiagnosticStage {
    param($Report, [string] $Stage, [string] $Outcome = 'ok', [string] $Category = '')
    $script:DiagnosticStages += [pscustomobject]@{ Stage = $Stage; Outcome = $Outcome; Category = $Category }
}
function Initialize-OfflineDirectories { }
function Assert-OfflineFirewallIsolation { if ($script:IsolationFails) { throw 'mock isolation unavailable' } }
function Test-OfflinePortAvailable {
    param([int] $Port)
    $script:PortChecks += $Port
    $Port -ne $script:BlockedPort
}
function Wait-OfflineServer {
    param($Process, [int] $Port, [string] $Name)
    if (($Name -eq 'map' -and $script:MapNotReady) -or ($Name -eq 'login' -and $script:LoginNotReady) -or ($Name -eq 'world/gate' -and $script:WorldNotReady)) {
        throw "mock $Name readiness failure"
    }
    $script:ReadyPorts += $Port
}
function Wait-OfflineGame {
    param($Game, [object[]] $Servers)
    if ($script:ServiceStopsDuringGame) { throw 'mock server stopped while client was running' }
    $Game.HasExited = $true
}
function New-MockProcess {
    param([int] $Id)
    $process = [pscustomobject]@{ Id = $Id; HasExited = $false; ExitCode = 0; KillCount = 0; DisposeCount = 0 }
    $process | Add-Member ScriptMethod Kill {
        $this.KillCount++
        if ($script:FailKill) { throw 'mock kill failed' }
        $this.HasExited = $true
    }
    $process | Add-Member ScriptMethod WaitForExit { $this.HasExited }
    $process | Add-Member ScriptMethod Dispose { $this.DisposeCount++ }
    $process
}
function Start-OfflineChild {
    param([string] $Path, [string[]] $Arguments, [string] $WorkingDirectory, [string] $LogName, [switch] $Visible, [switch] $CaptureOutput, [string] $SessionId)
    $script:StartCount++
    $script:LaunchSpecs += [pscustomobject]@{
        Path = $Path; Arguments = @($Arguments); WorkingDirectory = $WorkingDirectory
        LogName = $LogName; Visible = $Visible.IsPresent; CaptureOutput = $CaptureOutput.IsPresent
    }
    if ($LogName -in @('map', 'login', 'world')) {
        $process = New-MockProcess -Id (100 + $script:StartCount)
        $script:ServerProcesses[$LogName] = $process
        return $process
    }
    if ($script:FailGameStart) { throw 'mock client start failed' }
    $script:ClientWasVisible = $Visible.IsPresent
    $process = New-MockProcess -Id 200
    $process.ExitCode = $script:ClientExitCode
    $script:LastClientProcess = $process
    $process
}

$script:RemoteEndpoint = $true
$rejected = $false
$script:LauncherMarkers = @()
try { $null = Invoke-OfflineStart -ClientName 'eco_jp' } catch { $rejected = $_.Exception.Message -match 'server\.lst' }
if (-not $rejected -or $script:StartCount -ne 0 -or $script:LauncherMarkers.Count -ne 0) {
    throw 'Remote server.lst must be rejected before starting any process or emitting the client-start marker.'
}

$script:RemoteEndpoint = $false
$script:BlockedPort = 17833
$portRejected = $false
try { Invoke-OfflineStart -ClientName 'eco_jp' } catch { $portRejected = $_.Exception.Message -match '17833.*in use' }
if (-not $portRejected -or $script:StartCount -ne 0 -or ($script:PortChecks -join ',') -ne '17833') {
    throw 'A map-port collision must prevent all process launches.'
}

$script:BlockedPort = 17832
$script:PortChecks = @()
$portRejected = $false
try { Invoke-OfflineStart -ClientName 'eco_jp' } catch { $portRejected = $_.Exception.Message -match '17832.*in use' }
if (-not $portRejected -or $script:StartCount -ne 0 -or ($script:PortChecks -join ',') -ne '17833,17832') {
    throw 'A login-port collision must prevent all process launches.'
}

$script:BlockedPort = 17831
$script:PortChecks = @()
$portRejected = $false
try { Invoke-OfflineStart -ClientName 'eco_jp' } catch { $portRejected = $_.Exception.Message -match '17831.*in use' }
if (-not $portRejected -or $script:StartCount -ne 0 -or ($script:PortChecks -join ',') -ne '17833,17832,17831') {
    throw 'A world/gate-port collision must prevent all process launches.'
}

$script:BlockedPort = 0
$script:PortChecks = @()
$script:IsolationFails = $true
$isolationRejected = $false
try { Invoke-OfflineStart -ClientName 'eco_jp' } catch { $isolationRejected = $_.Exception.Message -match 'mock isolation unavailable' }
if (-not $isolationRejected -or $script:StartCount -ne 0 -or ($script:PortChecks -join ',') -ne '17833,17832,17831') {
    throw 'Missing isolation must prevent all process launches after checking all three ports.'
}

$script:IsolationFails = $false
$script:WorldNotReady = $true
$worldReadinessRejected = $false
$script:LauncherMarkers = @()
try { $null = Invoke-OfflineStart -ClientName 'eco_jp' } catch { $worldReadinessRejected = $_.Exception.Message -match 'world/gate readiness failure' }
if (-not $worldReadinessRejected -or $script:ServerProcesses.map.KillCount -ne 1 -or
    $script:ServerProcesses.login.KillCount -ne 1 -or $script:ServerProcesses.world.KillCount -ne 1 -or
    $script:LauncherMarkers.Count -ne 0) {
    throw 'Failure while waiting for world/gate readiness must clean up all three started services without emitting the client-start marker.'
}

$script:WorldNotReady = $false
$script:FailGameStart = $true
$script:FailKill = $true
$cleanupFailureReported = $false
$script:LauncherMarkers = @()
try { $null = Invoke-OfflineStart -ClientName 'eco_jp' } catch { $cleanupFailureReported = $_.Exception.Message -match 'Cleanup also failed' }
if (-not $cleanupFailureReported -or $script:StartCount -ne 7 -or
    $script:ServerProcesses.map.KillCount -ne 1 -or $script:ServerProcesses.login.KillCount -ne 1 -or $script:ServerProcesses.world.KillCount -ne 1 -or
    $script:LauncherMarkers.Count -ne 0) {
    throw 'Client startup failure must clean up all exact server child processes without emitting the client-start marker.'
}

$script:FailGameStart = $false
$script:FailKill = $false
$script:ReadyPorts = @()
$script:LauncherMarkers = @()
$successfulOutput = @(Invoke-OfflineStart -ClientName 'eco_jp')
if ($script:LauncherMarkers.Count -ne 1 -or @($successfulOutput | Where-Object { $_ -ceq 'ECO_LAUNCHER_CLIENT_STARTED' }).Count -ne 1) {
    throw 'A successful client process start must emit the static marker exactly once.'
}
$launches = @($script:LaunchSpecs | Select-Object -Last 4)
if (-not $script:ClientWasVisible -or ($script:ReadyPorts -join ',') -ne '17833,17832,17831') {
    throw 'All three services must become ready before the visible client starts.'
}
if ($launches[0].LogName -ne 'map' -or $launches[0].Arguments[0] -ne 'serve-map' -or $launches[0].Arguments[1] -ne '17833' -or
    $launches[1].LogName -ne 'login' -or $launches[1].Arguments[0] -ne 'serve-login' -or $launches[1].Arguments[1] -ne '17832' -or
    $launches[2].LogName -ne 'world' -or $launches[2].Arguments[0] -ne 'serve-world' -or $launches[2].Arguments[1] -ne '17831') {
    throw 'The wrapper must start map:17833, login:17832, then world/gate:17831.'
}
if (@($launches | Select-Object -First 3 | ForEach-Object { $_.Arguments[2] } | Sort-Object -Unique).Count -ne 1 -or
    @($launches | Select-Object -First 3 | Where-Object { -not $_.CaptureOutput }).Count -ne 0) {
    throw 'All three services must share the same save path and capture distinct session-scoped server streams.'
}

$expectedTerrain = [IO.Path]::GetFullPath((Join-Path (Split-Path $scriptPath -Parent) 'data\20013003.map'))
$expectedUpTown = [IO.Path]::GetFullPath((Join-Path (Split-Path $scriptPath -Parent) 'data\10023000.map'))
if ($launches[0].Arguments[3] -ne ('"' + $expectedTerrain + '"') -or $launches[0].Arguments[4] -ne ('"' + $expectedUpTown + '"')) {
    throw 'Map service must receive the absolute source terrain paths.'
}
$clientExitStage = @($script:DiagnosticStages | Where-Object { $_.Stage -eq 'client_exit' })[-1]
if (-not $clientExitStage -or $clientExitStage.Outcome -ne 'ok' -or $script:LastClientProcess.DisposeCount -ne 1) {
    throw 'A zero client exit must be recorded as successful and the exited client disposed.'
}

$script:DiagnosticStages = @()
$script:ClientExitCode = -1073741819
$nonzeroExit = $false
$script:LauncherMarkers = @()
try { $null = Invoke-OfflineStart -ClientName 'eco_jp' }
catch { $nonzeroExit = $_.Exception.Message -ceq 'The ECO client exited with a nonzero status.' }
$clientExitStage = @($script:DiagnosticStages | Where-Object { $_.Stage -eq 'client_exit' })[-1]
if (-not $nonzeroExit -or -not $clientExitStage -or $clientExitStage.Outcome -ne 'failed' -or
    $clientExitStage.Category -ne 'other_failure' -or $script:DiagnosticStages.Where({ $_.Stage -eq 'client_exit' }).Count -ne 1 -or
    $script:LauncherMarkers.Count -ne 1 -or
    $script:DiagnosticStages[-1].Stage -ne 'cleanup' -or $script:DiagnosticStages[-1].Outcome -ne 'ok' -or
    $script:LastClientProcess.DisposeCount -ne 1 -or $script:ServerProcesses.map.KillCount -ne 1 -or
    $script:ServerProcesses.login.KillCount -ne 1 -or $script:ServerProcesses.world.KillCount -ne 1) {
    throw 'A nonzero client exit must produce one static failed report stage and clean up every owned process.'
}

$script:DiagnosticStages = @()
$script:ClientExitCode = 0
$script:ServiceStopsDuringGame = $true
$serviceStopFailed = $false
$script:LauncherMarkers = @()
try { $null = Invoke-OfflineStart -ClientName 'eco_jp' }
catch { $serviceStopFailed = $_.Exception.Message -ceq 'mock server stopped while client was running' }
$clientExitStage = @($script:DiagnosticStages | Where-Object { $_.Stage -eq 'client_exit' })[-1]
if (-not $serviceStopFailed -or -not $clientExitStage -or $clientExitStage.Outcome -ne 'failed' -or
    $clientExitStage.Category -ne 'server_stopped' -or $script:DiagnosticStages.Where({ $_.Stage -eq 'client_exit' }).Count -ne 1 -or
    $script:LauncherMarkers.Count -ne 1 -or
    $script:DiagnosticStages[-1].Stage -ne 'cleanup' -or $script:DiagnosticStages[-1].Outcome -ne 'ok' -or
    $script:LastClientProcess.DisposeCount -ne 1 -or $script:LastClientProcess.KillCount -ne 1 -or
    $script:ServerProcesses.map.KillCount -ne 1 -or $script:ServerProcesses.login.KillCount -ne 1 -or
    $script:ServerProcesses.world.KillCount -ne 1) {
    throw 'A service stopping during the client run must retain server_stopped and clean up every owned process.'
}
'Offline launcher self-check passed (loopback guard, three port gates, role startup/readiness, distinct logs, shared save, terrain path, client exit classification, and process cleanup).'
