# Internal structured-input backend. The public setup wrapper supplies live Hive readiness.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([Console]::InputEncoding.CodePage -ne 65001) { [Console]::InputEncoding = [Text.UTF8Encoding]::new($false) }
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$hardwareRequest = [Console]::In.ReadToEnd() | ConvertFrom-Json
if ($hardwareRequest.action -notin @('inspect','disable')) { throw 'Unknown hardware action.' }
$hardwareProtected = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($hardwareEndpoint in $hardwareRequest.protectedEndpointIds) {
    $hardwareCurrent = 'SWD\MMDEVAPI\' + $hardwareEndpoint
    for ($hardwareDepth = 0; $hardwareDepth -lt 32; $hardwareDepth++) {
        if (!$hardwareProtected.Add($hardwareCurrent)) { break }
        $hardwareNode = @(Get-PnpDevice -InstanceId $hardwareCurrent -ErrorAction Stop | Where-Object { $_.InstanceId -ieq $hardwareCurrent })
        if ($hardwareNode.Count -ne 1) { throw 'Configured route endpoint ancestry is unavailable.' }
        if ($hardwareCurrent -ieq 'HTREE\ROOT\0') { break }
        $hardwareParent = Get-PnpDeviceProperty -InstanceId $hardwareCurrent -KeyName 'DEVPKEY_Device_Parent' -ErrorAction Stop
        if ([string]::IsNullOrWhiteSpace($hardwareParent.Data)) { break }
        $hardwareCurrent = [string]$hardwareParent.Data
        if ($hardwareDepth -eq 31) { throw 'Device ancestry exceeds its bound.' }
    }
}
function Read-HardwareTarget([string]$Id) {
    $hardwareMatches = @(Get-PnpDevice -InstanceId $Id -ErrorAction Stop | Where-Object { $_.InstanceId -ceq $Id })
    if ($hardwareMatches.Count -ne 1) { throw 'Exact hardware target unavailable.' }
    $hardwareDevice = $hardwareMatches[0]
    if ($hardwareDevice.Class -ne 'MEDIA' -or $hardwareProtected.Contains($Id) -or
        $hardwareDevice.FriendlyName -match '(?i)VB-Audio|CABLE|Voicemeeter|Virtual' -or $Id -notmatch '^(?i)(HDAUDIO|PCI|USB)\\') {
        throw 'Target is virtual, outside physical audio hardware, or part of a configured route.'
    }
    $hardwareIds = @((Get-PnpDeviceProperty -InstanceId $Id -KeyName 'DEVPKEY_Device_HardwareIds' -ErrorAction Stop).Data)
    $hardwareProblem = (Get-PnpDeviceProperty -InstanceId $Id -KeyName 'DEVPKEY_Device_ProblemCode' -ErrorAction Stop).Data
    if ($hardwareProblem -notin @(0,22)) { throw 'Hardware has an unrelated device problem.' }
    return [ordered]@{ instanceId = $Id; name = [string]$hardwareDevice.FriendlyName; hardwareIds = $hardwareIds; disabled = ($hardwareProblem -eq 22) }
}
$hardwareTargets = @($hardwareRequest.instanceIds | ForEach-Object { Read-HardwareTarget $_ })
if ($hardwareRequest.action -eq 'disable') {
    if ($hardwareTargets.Count -ne 1) { throw 'One exact hardware mutation per command.' }
    $hardwareTarget = $hardwareTargets[0]; $hardwareExpected = $hardwareRequest.expected
    if ($hardwareExpected.instanceId -cne $hardwareTarget.instanceId -or $hardwareExpected.name -cne $hardwareTarget.name -or
        (ConvertTo-Json -Compress -InputObject @($hardwareExpected.hardwareIds)) -cne (ConvertTo-Json -Compress -InputObject @($hardwareTarget.hardwareIds))) { throw 'Original hardware identity changed.' }
    if (!$hardwareTarget.disabled) { Disable-PnpDevice -InstanceId $hardwareTarget.instanceId -Confirm:$false -ErrorAction Stop }
    $hardwareConfirmed = Read-HardwareTarget $hardwareTarget.instanceId
    if (!$hardwareConfirmed.disabled) { throw 'Hardware disabling was not confirmed.' }
    $hardwareTargets = @($hardwareConfirmed)
}
ConvertTo-Json -InputObject $hardwareTargets -Depth 5 -Compress
