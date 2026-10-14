function New-OfflineDiagnosticReport {
    $directory = Join-Path $PSScriptRoot 'reports'
    $path = $null
    try {
        [void](New-Item -ItemType Directory -Force -Path $directory)
        $session = [Guid]::NewGuid().ToString('N')
        $path = Join-Path $directory "session-$session.txt"
        $content = @(
            'Eco Reborn session report'
            'Only approved static events and elapsed seconds are recorded.'
            'Names, credentials, tokens, addresses, machine identifiers, paths, and raw logs are excluded.'
            'Protocol event timings are not recorded.'
            'Service event streams include wrapper startup health-probe connections.'
            ''
        ) -join [Environment]::NewLine
        $stream = [IO.File]::Open($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
        try {
            $bytes = [Text.UTF8Encoding]::new($false).GetBytes($content + [Environment]::NewLine)
            $stream.Write($bytes, 0, $bytes.Length)
        } finally { $stream.Dispose() }
        [pscustomobject]@{
            Path = $path; Clock = [Diagnostics.Stopwatch]::StartNew(); SessionId = $session
            WriteFailed = $false; ImportedEventCount = 0; EventsTruncated = $false; TruncationWritten = $false; ImportedStreams = @()
        }
    } catch {
        if ($path -and (Test-Path -LiteralPath $path)) { Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue }
        Write-Warning 'The privacy-safe session report could not be created; no report will be available.'
        $null
    }
}

function Write-OfflineDiagnosticWarning {
    param($Report)
    if ($null -eq $Report -or $Report.WriteFailed) { return }
    $Report.WriteFailed = $true
    Write-Warning 'The privacy-safe session report could not be updated; some diagnostic events may be missing.'
}

function Write-OfflineDiagnosticStage {
    param($Report, [string] $Stage, [string] $Outcome = 'ok', [string] $Category = '')
    $allowedStages = @('wrapper_start', 'preflight', 'isolation', 'map_start', 'map_ready', 'login_start', 'login_ready', 'world_start', 'world_ready', 'client_start', 'client_exit', 'cleanup')
    $allowedOutcomes = @('ok', 'failed', 'refused', 'unavailable', 'stopped')
    $allowedCategories = @('preflight_refused', 'isolation_unavailable', 'server_start_failed', 'server_not_ready', 'client_start_failed', 'server_stopped', 'cleanup_failed', 'other_failure')
    if ($null -eq $Report -or $Stage -notin $allowedStages -or $Outcome -notin $allowedOutcomes) { return }
    if ($Category -and ($Outcome -ne 'failed' -or $Category -notin $allowedCategories)) { return }
    try {
        $elapsed = [Math]::Max(0, [int]$Report.Clock.Elapsed.TotalSeconds)
        $categoryText = if ($Category) { " category=$Category" } else { '' }
        [IO.File]::AppendAllText($Report.Path, ('+{0:D6}s stage={1} outcome={2}{3}{4}' -f $elapsed, $Stage, $Outcome, $categoryText, [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
    } catch { Write-OfflineDiagnosticWarning -Report $Report }
}

function ConvertFrom-OfflineRustEvent {
    param([string] $Line)
    $plainEvents = @('LISTENING', 'CLIENT_CONNECTED', 'HANDSHAKE_COMPLETE', 'LOGIN_ACCEPTED', 'LOGIN_REJECTED', 'CHARACTER_LIST_SENT', 'CHARACTER_CREATED', 'CHARACTER_CREATE_REJECTED', 'MAP_ID_SENT', 'LOGIN_STATUS_SENT', 'MAP_UNAVAILABLE', 'SERVER_LIST_SENT', 'MAP_ENTERED', 'MAP_LOADED', 'PLAYER_MOVED', 'MAP_REDIRECT_SENT', 'CONNECTION_CLOSED', 'PANIC', 'BATCHED_FRAME_ACCEPTED')
    $errors = @('IO', 'INVALID_DATA', 'PERMISSION_DENIED', 'NOT_FOUND', 'ADDRESS_IN_USE', 'TIMEOUT', 'CONNECTION_RESET', 'BROKEN_PIPE', 'ALREADY_EXISTS')
    $match = [regex]::Match($Line, '^ECO_EVENT event=(?<event>[A-Z_]+)$')
    if ($match.Success -and $match.Groups['event'].Value -in $plainEvents) { return 'protocol event=' + $match.Groups['event'].Value }
    $match = [regex]::Match($Line, '^ECO_EVENT event=(?<event>CONNECTION_ERROR|STARTUP_FAILED|STORAGE_ERROR|ACCEPT_ERROR) kind=(?<kind>[A-Z_]+)$')
    if ($match.Success -and $match.Groups['kind'].Value -in $errors) {
        return 'protocol event=' + $match.Groups['event'].Value + ' kind=' + $match.Groups['kind'].Value
    }
    $match = [regex]::Match($Line, '^ECO_EVENT event=UNKNOWN_OPCODE opcode=(?<opcode>0x[0-9a-f]{4})$')
    if ($match.Success) { return 'protocol event=UNKNOWN_OPCODE opcode=' + $match.Groups['opcode'].Value }
    $match = [regex]::Match($Line, '^ECO_EVENT event=PROTOCOL_FAILURE stage=(?<stage>FRAME_HEADER|RECEIVE|FRAME_BODY|FRAME_PADDING|INNER_PACKET|FRIEND_INIT_PAYLOAD|FRIEND_INIT_OWNERSHIP|DISPATCH)(?: opcode=(?<opcode>0x[0-9a-f]{4}))?$')
    if ($match.Success) {
        if ($match.Groups['stage'].Value -eq 'DISPATCH' -and $match.Groups['opcode'].Success) {
            return 'protocol event=PROTOCOL_FAILURE stage=DISPATCH opcode=' + $match.Groups['opcode'].Value
        }
        if ($match.Groups['stage'].Value -ne 'DISPATCH' -and -not $match.Groups['opcode'].Success) {
            return 'protocol event=PROTOCOL_FAILURE stage=' + $match.Groups['stage'].Value
        }
    }
    $match = [regex]::Match($Line, '^ECO_EVENT event=FRIEND_INIT_ID_SHAPE zero=(?<zero>true|false) matches_account=(?<account>true|false) matches_owned_slot_u32=(?<slot_u32>true|false) first_byte_owned_slot=(?<first_byte>true|false) zero_tail=(?<zero_tail>true|false)$')
    if ($match.Success) { return 'protocol event=FRIEND_INIT_ID_SHAPE zero=' + $match.Groups['zero'].Value + ' matches_account=' + $match.Groups['account'].Value + ' matches_owned_slot_u32=' + $match.Groups['slot_u32'].Value + ' first_byte_owned_slot=' + $match.Groups['first_byte'].Value + ' zero_tail=' + $match.Groups['zero_tail'].Value }
    $null
}

function Add-OfflineRustEvents {
    param($Report, [ValidateSet('map', 'login', 'world')][string] $Service, [string[]] $LogPaths)
    try {
        if ($null -eq $Report) { return }
        foreach ($logPath in $LogPaths) {
            if (-not (Test-Path -LiteralPath $logPath -PathType Leaf -ErrorAction Stop)) { continue }
            foreach ($line in [IO.File]::ReadLines($logPath)) {
                $event = ConvertFrom-OfflineRustEvent -Line $line
                if ($null -ne $event) {
                    if ($Service -notin $Report.ImportedStreams) {
                        [IO.File]::AppendAllText($Report.Path, ("protocol stream=$Service" + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
                        $Report.ImportedStreams += $Service
                    }
                    if ($Report.ImportedEventCount -lt 2000) {
                        [IO.File]::AppendAllText($Report.Path, ($event + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
                        $Report.ImportedEventCount++
                    } else { $Report.EventsTruncated = $true }
                }
            }
        }
        if ($Report.EventsTruncated -and -not $Report.TruncationWritten) {
            [IO.File]::AppendAllText($Report.Path, ('protocol events truncated after 2000 approved events' + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
            $Report.TruncationWritten = $true
        }
    } catch { Write-OfflineDiagnosticWarning -Report $Report }
}
