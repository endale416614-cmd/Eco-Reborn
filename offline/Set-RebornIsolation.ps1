<#
.SYNOPSIS
  Reversibly blocks the ECO client and launcher from non-loopback network peers.
.DESCRIPTION
  Enable adds per-application Windows Firewall block rules for the installed game clients,
  their launchers, and any eco-reborn.exe debug/release build found in offline/target.
  Rules exclude IPv4 loopback and IPv6 ::1 so a local emulator can communicate. Enable requires UAC
  elevation and an already-enabled firewall on every profile. Disable also requires elevation
  and removes only rules in this script's dedicated group; Status verifies the full effective
  rule set in ActiveStore. The rules do not isolate other
  processes, and they do not disable or change global firewall settings.
.PARAMETER Action
  Enable, Disable, or Status. The default is Status.
.EXAMPLE
  .\Set-RebornIsolation.ps1 Enable
  Run from an elevated PowerShell window, then use Disable to restore the previous rule state.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('Enable', 'Disable', 'Status')]
    [string] $Action = 'Status'
)

$script:RuleGroup = 'Eco Reborn Isolation (managed by Set-RebornIsolation.ps1)'
# Group used before the 2026-10-14 rebrand; Disable also removes its rules.
$script:LegacyRuleGroups = @('ECO Offline Isolation (managed by Set-OfflineIsolation.ps1)')
$script:RemoteRanges = @(
    '0.0.0.0-126.255.255.255',
    '128.0.0.0-255.255.255.255',
    '::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff'
)

function Get-OfflineAppPaths {
    $root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
    $jpDir = Join-Path $root 'eco_jp'
    $paths = @((Join-Path $jpDir 'eco.exe'), (Join-Path $jpDir 'eco_launch.exe'))
    foreach ($configuration in @('debug', 'release')) {
        $optional = Join-Path $PSScriptRoot "target\$configuration\eco-reborn.exe"
        if (Test-Path -LiteralPath $optional -PathType Leaf) { $paths += $optional }
    }

    foreach ($path in $paths) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Required application executable was not found: $path"
        }
        $resolved = (Resolve-Path -LiteralPath $path -ErrorAction Stop).Path
        if ([IO.Path]::GetExtension($resolved) -ne '.exe' -or
            -not $resolved.StartsWith($root + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Executable path is outside the expected workspace or is not an .exe: $resolved"
        }
        $resolved
    }
}

function Get-OfflineFirewallPlan {
    param([string[]] $Paths)
    foreach ($path in $Paths) {
        $normalizedPath = [IO.Path]::GetFullPath($path).ToUpperInvariant()
        $hash = [Security.Cryptography.SHA256]::Create()
        try { $pathId = ([BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes($normalizedPath))).Replace('-', '').Substring(0, 12)) }
        finally { $hash.Dispose() }
        foreach ($direction in @('Inbound', 'Outbound')) {
            [pscustomobject]@{
                Name          = 'EcoReborn_' + $pathId + '_' + $direction
                Group         = $script:RuleGroup
                Program       = $path
                Direction     = $direction
                Action        = 'Block'
                Profile       = 'Any'
                Protocol      = 'Any'
                RemoteAddress = $script:RemoteRanges
            }
        }
    }
}

function Test-IsAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Test-RuleMatchesPlan {
    param($Rule, $Plan, $ApplicationFilter, $AddressFilter, $PortFilter)
    $Rule.Enabled -eq 'True' -and $Rule.Action -eq 'Block' -and
        $Rule.Direction -eq $Plan.Direction -and $Rule.Profile -eq 'Any' -and
        $ApplicationFilter.Program -eq $Plan.Program -and
        $AddressFilter.RemoteAddress.Count -eq $Plan.RemoteAddress.Count -and
        (@($AddressFilter.RemoteAddress | Sort-Object) -join ',') -eq (@($Plan.RemoteAddress | Sort-Object) -join ',') -and
        $PortFilter.Protocol -eq 'Any' -and $PortFilter.LocalPort -eq 'Any' -and $PortFilter.RemotePort -eq 'Any'
}

function Test-OfflineRuleNames {
    param([object[]] $Rules, [object[]] $Plan)
    if ($Rules.Count -ne $Plan.Count) { return $false }
    foreach ($entry in $Plan) {
        $matches = @($Rules | Where-Object { $_.Name -eq $entry.Name -and $_.Group -eq $script:RuleGroup })
        if ($matches.Count -ne 1) { return $false }
    }
    return $true
}

function Test-OfflineProfilesEnabled {
    param([object[]] $Profiles)
    $Profiles.Count -gt 0 -and @($Profiles | Where-Object { $_.Enabled -ne 'True' }).Count -eq 0
}

function Test-OfflineRuleSet {
    param([object[]] $Rules, [object[]] $Plan)
    if (-not (Test-OfflineRuleNames -Rules $Rules -Plan $Plan)) { return $false }
    foreach ($entry in $Plan) {
        $matches = @($Rules | Where-Object { $_.Name -eq $entry.Name })
        if ($matches.Count -ne 1 -or $matches[0].Group -ne $script:RuleGroup) { return $false }
        $app = Get-NetFirewallApplicationFilter -AssociatedNetFirewallRule $matches[0] -ErrorAction Stop
        $address = Get-NetFirewallAddressFilter -AssociatedNetFirewallRule $matches[0] -ErrorAction Stop
        $port = Get-NetFirewallPortFilter -AssociatedNetFirewallRule $matches[0] -ErrorAction Stop
        if (-not (Test-RuleMatchesPlan $matches[0] $entry $app $address $port)) { return $false }
    }
    return $true
}

function Invoke-OfflineIsolation {
    param([string] $SelectedAction)
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'This script requires Windows PowerShell or PowerShell on Windows.' }
    if (-not (Get-Command Get-NetFirewallRule -ErrorAction SilentlyContinue)) {
        throw 'Windows Defender Firewall cmdlets are unavailable.'
    }

    if ($SelectedAction -eq 'Disable') {
        if (-not (Test-IsAdministrator)) { throw 'Run this action from an elevated PowerShell window (UAC). No rules were changed.' }
        $allPersistentRules = @(Get-NetFirewallRule -PolicyStore PersistentStore -ErrorAction Stop)
        $groups = @($script:RuleGroup) + $script:LegacyRuleGroups
        $owned = @($allPersistentRules | Where-Object { $groups -contains $_.Group })
        if ($owned.Count) { $owned | Remove-NetFirewallRule -ErrorAction Stop }
        $allActiveRules = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction Stop)
        if (@($allActiveRules | Where-Object { $groups -contains $_.Group }).Count) {
            throw 'Persistent rules were removed, but ActiveStore still contains rules in the isolation group.'
        }
        'Offline isolation rules removed.'
        return
    }

    if ($SelectedAction -eq 'Status') {
        $plan = @(Get-OfflineFirewallPlan -Paths (Get-OfflineAppPaths))
        $profiles = @(Get-NetFirewallProfile -PolicyStore ActiveStore -ErrorAction Stop)
        if (-not (Test-OfflineProfilesEnabled -Profiles $profiles)) {
            throw 'Status cannot confirm isolation because one or more active firewall profiles are disabled or unavailable.'
        }
        $allActiveRules = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction Stop)
        $owned = @($allActiveRules | Where-Object { $_.Group -eq $script:RuleGroup })
        if ($owned.Count) {
            if (-not (Test-OfflineRuleSet -Rules $owned -Plan $plan)) {
                throw 'Offline isolation is partial or mismatched in ActiveStore. Do not rely on isolation until repaired.'
            }
            $owned | Select-Object DisplayName, Direction, Action, Enabled, Profile
        } else {
            'Offline isolation is disabled (no managed firewall rules found).'
        }
        return
    }

    if (-not (Test-IsAdministrator)) { throw 'Run this action from an elevated PowerShell window (UAC). No rules were changed.' }
    $plan = @(Get-OfflineFirewallPlan -Paths (Get-OfflineAppPaths))

    $profiles = @(Get-NetFirewallProfile -PolicyStore ActiveStore -ErrorAction Stop)
    if (-not (Test-OfflineProfilesEnabled -Profiles $profiles)) {
        $disabledProfiles = @($profiles | Where-Object { $_.Enabled -ne 'True' })
        if (-not $profiles.Count) { throw 'No active firewall profiles were returned; no rules were changed.' }
        throw ('Firewall must already be enabled for every profile. Disabled: ' + (($disabledProfiles.Name) -join ', '))
    }

    $allActiveRules = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction Stop)
    foreach ($entry in $plan) {
        $existing = @($allActiveRules | Where-Object { $_.Name -eq $entry.Name })
        if ($existing.Count) {
            if ($existing.Count -ne 1 -or $existing[0].Group -ne $script:RuleGroup) {
                throw "A firewall rule named '$($entry.Name)' exists but is not this script's expected rule. No rule was overwritten."
            }
            $app = Get-NetFirewallApplicationFilter -AssociatedNetFirewallRule $existing[0] -ErrorAction Stop
            $address = Get-NetFirewallAddressFilter -AssociatedNetFirewallRule $existing[0] -ErrorAction Stop
            $port = Get-NetFirewallPortFilter -AssociatedNetFirewallRule $existing[0] -ErrorAction Stop
            if (-not (Test-RuleMatchesPlan $existing[0] $entry $app $address $port)) {
                throw "A firewall rule named '$($entry.Name)' exists but does not match the expected rule. No rule was overwritten."
            }
            continue
        }
        New-NetFirewallRule -Name $entry.Name -DisplayName ('Eco Reborn isolation: ' + [IO.Path]::GetFileName($entry.Program) + ' ' + $entry.Direction) `
            -Group $entry.Group -Program $entry.Program -Direction $entry.Direction -Action Block `
            -Profile Any -Protocol Any -RemoteAddress $entry.RemoteAddress -Enabled True -ErrorAction Stop | Out-Null
    }
    $allActiveRules = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction Stop)
    $effectiveRules = @($allActiveRules | Where-Object { $_.Group -eq $script:RuleGroup })
    if (-not (Test-OfflineRuleSet -Rules $effectiveRules -Plan $plan)) {
        throw 'Enable did not produce the complete matching rule set in ActiveStore. Check the firewall policy and retry.'
    }
    'Offline isolation enabled for the game client and launcher (plus the local emulator if present). Loopback remains allowed.'
}

if ($MyInvocation.InvocationName -ne '.') {
    try { Invoke-OfflineIsolation -SelectedAction $Action }
    catch {
        $message = $_.Exception.Message
        if ($message -match 'access is denied|access denied') {
            $message = 'Windows denied access to firewall data. Run PowerShell as Administrator and retry.'
        }
        Write-Error $message
        exit 1
    }
}
