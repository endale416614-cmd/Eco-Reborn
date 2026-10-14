<#
.SYNOPSIS
  Starts the Eco Reborn loopback servers and the eco_jp client.
.DESCRIPTION
  The wrapper never edits server.lst, applies firewall rules, or starts the vendor launcher.
  In borderless display mode it sets the game's resolution in eco.ini to the screen size.
  It requires the complete active firewall isolation rules and does not request elevation.
.PARAMETER Client
  eco_jp, the only client folder in this workspace.
#>
[CmdletBinding()]
param(
    [ValidateSet('eco_jp')]
    [string] $Client = 'eco_jp'
)

. (Join-Path $PSScriptRoot 'diagnostics.ps1')

function Get-OfflineClient {
    param([string] $Name)
    $workspace = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
    $directory = Join-Path $workspace $Name
    $exe = Join-Path $directory 'eco.exe'
    $serverList = Join-Path $directory 'server.lst'
    foreach ($path in @($exe, $serverList)) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required client file is missing: $path" }
    }
    [pscustomobject]@{
        Name = $Name
        Directory = (Resolve-Path -LiteralPath $directory).Path
        Executable = (Resolve-Path -LiteralPath $exe).Path
        ServerList = [IO.File]::ReadAllText($serverList)
    }
}

function Test-OfflineServerListText {
    param([string] $Text, [int] $Port)
    $rows = @($Text -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if ($rows.Count -ne 1) { return $false }
    $fields = $rows[0].Trim().Split(',')
    if ($fields.Count -lt 2 -or [string]::IsNullOrWhiteSpace($fields[0])) { return $false }
    if ($fields[1].Trim() -cne "127.0.0.1:$Port") { return $false }
    return (@($fields | Select-Object -Skip 2 | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }).Count -eq 0)
}

function Assert-OfflineFirewallIsolation {
    . (Join-Path $PSScriptRoot 'Set-RebornIsolation.ps1')
    try {
        $profiles = @(Get-NetFirewallProfile -PolicyStore ActiveStore -ErrorAction Stop)
        if (-not (Test-OfflineProfilesEnabled -Profiles $profiles)) { throw 'one or more active firewall profiles are disabled or unavailable' }
        $paths = @(Get-OfflineAppPaths)
        $plan = @(Get-OfflineFirewallPlan -Paths $paths)
        $rules = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction Stop)
        $owned = @($rules | Where-Object { $_.Group -eq $script:RuleGroup })
        if (-not (Test-OfflineRuleSet -Rules $owned -Plan $plan)) { throw 'the complete matching application rule set is not active' }
    } catch {
        throw "Cannot verify offline network isolation. Run 'Repair Isolation.cmd' in the Eco Reborn folder (approve the UAC prompt), then retry. Details: $($_.Exception.Message)"
    }
}

function Test-OfflinePortAvailable {
    param([int] $Port)
    Test-OfflineListenerTable -Listeners ([Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners()) -Port $Port
}

function Test-OfflineListenerTable {
    param([object[]] $Listeners, [int] $Port)
    @($Listeners | Where-Object { $_.Port -eq $Port }).Count -eq 0
}

function Get-OfflineServerExecutable {
    $path = Join-Path $PSScriptRoot 'target\release\eco-reborn.exe'
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Release server not found: $path (build offline first)." }
    (Resolve-Path -LiteralPath $path).Path
}

function Initialize-OfflineDirectories {
    param([string] $LogDirectory, [string] $SaveDirectory)
    [void](New-Item -ItemType Directory -Force -Path $LogDirectory, $SaveDirectory)
}

function Test-OfflineServerReady {
    param([int] $Port)
    $client = [Net.Sockets.TcpClient]::new()
    try {
        $client.Connect([Net.IPAddress]::Loopback, $Port)
        $stream = $client.GetStream()
        $stream.ReadTimeout = 1000
        $stream.WriteTimeout = 1000
        $stream.Write([byte[]]@(0, 0, 0, 0, 0, 0, 0, 16), 0, 8)
        $response = New-Object byte[] 529
        $read = 0
        while ($read -lt $response.Length) {
            $count = $stream.Read($response, $read, $response.Length - $read)
            if ($count -eq 0) { return $false }
            $read += $count
        }
        return [BitConverter]::ToString($response, 0, 4) -eq '00-00-00-00' -and
            [BitConverter]::ToString($response, 4, 4) -eq '00-00-00-01'
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

function Start-OfflineChild {
    param([string] $Path, [string[]] $Arguments, [string] $WorkingDirectory, [ValidateSet('map', 'login', 'world', 'client')][string] $LogName, [switch] $Visible, [switch] $CaptureOutput, [string] $SessionId)
    $windowStyle = if ($Visible) { 'Normal' } else { 'Hidden' }
    $startArgs = @{
        FilePath = $Path; ArgumentList = $Arguments; WorkingDirectory = $WorkingDirectory
        WindowStyle = $windowStyle; PassThru = $true; ErrorAction = 'Stop'
    }
    if ($CaptureOutput) {
        $paths = Get-OfflineServerLogPaths -ServiceName $LogName -SessionId $SessionId
        $startArgs.RedirectStandardOutput = $paths.Stdout
        $startArgs.RedirectStandardError = $paths.Stderr
    }
    Start-Process @startArgs
}

function Get-OfflineServerLogPaths {
    param([ValidateSet('map', 'login', 'world')][string] $ServiceName, [ValidatePattern('^[a-f0-9]{32}$')][string] $SessionId)
    $logDir = Join-Path $PSScriptRoot 'logs'
    [pscustomobject]@{
        Stdout = Join-Path $logDir "$ServiceName-$SessionId.out.log"
        Stderr = Join-Path $logDir "$ServiceName-$SessionId.err.log"
    }
}

function Stop-OfflineChild {
    param($Process, [string] $Name)
    if ($null -eq $Process) { return }
    if (-not $Process.HasExited) {
        $Process.Kill()
        if (-not $Process.WaitForExit(5000)) { throw "Timed out stopping the $Name process (PID $($Process.Id))." }
    }
    $Process.Dispose()
}

function Get-OfflineDisplayMode {
    # launcher-settings.txt: "display = borderless" (default) or "display = windowed".
    $path = Join-Path $PSScriptRoot 'launcher-settings.txt'
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        foreach ($line in [IO.File]::ReadAllLines($path)) {
            if ($line -match '^\s*display\s*=\s*(\w+)') { return $Matches[1].ToLowerInvariant() }
        }
    }
    'borderless'
}

function Set-OfflineClientResolution {
    param([string] $Directory)
    # Borderless needs the game's windowed size to match the screen (1920x1080 here); an unset
    # full-screen size gets the same default.
    if ((Get-OfflineDisplayMode) -ne 'borderless') { return }
    $ini = Join-Path $Directory 'eco.ini'
    if (-not (Test-Path -LiteralPath $ini -PathType Leaf)) { return }
    Add-Type -AssemblyName System.Windows.Forms
    $screen = [Windows.Forms.Screen]::PrimaryScreen.Bounds
    $text = [IO.File]::ReadAllText($ini, [Text.Encoding]::Unicode)
    $updated = $text -replace '(?m)^WinX=\d*', "WinX=$($screen.Width)" `
        -replace '(?m)^WinY=\d*', "WinY=$($screen.Height)" `
        -replace '(?m)^FullX=0?(?=\r?$)', "FullX=$($screen.Width)" `
        -replace '(?m)^FullY=0?(?=\r?$)', "FullY=$($screen.Height)"
    if ($updated -cne $text) { [IO.File]::WriteAllText($ini, $updated, [Text.Encoding]::Unicode) }
}

function Start-OfflineDisplayWatcher {
    # Keeps the game borderless for the whole session, including after Setup mode switches.
    if ((Get-OfflineDisplayMode) -ne 'borderless') { return $null }
    $tool = Join-Path $PSScriptRoot 'target\eco-window.exe'
    if (-not (Test-Path -LiteralPath $tool -PathType Leaf)) { return $null }
    Start-Process -FilePath $tool -ArgumentList 'watch' -WindowStyle Hidden -PassThru
}

function Wait-OfflineMapIdle {
    param([int] $Port, [int] $TimeoutSeconds = 3)
    # The map service writes the batched position when the client's connection closes and
    # only then closes its side, so wait for that before stopping the process.
    $limit = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $limit) {
        $open = @(Get-NetTCPConnection -LocalPort $Port -State Established -ErrorAction SilentlyContinue)
        if ($open.Count -eq 0) { return }
        Start-Sleep -Milliseconds 100
    }
}

function Wait-OfflineServer {
    param($Process, [int] $Port, [string] $Name, [int] $TimeoutSeconds = 15)
    $limit = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $limit) {
        if ($Process.HasExited) { throw "The local $Name service exited during startup (code $($Process.ExitCode))." }
        if (Test-OfflineServerReady -Port $Port) { return }
        Start-Sleep -Milliseconds 150
    }
    throw "Timed out waiting for the local $Name service handshake on 127.0.0.1:$Port."
}

function Wait-OfflineGame {
    param($Game, [object[]] $Servers)
    while (-not $Game.HasExited) {
        foreach ($server in $Servers) {
            if ($server.HasExited) { throw "A local server process stopped while the client was running (code $($server.ExitCode))." }
        }
        Start-Sleep -Milliseconds 250
    }
}

function Invoke-OfflineStart {
    param([string] $ClientName)
    $worldPort = 17831
    $loginPort = 17832
    $mapPort = 17833
    $report = New-OfflineDiagnosticReport
    $stage = 'preflight'
    Write-OfflineDiagnosticStage -Report $report -Stage 'wrapper_start'
    try {
        $client = Get-OfflineClient -Name $ClientName
        if (-not (Test-OfflineServerListText -Text $client.ServerList -Port $worldPort)) {
            throw "Refusing to launch: $ClientName server.lst must contain exactly one endpoint at 127.0.0.1:$worldPort. No client files were changed."
        }
        $serverExe = Get-OfflineServerExecutable
        foreach ($port in @($mapPort, $loginPort, $worldPort)) {
            if (-not (Test-OfflinePortAvailable -Port $port)) { throw "Port $port is already in use; close that program and retry." }
        }
    } catch {
        Write-OfflineDiagnosticStage -Report $report -Stage 'preflight' -Outcome 'failed' -Category 'preflight_refused'
        throw
    }
    Write-OfflineDiagnosticStage -Report $report -Stage 'preflight'
    $stage = 'isolation'
    try { Assert-OfflineFirewallIsolation }
    catch {
        Write-OfflineDiagnosticStage -Report $report -Stage 'isolation' -Outcome 'failed' -Category 'isolation_unavailable'
        $script:IsolationMissing = $true
        throw
    }
    Write-OfflineDiagnosticStage -Report $report -Stage 'isolation'

    $logs = Join-Path $PSScriptRoot 'logs'
    $saves = Join-Path $PSScriptRoot 'saves'
    try { Initialize-OfflineDirectories -LogDirectory $logs -SaveDirectory $saves }
    catch {
        Write-OfflineDiagnosticStage -Report $report -Stage 'preflight' -Outcome 'failed' -Category 'other_failure'
        throw
    }
    $store = Join-Path $saves 'world.sqlite'
    $terrain = Join-Path $PSScriptRoot 'data\20013003.map'
    $upTown = Join-Path $PSScriptRoot 'data\10023000.map'
    $mapServer = $null
    $loginServer = $null
    $worldServer = $null
    $game = $null
    $displayWatcher = $null
    $failure = $null
    $clientExitFailed = $false
    $cleanupFailures = @()
    $sessionLogId = if ($report) { $report.SessionId } else { [Guid]::NewGuid().ToString('N') }
    try {
        $stage = 'map_start'
        $mapServer = Start-OfflineChild -Path $serverExe -Arguments @('serve-map', "$mapPort", "`"$store`"", "`"$terrain`"", "`"$upTown`"") `
            -WorkingDirectory $PSScriptRoot -LogName 'map' -CaptureOutput -SessionId $sessionLogId
        if ($null -eq $mapServer) { throw 'Could not start the local map service.' }
        Write-OfflineDiagnosticStage -Report $report -Stage 'map_start'
        $stage = 'map_ready'
        Wait-OfflineServer -Process $mapServer -Port $mapPort -Name 'map'
        Write-OfflineDiagnosticStage -Report $report -Stage 'map_ready'

        $stage = 'login_start'
        $loginServer = Start-OfflineChild -Path $serverExe -Arguments @('serve-login', "$loginPort", "`"$store`"") `
            -WorkingDirectory $PSScriptRoot -LogName 'login' -CaptureOutput -SessionId $sessionLogId
        if ($null -eq $loginServer) { throw 'Could not start the local login service.' }
        Write-OfflineDiagnosticStage -Report $report -Stage 'login_start'
        $stage = 'login_ready'
        Wait-OfflineServer -Process $loginServer -Port $loginPort -Name 'login'
        Write-OfflineDiagnosticStage -Report $report -Stage 'login_ready'

        $stage = 'world_start'
        $worldServer = Start-OfflineChild -Path $serverExe -Arguments @('serve-world', "$worldPort", "`"$store`"") `
            -WorkingDirectory $PSScriptRoot -LogName 'world' -CaptureOutput -SessionId $sessionLogId
        if ($null -eq $worldServer) { throw 'Could not start the local world/gate service.' }
        Write-OfflineDiagnosticStage -Report $report -Stage 'world_start'
        $stage = 'world_ready'
        Wait-OfflineServer -Process $worldServer -Port $worldPort -Name 'world/gate'
        Write-OfflineDiagnosticStage -Report $report -Stage 'world_ready'

        $stage = 'client_start'
        Set-OfflineClientResolution -Directory $client.Directory
        $game = Start-OfflineChild -Path $client.Executable `
            -Arguments @('/launch', '-u:test', '-p:96e79218965eb72c92a549dd5a330112') `
            -WorkingDirectory $client.Directory -LogName 'client' -Visible
        if ($null -eq $game) { throw 'Could not start eco.exe.' }
        Write-OfflineDiagnosticStage -Report $report -Stage 'client_start'
        Write-Output 'ECO_LAUNCHER_CLIENT_STARTED'
        $displayWatcher = Start-OfflineDisplayWatcher
        $stage = 'client_exit'
        Wait-OfflineGame -Game $game -Servers @($mapServer, $loginServer, $worldServer)
        if ($game.ExitCode -ne 0) {
            $clientExitFailed = $true
            throw 'The ECO client exited with a nonzero status.'
        }
        Write-OfflineDiagnosticStage -Report $report -Stage 'client_exit'
    } catch {
        $failure = $_.Exception.Message
        $category = switch ($stage) {
            { $_ -in @('map_start', 'login_start', 'world_start') } { 'server_start_failed' }
            { $_ -in @('map_ready', 'login_ready', 'world_ready') } { 'server_not_ready' }
            'client_start' { 'client_start_failed' }
            'client_exit' { if ($clientExitFailed) { 'other_failure' } else { 'server_stopped' } }
            default { 'other_failure' }
        }
        Write-OfflineDiagnosticStage -Report $report -Stage $stage -Outcome 'failed' -Category $category
    } finally {
        foreach ($child in @(
            @{ Process = $game; Name = 'client' },
            @{ Process = $displayWatcher; Name = 'display watcher' },
            @{ Process = $worldServer; Name = 'world service' },
            @{ Process = $loginServer; Name = 'login service' },
            @{ Process = $mapServer; Name = 'map service' }
        )) {
            if ($child.Name -eq 'map service' -and $null -ne $mapServer -and -not $mapServer.HasExited) {
                Wait-OfflineMapIdle -Port $mapPort
            }
            try { Stop-OfflineChild -Process $child.Process -Name $child.Name }
            catch { $cleanupFailures += $_.Exception.Message }
        }
        if ($cleanupFailures.Count) {
            Write-OfflineDiagnosticStage -Report $report -Stage 'cleanup' -Outcome 'failed' -Category 'cleanup_failed'
        } else { Write-OfflineDiagnosticStage -Report $report -Stage 'cleanup' -Outcome 'ok' }
        if ($report) {
            Add-OfflineRustEvents -Report $report -Service 'map' -LogPaths @(
                (Join-Path $PSScriptRoot "logs\map-$sessionLogId.out.log"),
                (Join-Path $PSScriptRoot "logs\map-$sessionLogId.err.log")
            )
            Add-OfflineRustEvents -Report $report -Service 'login' -LogPaths @(
                (Join-Path $PSScriptRoot "logs\login-$sessionLogId.out.log"),
                (Join-Path $PSScriptRoot "logs\login-$sessionLogId.err.log")
            )
            Add-OfflineRustEvents -Report $report -Service 'world' -LogPaths @(
                (Join-Path $PSScriptRoot "logs\world-$sessionLogId.out.log"),
                (Join-Path $PSScriptRoot "logs\world-$sessionLogId.err.log")
            )
        }
    }
    if ($failure -and $cleanupFailures.Count) { throw "$failure Cleanup also failed: $($cleanupFailures -join '; ')" }
    if ($failure) { throw $failure }
    if ($cleanupFailures.Count) { throw "Cleanup failed: $($cleanupFailures -join '; ')" }
    'Login/character prototype stopped. Full world gameplay is not implemented.'
}

if ($MyInvocation.InvocationName -ne '.') {
    try { Invoke-OfflineStart -ClientName $Client }
    catch {
        [Console]::Error.WriteLine('Eco Reborn startup failed. Check the privacy-safe report in offline/reports; verify the client files, free local ports, and offline isolation.')
        # Exit code 3 lets the desktop launcher tell the player to run Repair Isolation.cmd.
        if ($script:IsolationMissing) { exit 3 }
        exit 1
    }
}
