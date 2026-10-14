param()

$ErrorActionPreference = 'Stop'
$script:FailureText = 'Map text coverage audit failed: invalid input or unsupported archive.'
$script:MaxZipBytes = 256MB
$script:MaxZipEntries = 500000
$script:MaxIdInputBytes = 1MB

function Get-ReferenceIds([string] $Path) {
    $file = [IO.File]::OpenRead($Path)
    try {
        if ($file.Length -le 0 -or $file.Length -gt $script:MaxZipBytes) { throw 'invalid' }
        $zip = [IO.Compression.ZipArchive]::new($file, [IO.Compression.ZipArchiveMode]::Read, $true)
        try {
            if ($zip.Entries.Count -eq 0 -or $zip.Entries.Count -gt $script:MaxZipEntries) { throw 'invalid' }
            $ids = [Collections.Generic.HashSet[uint32]]::new()
            foreach ($entry in $zip.Entries) {
                if ($entry.FullName.EndsWith('/') -or $entry.FullName.EndsWith([string][char]92)) { throw 'invalid' }
                if ($entry.FullName.Contains('/') -or $entry.FullName.Contains([char]92)) { throw 'invalid' }
                if (-not $entry.FullName.EndsWith('.map', [StringComparison]::OrdinalIgnoreCase)) { throw 'invalid' }
                $baseName = [IO.Path]::GetFileNameWithoutExtension($entry.FullName)
                if ($baseName -cnotmatch '\A[0-9]+\z') { throw 'invalid' }
                [uint32] $id = 0
                if (-not [uint32]::TryParse($baseName, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$id) -or $id -eq 0) { throw 'invalid' }
                if (-not $ids.Add($id)) { throw 'invalid' }
            }
            if ($ids.Count -eq 0) { throw 'invalid' }
            return @($ids | Sort-Object)
        }
        finally { $zip.Dispose() }
    }
    finally { $file.Dispose() }
}

function Test-Report([string] $Output) {
    $expected = @(
        '^Map text byte audit$',
        '^Reference archive index IDs: [0-9]+$',
        '^Autorun rows: [0-9]+$',
        '^Autorun three-field row-shape mismatches: [0-9]+$',
        '^Autorun unique positive-u32 candidates: [0-9]+$',
        '^Autorun missing first fields: [0-9]+$',
        '^Autorun nonnumeric first fields: [0-9]+$',
        '^Autorun zero decimal fields: [0-9]+$',
        '^Autorun overflow decimal fields: [0-9]+$',
        '^Autorun duplicate numeric candidates: [0-9]+$',
        '^Autorun candidates absent from reference index: [0-9]+$',
        '^Autorun unique reference index matches: [0-9]+$',
        '^Autorun empty fields: [0-9]+$',
        '^Overlap rows: [0-9]+$',
        '^Overlap unique positive-u32 candidates: [0-9]+$',
        '^Overlap missing first fields: [0-9]+$',
        '^Overlap nonnumeric first fields: [0-9]+$',
        '^Overlap zero decimal fields: [0-9]+$',
        '^Overlap overflow decimal fields: [0-9]+$',
        '^Overlap duplicate numeric candidates: [0-9]+$',
        '^Overlap candidates absent from reference index: [0-9]+$',
        '^Overlap unique reference index matches: [0-9]+$',
        '^Overlap empty fields: [0-9]+$',
        '^Reference source: map archive index only$',
        '^Encoding: CP932 byte validation only; runtime unverified$',
        '^Schema: unverified$',
        '^English coverage: unverified$'
    )
    $lines = @($Output.TrimEnd("`r", "`n") -split "`r?`n")
    if ($lines.Count -ne $expected.Count) { return $false }
    for ($i = 0; $i -lt $expected.Count; $i++) {
        if ($lines[$i] -cnotmatch $expected[$i]) { return $false }
    }
    return $true
}

try {
    if ($PSVersionTable.PSVersion.Major -lt 7 -or $args.Count -lt 2 -or $args.Count -gt 3) { throw 'invalid' }
    $ClientRoot = [string]$args[0]
    $DbRoot = [string]$args[1]
    $AuditExecutable = if ($args.Count -eq 3) { [string]$args[2] } else { '' }
    if ([string]::IsNullOrWhiteSpace($AuditExecutable)) {
        $AuditExecutable = Join-Path $PSScriptRoot '..\target\Audit-MapText.exe'
    }
    if (-not [IO.File]::Exists($AuditExecutable)) { throw 'invalid' }
    $zipPath = Join-Path $DbRoot 'MapInfo.zip'
    $ids = @(Get-ReferenceIds $zipPath)
    $idLines = ($ids | ForEach-Object { $_.ToString([Globalization.CultureInfo]::InvariantCulture) }) -join "`n"
    $idInput = $idLines + "`n"
    if ([Text.Encoding]::ASCII.GetByteCount($idInput) -gt $script:MaxIdInputBytes) { throw 'invalid' }

    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $AuditExecutable
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.ArgumentList.Add($ClientRoot)
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        if (-not $process.Start()) { throw 'invalid' }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write($idInput)
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(60000)) {
            $process.Kill()
            throw 'invalid'
        }
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0 -or $stderr.Length -ne 0 -or -not (Test-Report $stdout)) { throw 'invalid' }
        [Console]::Out.Write($stdout)
    }
    finally { $process.Dispose() }
}
catch {
    [Console]::Error.WriteLine($script:FailureText)
    exit 1
}
