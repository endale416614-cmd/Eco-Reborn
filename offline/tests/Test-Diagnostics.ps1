$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\diagnostics.ps1')

$report = New-OfflineDiagnosticReport
if ($null -eq $report) { throw 'Could not create a test report.' }
$tempLog = [IO.Path]::GetTempFileName()
$mapLog = [IO.Path]::GetTempFileName()
try {
    Write-OfflineDiagnosticStage -Report $report -Stage 'wrapper_start'
    Write-OfflineDiagnosticStage -Report $report -Stage 'client_start' -Outcome 'failed' -Category 'client_start_failed'
    Write-OfflineDiagnosticStage -Report $report -Stage 'client_exit' -Outcome 'failed' -Category 'other_failure'
    $seeded = @(
        'ECO_EVENT event=HANDSHAKE_COMPLETE'
        'ECO_EVENT event=PANIC'
        'ECO_EVENT event=SERVER_LIST_SENT'
        'ECO_EVENT event=UNKNOWN_OPCODE opcode=0x0032'
        'ECO_EVENT event=CONNECTION_ERROR kind=TIMEOUT'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=FRAME_HEADER'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=RECEIVE'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=FRAME_BODY'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=FRAME_PADDING'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=INNER_PACKET'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=FRIEND_INIT_PAYLOAD'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=FRIEND_INIT_OWNERSHIP'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=DISPATCH opcode=0x0032'
        'ECO_EVENT event=BATCHED_FRAME_ACCEPTED'
        'ECO_EVENT event=FRIEND_INIT_ID_SHAPE zero=false matches_account=true matches_owned_slot_u32=false first_byte_owned_slot=true zero_tail=false'
        'ECO_EVENT event=FRIEND_INIT_ID_SHAPE zero=false matches_account=true matches_owned_slot_u32=false first_byte_owned_slot=true zero_tail=false character_id=734523'
        'ECO_EVENT event=FRIEND_INIT_ID_SHAPE zero=1 matches_account=true matches_owned_slot_u32=false first_byte_owned_slot=true zero_tail=false'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=DISPATCH opcode=0x0032 username=Alice password=SECRET_TOKEN'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=RECEIVE opcode=0x0032'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=UNKNOWN'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=DISPATCH opcode=0xZZZZ'
        'ECO_EVENT event=PROTOCOL_FAILURE stage=DISPATCH'
        'ECO_EVENT event=LOGIN_ACCEPTED username=Alice password=SECRET_TOKEN'
        'ECO_EVENT event=UNKNOWN_OPCODE opcode=0x0032 password=SECRET_TOKEN'
        'ECO_EVENT event=SECRET_PHASE opcode=0x0032'
        'alice@example.invalid Alice SECRET_TOKEN HASH_VALUE 192.0.2.5 fe80::1234'
        'AA:BB:CC:DD:EE:FF C:\\Users\\Alice\\Game DOMAIN\\Alice SID-1234567890'
        'C:\\Program Files\\ECO\\eco.exe --token=SECRET_TOKEN stacktrace at private.method()'
        'machine-id=0123456789abcdef MAC=AA-BB-CC-DD-EE-FF'
    )
    [IO.File]::WriteAllLines($tempLog, $seeded, [Text.UTF8Encoding]::new($false))
    Add-OfflineRustEvents -Report $report -Service 'login' -LogPaths @($tempLog)
    [IO.File]::WriteAllLines($mapLog, @('ECO_EVENT event=MAP_ENTERED', 'ECO_EVENT event=MAP_LOADED', 'ECO_EVENT event=PLAYER_MOVED', 'ECO_EVENT event=MAP_REDIRECT_SENT'), [Text.UTF8Encoding]::new($false))
    Add-OfflineRustEvents -Report $report -Service 'map' -LogPaths @($mapLog)

    $text = [IO.File]::ReadAllText($report.Path)
    foreach ($marker in @('Alice', 'alice@example.invalid', 'SECRET_TOKEN', 'HASH_VALUE', '192.0.2.5', 'fe80::1234', 'AA:BB:CC:DD:EE:FF', 'Program Files', 'DOMAIN', 'SID-1234567890', 'private.method', 'machine-id')) {
        if ($text.Contains($marker)) { throw "Sensitive test marker was written to the report: $marker" }
    }
    foreach ($safe in @('stage=wrapper_start', 'stage=client_start outcome=failed category=client_start_failed', 'stage=client_exit outcome=failed category=other_failure', 'protocol stream=login', 'protocol stream=map', 'protocol event=HANDSHAKE_COMPLETE', 'protocol event=PANIC', 'protocol event=SERVER_LIST_SENT', 'protocol event=MAP_ENTERED', 'protocol event=MAP_LOADED', 'protocol event=PLAYER_MOVED', 'protocol event=MAP_REDIRECT_SENT', 'protocol event=UNKNOWN_OPCODE opcode=0x0032', 'protocol event=CONNECTION_ERROR kind=TIMEOUT', 'protocol event=PROTOCOL_FAILURE stage=FRAME_HEADER', 'protocol event=PROTOCOL_FAILURE stage=RECEIVE', 'protocol event=PROTOCOL_FAILURE stage=FRAME_BODY', 'protocol event=PROTOCOL_FAILURE stage=FRAME_PADDING', 'protocol event=PROTOCOL_FAILURE stage=INNER_PACKET', 'protocol event=PROTOCOL_FAILURE stage=FRIEND_INIT_PAYLOAD', 'protocol event=PROTOCOL_FAILURE stage=FRIEND_INIT_OWNERSHIP', 'protocol event=PROTOCOL_FAILURE stage=DISPATCH opcode=0x0032', 'protocol event=BATCHED_FRAME_ACCEPTED', 'protocol event=FRIEND_INIT_ID_SHAPE zero=false matches_account=true matches_owned_slot_u32=false first_byte_owned_slot=true zero_tail=false')) {
        if (-not $text.Contains($safe)) { throw "Expected approved report item is missing: $safe" }
    }
    if ($text.Contains('LOGIN_ACCEPTED username') -or $text.Contains('SECRET_PHASE') -or $text.Contains('password=SECRET_TOKEN') -or $text.Contains('stage=RECEIVE opcode=') -or $text.Contains('stage=UNKNOWN') -or $text.Contains('opcode=0xZZZZ') -or $text.Contains('stage=DISPATCH' + [Environment]::NewLine) -or $text.Contains('character_id=') -or $text.Contains('zero=1')) {
        throw 'A malformed event line or unknown field was not omitted.'
    }
    if ($text -match '\d{4}-\d{2}-\d{2}|[A-Z]:\\|\\\\[^\\]+\\') { throw 'Report contains a calendar date or filesystem location.' }

    $boundedReport = New-OfflineDiagnosticReport
    [IO.File]::WriteAllLines($tempLog, (1..2002 | ForEach-Object { 'ECO_EVENT event=HANDSHAKE_COMPLETE' }), [Text.UTF8Encoding]::new($false))
    Add-OfflineRustEvents -Report $boundedReport -Service 'login' -LogPaths @($tempLog)
    [IO.File]::WriteAllLines($tempLog, @('ECO_EVENT event=PANIC', 'ECO_EVENT event=PANIC'), [Text.UTF8Encoding]::new($false))
    Add-OfflineRustEvents -Report $boundedReport -Service 'world' -LogPaths @($tempLog)
    $bounded = [IO.File]::ReadAllText($boundedReport.Path)
    if (@($bounded -split "`r?`n" | Where-Object { $_ -eq 'protocol event=HANDSHAKE_COMPLETE' }).Count -ne 2000 -or
        -not $bounded.Contains('protocol stream=login') -or -not $bounded.Contains('protocol stream=world') -or
        -not $bounded.Contains('protocol events truncated after 2000 approved events') -or
        $bounded.Contains('protocol event=PANIC')) {
        throw 'Protocol event import must be capped and report the truncation.'
    }
    $originalReportPath = $report.Path
    $badPath = Join-Path ([IO.Path]::GetTempPath()) ('report-denied-' + [Guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $badPath)
    $report.Path = $badPath
    $warning = Write-OfflineDiagnosticStage -Report $report -Stage 'client_exit' 3>&1 | Out-String
    if ($warning -notmatch 'privacy-safe session report could not be updated' -or $warning -match [regex]::Escape($badPath)) {
        throw 'Report write failures must warn with static path-free text.'
    }
    $report.Path = $originalReportPath
    Remove-Item -LiteralPath $badPath -Force
} finally {
    Remove-Item -LiteralPath $tempLog -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $mapLog -Force -ErrorAction SilentlyContinue
    foreach ($item in @($report, $boundedReport)) {
        if ($item -and (Test-Path -LiteralPath $item.Path)) { Remove-Item -LiteralPath $item.Path -Force }
    }
}

'Offline diagnostics self-check passed (allowlist, unknown-line omission, and seeded privacy markers).'
