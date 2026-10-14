#Requires -Version 7.0
$ErrorActionPreference = 'Stop'

$offlineRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$targetRoot = (Resolve-Path (Join-Path $offlineRoot 'target')).Path
$source = Join-Path $offlineRoot 'tools\Inspect-ClientText.rs'
$marker = 'PRIVATE_MARKER_DO_NOT_PRINT'
$testRoot = Join-Path $targetRoot ('client-text-test-' + [guid]::NewGuid().ToString('N'))
$privateRoot = Join-Path $testRoot ($marker + '-path')
$exe = Join-Path $testRoot 'inspect-client-text.exe'
$errorText = 'Text archive inspection failed: missing, malformed, or unsupported input.'

function Write-U32($Writer, [uint32] $Value) {
    $Writer.Write($Value)
}

function New-ArchivePair([string] $XlsPath, [string] $BaseName) {
    New-Item -ItemType Directory -Force -Path $XlsPath | Out-Null
    $names = @('GuestIDAgreement.txt', ($marker + '.txt'))
    $nameStream = [System.IO.MemoryStream]::new()
    $nameWriter = [System.IO.BinaryWriter]::new($nameStream)
    Write-U32 $nameWriter ([uint32]$names.Count)
    foreach ($name in $names) {
        $nameWriter.Write([Text.Encoding]::ASCII.GetBytes($name))
        $nameWriter.Write([byte]0)
    }
    $nameWriter.Write([byte]0)
    $nameWriter.Dispose()
    $nameData = $nameStream.ToArray()
    $nameStream.Dispose()

    $payloads = @("Agreement content $marker", "Private content $marker")
    $blocks = [System.Collections.Generic.List[byte[]]]::new()
    $blocks.Add($nameData)
    foreach ($payload in $payloads) { $blocks.Add([Text.Encoding]::ASCII.GetBytes($payload)) }
    $datStream = [System.IO.MemoryStream]::new()
    $datWriter = [System.IO.BinaryWriter]::new($datStream)
    $records = [System.Collections.Generic.List[object]]::new()
    $offset = [uint32]0
    foreach ($block in $blocks) {
        $records.Add([pscustomobject]@{ Offset = $offset; Length = [uint32]$block.Length })
        $datWriter.Write($block)
        $offset += [uint32]$block.Length
    }
    $datWriter.Dispose()

    $hedStream = [System.IO.MemoryStream]::new()
    $hedWriter = [System.IO.BinaryWriter]::new($hedStream)
    foreach ($record in $records) {
        Write-U32 $hedWriter $record.Offset
        Write-U32 $hedWriter $record.Length
        Write-U32 $hedWriter $record.Length
    }
    $hedWriter.Dispose()
    [IO.File]::WriteAllBytes((Join-Path $XlsPath ($BaseName + '.hed')), $hedStream.ToArray())
    [IO.File]::WriteAllBytes((Join-Path $XlsPath ($BaseName + '.dat')), $datStream.ToArray())
    $hedStream.Dispose()
    $datStream.Dispose()
}

function Invoke-Inspector([string[]] $Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $exe
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add([string]$argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    [void]$process.Start()
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    $result = [pscustomobject]@{ ExitCode = $process.ExitCode; Stdout = $stdout.TrimEnd("`r", "`n"); Stderr = $stderr.TrimEnd("`r", "`n") }
    $process.Dispose()
    $result
}

function Assert-Success($Result, [string] $Expected) {
    if ($Result.ExitCode -ne 0 -or $Result.Stdout -cne $Expected -or $Result.Stderr -ne '') {
        throw 'Inspector did not produce the expected successful result.'
    }
    if (($Result.Stdout + $Result.Stderr).Contains($marker)) { throw 'Private marker leaked into successful output.' }
}

function Assert-Failure($Result) {
    if ($Result.ExitCode -eq 0 -or $Result.Stdout -ne '' -or $Result.Stderr -cne $errorText) {
        throw 'Inspector did not produce the fixed error result.'
    }
    if (($Result.Stdout + $Result.Stderr).Contains($marker)) { throw 'Private marker leaked into error output.' }
}

try {
    New-Item -ItemType Directory -Path $privateRoot | Out-Null
    $textXls = Join-Path $privateRoot 'data\xls'
    $tableXls = Join-Path $privateRoot 'table-client\data\xls'
    New-ArchivePair $textXls 'text'
    New-ArchivePair $tableXls 'table'
    $inputFiles = @(
        (Join-Path $textXls 'text.hed'), (Join-Path $textXls 'text.dat'),
        (Join-Path $tableXls 'table.hed'), (Join-Path $tableXls 'table.dat')
    )
    $hashesBefore = @{}; foreach ($path in $inputFiles) { $hashesBefore[$path] = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash }

    & rustc --edition=2021 $source -o $exe
    if ($LASTEXITCODE -ne 0) { throw 'rustc failed to build the inspector.' }

    $firstLength = [Text.Encoding]::ASCII.GetByteCount("Agreement content $marker")
    $secondLength = [Text.Encoding]::ASCII.GetByteCount("Private content $marker")
    $expectedEntries = "entry 1: $firstLength bytes`nentry 2: $secondLength bytes"
    Assert-Success (Invoke-Inspector @($privateRoot)) "experimental text archive entries: 2`n$expectedEntries"
    Assert-Success (Invoke-Inspector @($privateRoot, 'text')) "experimental text archive entries: 2`n$expectedEntries"
    $tableRoot = Split-Path (Split-Path $tableXls -Parent) -Parent
    Assert-Success (Invoke-Inspector @($tableRoot, 'table')) "experimental table archive entries: 2`n$expectedEntries"
    Assert-Success (Invoke-Inspector @($tableRoot, 'table', '1')) "experimental table archive selected entries: 1`nentry 1: $firstLength bytes"
    Assert-Success (Invoke-Inspector @($tableRoot, 'table', '2')) "experimental table archive selected entries: 1`nentry 2: $secondLength bytes"

    Assert-Failure (Invoke-Inspector @())
    Assert-Failure (Invoke-Inspector @($privateRoot, 'invalid'))
    Assert-Failure (Invoke-Inspector @($privateRoot, 'text', 'extra'))
    Assert-Failure (Invoke-Inspector @((Join-Path $privateRoot 'missing-client')))
    Assert-Failure (Invoke-Inspector @($privateRoot, '../text'))
    foreach ($selector in @('0', '+1', '-1', ' 1', '1 ', '4294967296', '3', '１')) {
        Assert-Failure (Invoke-Inspector @($tableRoot, 'table', $selector))
    }
    Assert-Failure (Invoke-Inspector @($tableRoot, 'table', '1', 'extra'))

    $badHedRoot = Join-Path $privateRoot 'bad-hed'
    $badHedXls = Join-Path $badHedRoot 'data\xls'
    New-Item -ItemType Directory -Force -Path $badHedXls | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $badHedXls 'text.hed'), [byte[]](1, 2, 3))
    [IO.File]::WriteAllBytes((Join-Path $badHedXls 'text.dat'), [byte[]](0))
    Assert-Failure (Invoke-Inspector @($badHedRoot))

    $shortDatRoot = Join-Path $privateRoot 'short-dat'
    $shortDatXls = Join-Path $shortDatRoot 'data\xls'
    New-Item -ItemType Directory -Force -Path $shortDatXls | Out-Null
    Copy-Item -LiteralPath (Join-Path $textXls 'text.hed') -Destination (Join-Path $shortDatXls 'text.hed')
    $validDat = [IO.File]::ReadAllBytes((Join-Path $textXls 'text.dat'))
    [IO.File]::WriteAllBytes((Join-Path $shortDatXls 'text.dat'), $validDat[0..($validDat.Length - 2)])
    Assert-Failure (Invoke-Inspector @($shortDatRoot))

    foreach ($path in $inputFiles) {
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash -cne $hashesBefore[$path]) {
            throw 'Inspector modified a synthetic input archive.'
        }
    }
    'Client text archive tests passed.'
}
finally {
    $resolvedTestRoot = [IO.Path]::GetFullPath($testRoot).TrimEnd('\')
    $resolvedTargetRoot = [IO.Path]::GetFullPath($targetRoot).TrimEnd('\')
    if (-not $resolvedTestRoot.StartsWith($resolvedTargetRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Refusing cleanup outside offline/target.'
    }
    if (Test-Path -LiteralPath $resolvedTestRoot) { Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force }
}
