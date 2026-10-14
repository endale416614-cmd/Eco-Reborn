$ErrorActionPreference = 'Stop'
$scriptPath = Join-Path $PSScriptRoot '..\Set-RebornIsolation.ps1'
$tokens = $null
$parseErrors = $null
[System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$parseErrors) | Out-Null
if ($parseErrors.Count) { throw ('Script parse failed: ' + ($parseErrors -join '; ')) }

. $scriptPath
$fakePaths = @('C:\workspace\eco.exe', 'C:\workspace\eco_launcher.exe')
$plan = @(Get-OfflineFirewallPlan -Paths $fakePaths)
if ($plan.Count -ne 4) { throw "Expected four directional rules, got $($plan.Count)." }
if (@($plan | Where-Object { $_.Action -ne 'Block' -or $_.Profile -ne 'Any' }).Count) {
    throw 'Every planned rule must block on every firewall profile.'
}
if (@($plan | Where-Object { $_.Direction -notin @('Inbound', 'Outbound') }).Count) {
    throw 'Rule plan contains an invalid direction.'
}
if (@($plan | Where-Object { $_.RemoteAddress -notcontains '0.0.0.0-126.255.255.255' -or
        $_.RemoteAddress -notcontains '128.0.0.0-255.255.255.255' -or
        $_.RemoteAddress -notcontains '::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff' }).Count) {
    throw 'Rule ranges must exclude IPv4 loopback and IPv6 ::1.'
}
if (@($plan | Where-Object { $_.Program -notin $fakePaths }).Count) {
    throw 'A planned rule targets an unexpected executable.'
}

$duplicateBasenamePlan = @(Get-OfflineFirewallPlan -Paths @('C:\jp\eco.exe', 'C:\classic\eco.exe'))
if ($duplicateBasenamePlan.Count -ne 4 -or
    @($duplicateBasenamePlan.Name | Select-Object -Unique).Count -ne 4) {
    throw 'Paths with the same executable basename must receive distinct firewall rule names.'
}
$expectedRuleRows = @($duplicateBasenamePlan | ForEach-Object {
    [pscustomobject]@{ Name = $_.Name; Group = $_.Group }
})
if (-not (Test-OfflineRuleNames -Rules $expectedRuleRows -Plan $duplicateBasenamePlan)) {
    throw 'A complete firewall rule name set was rejected.'
}
if (Test-OfflineRuleNames -Rules $expectedRuleRows[1..3] -Plan $duplicateBasenamePlan) {
    throw 'A partial firewall rule set must be rejected.'
}
$mismatchedRows = @($expectedRuleRows | Select-Object -First 3) +
    @([pscustomobject]@{ Name = 'wrong'; Group = $script:RuleGroup })
if (Test-OfflineRuleNames -Rules $mismatchedRows -Plan $duplicateBasenamePlan) {
    throw 'A mismatched firewall rule set must be rejected.'
}
$sample = $duplicateBasenamePlan[0]
$rule = [pscustomobject]@{
    Enabled = 'True'; Action = 'Block'; Direction = $sample.Direction; Profile = 'Any'
}
$applicationFilter = [pscustomobject]@{ Program = $sample.Program }
$addressFilter = [pscustomobject]@{ RemoteAddress = $sample.RemoteAddress }
$portFilter = [pscustomobject]@{ Protocol = 'Any'; LocalPort = 'Any'; RemotePort = 'Any' }
if (-not (Test-RuleMatchesPlan $rule $sample $applicationFilter $addressFilter $portFilter)) {
    throw 'A correctly configured firewall rule was rejected.'
}
$portFilter.Protocol = 'TCP'
if (Test-RuleMatchesPlan $rule $sample $applicationFilter $addressFilter $portFilter) {
    throw 'A rule with a mismatched protocol must be rejected.'
}
if (Test-OfflineProfilesEnabled -Profiles @()) { throw 'An empty active profile set must fail.' }
if (Test-OfflineProfilesEnabled -Profiles @([pscustomobject]@{ Name = 'Public'; Enabled = $false })) {
    throw 'A disabled active firewall profile must fail.'
}

# Mock firewall cmdlets to exercise first-time creation and cleanup without touching Windows rules.
$script:MockRules = @()
function Test-IsAdministrator { $true }
function Get-OfflineAppPaths { @('C:\mock\eco.exe', 'D:\mock\eco.exe') }
function Get-NetFirewallProfile { @([pscustomobject]@{ Name = 'Private'; Enabled = $true }) }
function Get-NetFirewallRule {
    [CmdletBinding()]
    param([string] $PolicyStore, [string] $Name, [string] $Group)
    $result = @($script:MockRules)
    if ($Name) { $result = @($result | Where-Object Name -eq $Name) }
    if ($Group) { $result = @($result | Where-Object Group -eq $Group) }
    $result
}
function New-NetFirewallRule {
    [CmdletBinding()]
    param([string] $Name, [string] $DisplayName, [string] $Group, [string] $Program,
        [string] $Direction, [string] $Action, [string] $Profile, [string] $Protocol,
        [string[]] $RemoteAddress, [string] $Enabled)
    $rule = [pscustomobject]@{
        Name = $Name; DisplayName = $DisplayName; Group = $Group; Program = $Program
        Direction = $Direction; Action = $Action; Profile = $Profile; Enabled = $Enabled
        Protocol = $Protocol; RemoteAddress = $RemoteAddress
    }
    $script:MockRules += $rule
    $rule
}
function Get-NetFirewallApplicationFilter {
    [CmdletBinding()]
    param($AssociatedNetFirewallRule)
    [pscustomobject]@{ Program = $AssociatedNetFirewallRule.Program }
}
function Get-NetFirewallAddressFilter {
    [CmdletBinding()]
    param($AssociatedNetFirewallRule)
    [pscustomobject]@{ RemoteAddress = $AssociatedNetFirewallRule.RemoteAddress }
}
function Get-NetFirewallPortFilter {
    [CmdletBinding()]
    param($AssociatedNetFirewallRule)
    [pscustomobject]@{
        Protocol = $AssociatedNetFirewallRule.Protocol; LocalPort = 'Any'; RemotePort = 'Any'
    }
}
function Remove-NetFirewallRule {
    [CmdletBinding()]
    param([Parameter(ValueFromPipeline)] $InputObject)
    process { $script:MockRules = @($script:MockRules | Where-Object Name -ne $InputObject.Name) }
}

$mockPlan = @(Get-OfflineFirewallPlan -Paths (Get-OfflineAppPaths))
Invoke-OfflineIsolation -SelectedAction Enable | Out-Null
if ($script:MockRules.Count -ne 4 -or -not (Test-OfflineRuleSet -Rules $script:MockRules -Plan $mockPlan)) {
    throw 'First-time Enable did not create and verify the complete rule set.'
}
$script:MockRules += [pscustomobject]@{ Name = 'KeepMe'; Group = 'Unrelated' }
function Get-OfflineAppPaths { throw 'Disable should not depend on installed executable paths.' }
Invoke-OfflineIsolation -SelectedAction Disable | Out-Null
if (@($script:MockRules | Where-Object Group -eq $script:RuleGroup).Count -ne 0 -or
    @($script:MockRules | Where-Object Name -eq 'KeepMe').Count -ne 1) {
    throw 'Disable must clean managed rules without touching unrelated rules or requiring executable paths.'
}
'Offline isolation self-check passed (syntax, app scope, duplicate basenames, complete rule set, profiles, and loopback-excluding ranges).'
