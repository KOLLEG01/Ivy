#Requires -Version 7.0
# All PnP commands are shadowed by functions. No installed device is queried or changed.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$hardwareFixtureState = [pscustomobject]@{ Disabled = $false; Effects = 0; Confirm = $true }
function Get-PnpDevice {
    param([string]$InstanceId, $ErrorAction)
    $name = switch ($InstanceId) { 'USB\CABLE' { 'VB-Audio CABLE' }; 'PCI\PROTECTED' { 'Protected physical audio' }; 'PCI\GPUAUDIO' { 'Synthetic Audiogerät' }; default { 'Endpoint' } }
    [pscustomobject]@{ InstanceId = $InstanceId; Class = 'MEDIA'; FriendlyName = $name }
}
function Get-PnpDeviceProperty {
    param([string]$InstanceId, [string]$KeyName, $ErrorAction)
    $data = switch ($KeyName) {
        'DEVPKEY_Device_Parent' { switch ($InstanceId) { 'SWD\MMDEVAPI\capture' { 'USB\CABLE' }; 'SWD\MMDEVAPI\speaker' { 'PCI\PROTECTED' }; 'HTREE\ROOT\0' { '' }; default { 'HTREE\ROOT\0' } } }
        'DEVPKEY_Device_HardwareIds' { @('PCI\VEN_FIXTURE') }
        'DEVPKEY_Device_ProblemCode' { if ($hardwareFixtureState.Disabled -and $InstanceId -eq 'PCI\GPUAUDIO') { 22 } else { 0 } }
        default { throw 'Unexpected PnP property.' }
    }
    [pscustomobject]@{ Data = $data }
}
function Disable-PnpDevice {
    param([string]$InstanceId, [bool]$Confirm, $ErrorAction)
    if ($InstanceId -cne 'PCI\GPUAUDIO') { throw 'Fixture attempted a protected device.' }
    $hardwareFixtureState.Effects++; if ($hardwareFixtureState.Confirm) { $hardwareFixtureState.Disabled = $true }
}
foreach ($name in @('Get-PnpDevice','Get-PnpDeviceProperty','Disable-PnpDevice')) {
    if ((Get-Command $name).CommandType -ne 'Function') { throw 'PnP fixture isolation failed.' }
}
$backend = Join-Path $PSScriptRoot '../tools/operations/phone-audio-hardware.ps1'
function Invoke-HardwareFixture($Request) {
    [Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
    $previous = [Console]::In
    try { [Console]::SetIn([IO.StringReader]::new(($Request | ConvertTo-Json -Depth 6 -Compress))); & $backend }
    finally { [Console]::SetIn($previous) }
}
function Assert-HardwareRefused($Request) {
    $refused = $false; try { Invoke-HardwareFixture $Request | Out-Null } catch { $refused = $true }
    if (!$refused) { throw 'Expected protected hardware refusal.' }
}
$request = @{ action = 'inspect'; instanceIds = @('PCI\GPUAUDIO'); protectedEndpointIds = @('capture','speaker') }
$targets = @(Invoke-HardwareFixture $request | ConvertFrom-Json)
if ($targets.Count -ne 1 -or $targets[0].disabled) { throw 'Unexpected inspection.' }
foreach ($id in @('USB\CABLE','PCI\PROTECTED')) {
    Assert-HardwareRefused @{ action = 'inspect'; instanceIds = @($id); protectedEndpointIds = @('capture','speaker') }
}
$request.action = 'disable'; $request.expected = $targets[0]
$request.expected.name = 'changed'; Assert-HardwareRefused $request
if ($hardwareFixtureState.Effects -ne 0) { throw 'Refused plans changed hardware.' }
$request.expected.name = 'Synthetic Audiogerät'
$result = @(Invoke-HardwareFixture $request | ConvertFrom-Json)
if (!$result[0].disabled -or $hardwareFixtureState.Effects -ne 1) { throw 'Expected one confirmed synthetic mutation.' }
$hardwareFixtureState.Disabled = $false; $hardwareFixtureState.Confirm = $false
Assert-HardwareRefused $request
'phone_audio_hardware_backend_passed: protected ancestry, virtual devices, original identity and confirmed disable; all PnP calls simulated'
