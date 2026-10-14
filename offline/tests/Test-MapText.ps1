#Requires -Version 7.0
$ErrorActionPreference = 'Stop'

$offlineRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$targetRoot = (Resolve-Path (Join-Path $offlineRoot 'target')).Path
$rustSource = Join-Path $offlineRoot 'tools\Audit-MapText.rs'
$wrapper = Join-Path $offlineRoot 'tools\Audit-MapText.ps1'
$marker = 'PRIVATE_MAPTEXT_MARKER_DO_NOT_PRINT'
$testRoot = Join-Path $targetRoot ('map-text-test-' + [guid]::NewGuid().ToString('N'))
$clientRoot = Join-Path $testRoot 'client'
$dbRoot = Join-Path $testRoot 'db'
$exe = Join-Path $testRoot 'Audit-MapText.exe'
$rustError = 'Map text audit failed: invalid input or unsupported archive.'
$wrapperError = 'Map text coverage audit failed: invalid input or unsupported archive.'
$script:currentCheck = 'unspecified'

function Write-U32($Writer, [uint32] $Value) { $Writer.Write($Value) }

function New-TableArchive([string] $Root, [string] $Name83, [string] $Name84, [object] $Payload83, [object] $Payload84, [string] $ConfigName = 'strtable.csv', [object] $ConfigPayload = 'CLIENT_DATA_CODE_PAGE,"932"') {
    $xls = Join-Path $Root 'data\xls'
    New-Item -ItemType Directory -Force -Path $xls | Out-Null
    $names = [System.Collections.Generic.List[string]]::new()
    for ($i = 1; $i -le 142; $i++) { $names.Add("padding$i.csv") }
    $names[82] = $Name83
    $names[83] = $Name84
    $names[141] = $ConfigName

    $nameStream = [IO.MemoryStream]::new()
    $nameWriter = [IO.BinaryWriter]::new($nameStream)
    Write-U32 $nameWriter ([uint32]$names.Count)
    foreach ($name in $names) {
        $nameWriter.Write([Text.Encoding]::ASCII.GetBytes($name))
        $nameWriter.Write([byte]0)
    }
    $nameWriter.Write([byte]0)
    $nameWriter.Dispose()
    $blocks = [System.Collections.Generic.List[byte[]]]::new()
    $blocks.Add($nameStream.ToArray())
    $nameStream.Dispose()
    for ($i = 1; $i -le 142; $i++) { $blocks.Add([byte[]]::new(0)) }
    $blocks[83] = if ($Payload83 -is [byte[]]) { $Payload83 } else { [Text.Encoding]::ASCII.GetBytes([string]$Payload83) }
    $blocks[84] = if ($Payload84 -is [byte[]]) { $Payload84 } else { [Text.Encoding]::ASCII.GetBytes([string]$Payload84) }
    $blocks[142] = if ($ConfigPayload -is [byte[]]) { $ConfigPayload } else { [Text.Encoding]::ASCII.GetBytes([string]$ConfigPayload) }

    $datStream = [IO.MemoryStream]::new()
    $datWriter = [IO.BinaryWriter]::new($datStream)
    $records = [System.Collections.Generic.List[object]]::new()
    $offset = [uint32]0
    foreach ($block in $blocks) {
        $records.Add([pscustomobject]@{ Offset = $offset; Length = [uint32]$block.Length })
        $datWriter.Write($block)
        $offset += [uint32]$block.Length
    }
    $datWriter.Dispose()
    $hedStream = [IO.MemoryStream]::new()
    $hedWriter = [IO.BinaryWriter]::new($hedStream)
    foreach ($record in $records) {
        Write-U32 $hedWriter $record.Offset
        Write-U32 $hedWriter $record.Length
        Write-U32 $hedWriter $record.Length
    }
    $hedWriter.Dispose()
    [IO.File]::WriteAllBytes((Join-Path $xls 'table.hed'), $hedStream.ToArray())
    [IO.File]::WriteAllBytes((Join-Path $xls 'table.dat'), $datStream.ToArray())
    $hedStream.Dispose()
    $datStream.Dispose()
}

function New-MapZip([string] $Path, [string[]] $Names) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew)
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $false)
        try {
            foreach ($name in $Names) {
                $entry = $zip.CreateEntry($name)
                $entryStream = $entry.Open()
                $entryStream.Dispose()
            }
        }
        finally { $zip.Dispose() }
    }
    finally { $stream.Dispose() }
}

function Invoke-Process([string] $File, [string[]] $Arguments, [string] $InputText = '') {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $File
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.RedirectStandardInput = $true
    $start.StandardInputEncoding = [Text.UTF8Encoding]::new($false)
    foreach ($argument in $Arguments) { $start.ArgumentList.Add([string]$argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    [void]$process.Start()
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.StandardInput.Write($InputText)
    $process.StandardInput.Close()
    $process.WaitForExit()
    $result = [pscustomobject]@{
        ExitCode = $process.ExitCode
        Stdout = $stdoutTask.GetAwaiter().GetResult().TrimEnd("`r", "`n")
        Stderr = $stderrTask.GetAwaiter().GetResult().TrimEnd("`r", "`n")
    }
    $process.Dispose()
    return $result
}

function Invoke-Audit([string] $Client = $clientRoot, [string] $Db = $dbRoot) {
    Invoke-Process -File (Join-Path $PSHOME 'pwsh.exe') -Arguments @('-NoLogo', '-NoProfile', '-File', $wrapper, $Client, $Db, $exe)
}

function Assert-Result($Result, [int] $ExitCode, [string] $Stdout, [string] $Stderr) {
    if ($Result.ExitCode -ne $ExitCode -or $Result.Stdout -cne $Stdout -or $Result.Stderr -cne $Stderr) {
        throw "Map text audit check '$script:currentCheck' returned an unexpected result."
    }
    if (($Result.Stdout + $Result.Stderr).Contains($marker) -or ($Result.Stdout + $Result.Stderr).Contains($testRoot)) {
        throw 'Map text audit exposed private fixture text or a fixture path.'
    }
}

function Assert-WrapperFailure($Result) { Assert-Result $Result 1 '' $wrapperError }
function Assert-RustFailure($Result) { Assert-Result $Result 1 '' $rustError }

function Join-ByteArrays([object[]] $Parts) {
    $stream = [IO.MemoryStream]::new()
    try { foreach ($part in $Parts) { $stream.Write([byte[]]$part, 0, ([byte[]]$part).Length) }; return ,$stream.ToArray() }
    finally { $stream.Dispose() }
}

function New-TestClient([string] $Name, [object] $Payload83, [object] $Payload84, [string] $ConfigName = 'strtable.csv', [object] $ConfigPayload = 'CLIENT_DATA_CODE_PAGE,"932"') {
    $root = Join-Path $testRoot $Name
    New-TableArchive -Root $root -Name83 'mapname_autorun.csv' -Name84 'mapname_overlap.csv' -Payload83 $Payload83 -Payload84 $Payload84 -ConfigName $ConfigName -ConfigPayload $ConfigPayload
    return $root
}

function Assert-BadConfig([string] $Name, [string] $ConfigName, [object] $ConfigPayload) {
    $root = New-TestClient $Name $autorun $overlap $ConfigName $ConfigPayload
    $script:currentCheck = "configuration $Name"
    Assert-RustFailure (Invoke-Process -File $exe -Arguments @($root) -InputText "1001`n")
}

try {
    New-Item -ItemType Directory -Path $clientRoot, $dbRoot | Out-Null
    $autorun = "#private header`r`n1001,$marker,a`r`n#999999,$marker,a`r`n`r`n1002,label,b`r`n1002,duplicate,c`r`n,missing,d`r`nname,bad,e`r`n0,zero,f`r`n4294967296,overflow,g`r`n1003,short`r`n #leading-space comment,a,b`r`n#private final,$marker,c`r`n"
    $overlap = "#private header`r`n1001,$marker`r`n5555,missing`r`nname,bad`r`n"
    New-TableArchive -Root $clientRoot -Name83 'mapname_autorun.csv' -Name84 'mapname_overlap.csv' -Payload83 $autorun -Payload84 $overlap
    $zipPath = Join-Path $dbRoot 'MapInfo.zip'
    New-MapZip -Path $zipPath -Names @('1001.map', '1002.map', '9999.map')
    $checkZip = [IO.Compression.ZipArchive]::new([IO.File]::OpenRead($zipPath), [IO.Compression.ZipArchiveMode]::Read)
    try { if ($checkZip.Entries.Count -ne 3) { throw 'Synthetic map ZIP fixture is incomplete.' } }
    finally { $checkZip.Dispose() }
    $sourceFiles = @((Join-Path $clientRoot 'data\xls\table.hed'), (Join-Path $clientRoot 'data\xls\table.dat'), $zipPath)
    $hashesBefore = @{}
    foreach ($path in $sourceFiles) { $hashesBefore[$path] = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash }

    & rustc --edition=2021 $rustSource -o $exe
    if ($LASTEXITCODE -ne 0) { throw 'rustc failed to build the map text audit.' }

    $expected = @'
Map text byte audit
Reference archive index IDs: 3
Autorun rows: 9
Autorun three-field row-shape mismatches: 1
Autorun unique positive-u32 candidates: 3
Autorun missing first fields: 1
Autorun nonnumeric first fields: 2
Autorun zero decimal fields: 1
Autorun overflow decimal fields: 1
Autorun duplicate numeric candidates: 1
Autorun candidates absent from reference index: 1
Autorun unique reference index matches: 2
Autorun empty fields: 1
Overlap rows: 3
Overlap unique positive-u32 candidates: 2
Overlap missing first fields: 0
Overlap nonnumeric first fields: 1
Overlap zero decimal fields: 0
Overlap overflow decimal fields: 0
Overlap duplicate numeric candidates: 0
Overlap candidates absent from reference index: 1
Overlap unique reference index matches: 1
Overlap empty fields: 0
Reference source: map archive index only
Encoding: CP932 byte validation only; runtime unverified
Schema: unverified
English coverage: unverified
'@.TrimEnd()
    $script:currentCheck = 'valid fixture'
    Assert-Result (Invoke-Audit) 0 $expected ''

    $lexicalOverlap = "#private header`r`n1001`r`n1002,5,text`r`n1003,5,`r`n1004,5`r`n1005,0,$marker,,999`r`n1007,-1,$marker,,888`r`n1006,2,a,3,b`r`n#private comment,777`r`n`r`n"
    $lexicalRoot = New-TestClient 'overlap-lexical-counts' $autorun $lexicalOverlap
    $lexicalExpected = $expected -replace '(?s)Overlap rows: 3.*?Overlap empty fields: 0', "Overlap rows: 7`nOverlap unique positive-u32 candidates: 7`nOverlap missing first fields: 0`nOverlap nonnumeric first fields: 0`nOverlap zero decimal fields: 0`nOverlap overflow decimal fields: 0`nOverlap duplicate numeric candidates: 0`nOverlap candidates absent from reference index: 5`nOverlap unique reference index matches: 2`nOverlap empty fields: 3"
    $script:currentCheck = 'overlap raw lexical counts'
    Assert-Result (Invoke-Audit -Client $lexicalRoot -Db $dbRoot) 0 $lexicalExpected ''

    $secondaryAutorun = "#private header`r`n1001,5,$marker`r`n1001,6,$marker`r`n1002,7,$marker"
    $secondaryOverlap = "#private header`r`n2001,5,$marker,6,$marker`r`n2001,7,$marker"
    $secondaryRoot = New-TestClient 'secondary-numeric-fields' $secondaryAutorun $secondaryOverlap
    $secondaryDb = Join-Path $testRoot 'secondary-db'
    New-Item -ItemType Directory -Path $secondaryDb | Out-Null
    New-MapZip -Path (Join-Path $secondaryDb 'MapInfo.zip') -Names @('5.map', '6.map', '7.map')
    $secondaryExpected = $expected -replace '(?s)Autorun rows: 9.*?Autorun empty fields: 1', "Autorun rows: 3`nAutorun three-field row-shape mismatches: 0`nAutorun unique positive-u32 candidates: 2`nAutorun missing first fields: 0`nAutorun nonnumeric first fields: 0`nAutorun zero decimal fields: 0`nAutorun overflow decimal fields: 0`nAutorun duplicate numeric candidates: 1`nAutorun candidates absent from reference index: 2`nAutorun unique reference index matches: 0`nAutorun empty fields: 0"
    $secondaryExpected = $secondaryExpected -replace '(?s)Overlap rows: 3.*?Overlap empty fields: 0', "Overlap rows: 2`nOverlap unique positive-u32 candidates: 1`nOverlap missing first fields: 0`nOverlap nonnumeric first fields: 0`nOverlap zero decimal fields: 0`nOverlap overflow decimal fields: 0`nOverlap duplicate numeric candidates: 1`nOverlap candidates absent from reference index: 1`nOverlap unique reference index matches: 0`nOverlap empty fields: 0"
    $script:currentCheck = 'secondary numeric fields excluded from candidates'
    Assert-Result (Invoke-Audit -Client $secondaryRoot -Db $secondaryDb) 0 $secondaryExpected ''

    Assert-BadConfig 'config-missing' 'wrong.csv' 'CLIENT_DATA_CODE_PAGE,"932"'
    Assert-BadConfig 'config-duplicate' 'strtable.csv' "CLIENT_DATA_CODE_PAGE,`"932`"`r`nCLIENT_DATA_CODE_PAGE,`"932`""
    Assert-BadConfig 'config-other-codepage' 'strtable.csv' 'CLIENT_DATA_CODE_PAGE,"943"'
    Assert-BadConfig 'config-unquoted-value' 'strtable.csv' 'CLIENT_DATA_CODE_PAGE,932'
    Assert-BadConfig 'config-quoted-key' 'strtable.csv' '"CLIENT_DATA_CODE_PAGE","932"'
    Assert-BadConfig 'config-extra-field' 'strtable.csv' 'CLIENT_DATA_CODE_PAGE,"932",extra'

    $cp932 = [byte[]](0x82, 0xA0)
    $autorunRest = $autorun.Substring($autorun.IndexOf("`r`n") + 2)
    $overlapRest = $overlap.Substring($overlap.IndexOf("`r`n") + 2)
    $validCp932Autorun = Join-ByteArrays @([Text.Encoding]::ASCII.GetBytes('#private '), $cp932, [Text.Encoding]::ASCII.GetBytes(" header`r`n1001,"), $cp932, [Text.Encoding]::ASCII.GetBytes(",a`r`n" + $autorunRest.Substring($autorunRest.IndexOf("`r`n") + 2)))
    $validCp932Overlap = Join-ByteArrays @([Text.Encoding]::ASCII.GetBytes('#private '), $cp932, [Text.Encoding]::ASCII.GetBytes(" header`r`n" + $overlapRest))
    $validCp932Root = New-TestClient 'valid-cp932' $validCp932Autorun $validCp932Overlap
    $script:currentCheck = 'valid CP932 bytes in private headers'
    Assert-Result (Invoke-Process -File $exe -Arguments @($validCp932Root) -InputText "1001`n1002`n9999`n") 0 $expected ''

    $badLead = [byte[]](0x82)
    $badTrail = [byte[]](0x82, 0x20)
    $cases = @(
        @{ Name = 'bad-cp932-autorun-header'; A = (Join-ByteArrays @([Text.Encoding]::ASCII.GetBytes('#private '), $badLead, [Text.Encoding]::ASCII.GetBytes(" header`r`n" + $autorunRest))); O = $overlap },
        @{ Name = 'bad-cp932-autorun-comment'; A = (Join-ByteArrays @([Text.Encoding]::ASCII.GetBytes("#private `r`n#bad "), $badTrail, [Text.Encoding]::ASCII.GetBytes("`r`n" + $autorunRest))); O = $overlap },
        @{ Name = 'bad-cp932-overlap-header'; A = $autorun; O = (Join-ByteArrays @([Text.Encoding]::ASCII.GetBytes('#private '), $badLead, [Text.Encoding]::ASCII.GetBytes(" header`r`n" + $overlapRest))) },
        @{ Name = 'bad-cp932-overlap-comment'; A = $autorun; O = (Join-ByteArrays @([Text.Encoding]::ASCII.GetBytes("#private `r`n#bad "), $badTrail, [Text.Encoding]::ASCII.GetBytes("`r`n" + $overlapRest))) },
        @{ Name = 'bad-cp932-autorun-final-data'; A = (Join-ByteArrays @([Text.Encoding]::ASCII.GetBytes("#header`r`n1,x`r`n2,"), $badLead)); O = $overlap },
        @{ Name = 'bad-cp932-overlap-final-data'; A = $autorun; O = (Join-ByteArrays @([Text.Encoding]::ASCII.GetBytes("#header`r`n1,x`r`n2,"), $badLead)) }
    )
    foreach ($case in $cases) {
        $badRoot = New-TestClient $case.Name $case.A $case.O
        $script:currentCheck = $case.Name
        Assert-RustFailure (Invoke-Process -File $exe -Arguments @($badRoot) -InputText "1001`n1002`n9999`n")
    }

    # Verify exact table ordinals, stable static failures, and malformed reference-ID handling.
    $wrongOrdinal = Join-Path $testRoot 'wrong-ordinal-client'
    New-TableArchive -Root $wrongOrdinal -Name83 'wrong.csv' -Name84 'mapname_overlap.csv' -Payload83 $autorun -Payload84 $overlap
    $script:currentCheck = 'ordinal name guard'
    Assert-RustFailure (Invoke-Process -File $exe -Arguments @($wrongOrdinal) -InputText "1001`n")
    Assert-WrapperFailure (Invoke-Audit -Client $wrongOrdinal -Db $dbRoot)
    $badClient = Join-Path $testRoot 'broken-client'
    New-TableArchive -Root $badClient -Name83 'mapname_autorun.csv' -Name84 'mapname_overlap.csv' -Payload83 $autorun -Payload84 $overlap
    [IO.File]::WriteAllBytes((Join-Path $badClient 'data\xls\table.hed'), [byte[]](1, 2, 3))
    $script:currentCheck = 'broken HED'
    Assert-WrapperFailure (Invoke-Audit $badClient)
    $script:currentCheck = 'missing database input'
    Assert-WrapperFailure (Invoke-Audit $clientRoot (Join-Path $testRoot 'missing-db'))
    $script:currentCheck = 'missing client input'
    Assert-WrapperFailure (Invoke-Audit (Join-Path $testRoot 'missing-client') $dbRoot)
    $script:currentCheck = 'missing wrapper args'
    Assert-WrapperFailure (Invoke-Process -File (Join-Path $PSHOME 'pwsh.exe') -Arguments @('-NoLogo', '-NoProfile', '-File', $wrapper, $clientRoot))
    $script:currentCheck = 'extra wrapper args'
    Assert-WrapperFailure (Invoke-Process -File (Join-Path $PSHOME 'pwsh.exe') -Arguments @('-NoLogo', '-NoProfile', '-File', $wrapper, $clientRoot, $dbRoot, $exe, 'extra'))

    $badIdCases = @(
        @('0.map'), @('name.map'), @('4294967296.map'), @('.map'),
        @('nested/1001.map'), @('nested\1001.map'), @('1001.map', '1001.map'),
        @('1001.map', '01001.map'), @('１２.map'), @("$marker.map"), @()
    )
    for ($caseIndex = 0; $caseIndex -lt $badIdCases.Count; $caseIndex++) {
        $badIds = $badIdCases[$caseIndex]
        $script:currentCheck = "malformed ZIP ID case $caseIndex"
        $badDb = Join-Path $testRoot ('bad-db-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $badDb | Out-Null
        New-MapZip -Path (Join-Path $badDb 'MapInfo.zip') -Names $badIds
        Assert-WrapperFailure (Invoke-Audit $clientRoot $badDb)
    }

    $badInputs = @('', "0`n", "1`n`n", "1`n1`n", "+1`n", "4294967296`n", "１２`n")
    for ($caseIndex = 0; $caseIndex -lt $badInputs.Count; $caseIndex++) {
        $badInput = $badInputs[$caseIndex]
        $script:currentCheck = "malformed stdin ID case $caseIndex"
        Assert-RustFailure (Invoke-Process -File $exe -Arguments @($clientRoot) -InputText $badInput)
    }
    $script:currentCheck = 'extra Rust args'
    Assert-RustFailure (Invoke-Process -File $exe -Arguments @($clientRoot, 'extra') -InputText "1001`n")
    $script:currentCheck = 'valid Rust CRLF IDs'
    Assert-Result (Invoke-Process -File $exe -Arguments @($clientRoot) -InputText "1001`r`n1002`n") 0 $expected.Replace('Reference archive index IDs: 3', 'Reference archive index IDs: 2') ''

    foreach ($path in $sourceFiles) {
        if ((Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash -cne $hashesBefore[$path]) {
            throw 'Map text audit modified a synthetic input.'
        }
    }
    'Map text audit tests passed.'
}
finally {
    $resolvedTestRoot = [IO.Path]::GetFullPath($testRoot).TrimEnd('\')
    $resolvedTargetRoot = [IO.Path]::GetFullPath($targetRoot).TrimEnd('\')
    if (-not $resolvedTestRoot.StartsWith($resolvedTargetRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Refusing cleanup outside offline/target.'
    }
    if (Test-Path -LiteralPath $resolvedTestRoot) { Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force }
}
