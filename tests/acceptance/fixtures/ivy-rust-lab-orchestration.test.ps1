# Run the real entrypoint with real files/journal/hash, replacing privileged host boundaries only.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../../../tools/operations/ivy-rust-lab.ps1')
function Check($Value, [string]$Message) { if (-not $Value) { throw $Message } }
function Reject([scriptblock]$Body, [string]$Pattern) {
    try { & $Body | Out-Null } catch { if ($_.Exception.Message -notmatch $Pattern) { throw }; return }
    throw "Expected rejection: $Pattern"
}
function Effect([string]$EffectName, [scriptblock]$Body) {
    if ($script:fake.fail -eq "before:$EffectName") { throw "Injected before:$EffectName" }
    $script:fake.effects.Add($EffectName)
    & $Body
    if ($script:fake.fail -eq "after:$EffectName") { throw "Injected after:$EffectName" }
}
function Assert-LabAdministrator {}
function Protect-LabJournal($Stream) {}
function Read-LabPlan($Path) { return $script:fake.plan }
function Get-LabSnapshot($Plan) { return [pscustomobject]@{freeMemoryKiB=12582912;freeDiskBytes=1TB;prefixes=@();listeners=@()} }
function Get-ItemProperty($LiteralPath) { return [pscustomobject]@{MachineGuid='fixture-machine'} }
function New-LabDirectory($Path) { Effect directory { [void][IO.Directory]::CreateDirectory($Path) } }
function Get-LocalUser { if ($script:fake.account) { $script:fake.account } }
function New-LocalUser($Name,$Password,[switch]$Disabled,$Description) {
    Effect account { $script:fake.account=[pscustomobject]@{Name=$Name;SID=[pscustomobject]@{Value='S-1-5-21-123-456-789-1001'};Enabled=(-not $Disabled);Description=$Description} }
}
function Get-VMSwitch($Id) { if ($script:fake.switch) { $script:fake.switch } }
function New-VMSwitch($Name,$SwitchType,$Notes) {
    Effect switch { $script:fake.switch=[pscustomobject]@{Id=[guid]::NewGuid();Name=$Name;SwitchType=$SwitchType;Notes=$Notes} }
}
function New-VHD($Path,$SizeBytes,[switch]$Dynamic) {
    Effect disk {
        [IO.File]::WriteAllBytes($Path, [byte[]](1,2,3,4,5,6,7,8))
        $script:fake.disk=[pscustomobject]@{DiskIdentifier=[guid]::NewGuid();Path=$Path;Size=$SizeBytes;VhdType='Dynamic';Attached=$false}
    }
}
function Get-VHD($Path) { return $script:fake.disk }
function Get-VM($Id) { if ($script:fake.vm) { $script:fake.vm } }
function New-VM($Name,$Generation,$MemoryStartupBytes,$Path,$VHDPath,$SwitchName) {
    Effect vm {
        $script:fake.vm=[pscustomobject]@{Id=[guid]::NewGuid();Name=$Name;Generation=$Generation;Path=$Path;State='Off';
            AutomaticStartAction='StartIfRunning';AutomaticStopAction='Save';CheckpointType='Production';AutomaticCheckpointsEnabled=$true;
            SnapshotFileLocation=$Path;SmartPagingFilePath=$Path;DynamicMemoryEnabled=$false;MemoryStartup=$MemoryStartupBytes;ProcessorCount=1}
        $script:fake.firmware=[pscustomobject]@{SecureBoot='Off';SecureBootTemplate='';PreferredNetworkBootProtocol='IPv4';
            BootOrder=@([pscustomobject]@{BootType='Drive';FirmwarePath='fixture-disk';Description='Disk'})}
        $script:fake.drives=@([pscustomobject]@{VMId=$script:fake.vm.Id;Path=$VHDPath;DiskNumber=$null;ControllerType='SCSI';ControllerNumber=0;ControllerLocation=0})
        $script:fake.adapters=@([pscustomobject]@{Id='fixture-nic';VMId=$script:fake.vm.Id;Name='Network Adapter';SwitchId=$script:fake.switch.Id;
            Connected=$true;MacAddress='001122334455';DynamicMacAddressEnabled=$true;MacAddressSpoofing='Off';DhcpGuard='Off';RouterGuard='Off';PortMirroringMode='None'})
    }
}
function Set-VM($VM,$AutomaticStartAction,$AutomaticStopAction,$CheckpointType,$AutomaticCheckpointsEnabled,$SnapshotFileLocation,$SmartPagingFilePath) {
    Effect configuration {
        $VM.AutomaticStartAction=$AutomaticStartAction; $VM.AutomaticStopAction=$AutomaticStopAction
        $VM.CheckpointType=$CheckpointType; $VM.AutomaticCheckpointsEnabled=$AutomaticCheckpointsEnabled
        $VM.SnapshotFileLocation=$SnapshotFileLocation; $VM.SmartPagingFilePath=$SmartPagingFilePath
    }
}
function Set-VMMemory($VM,$DynamicMemoryEnabled,$StartupBytes) { Effect memory { $VM.DynamicMemoryEnabled=$DynamicMemoryEnabled; $VM.MemoryStartup=$StartupBytes } }
function Set-VMProcessor($VM,$Count) { Effect cpu { $VM.ProcessorCount=$Count } }
function Set-VMFirmware($VM,$EnableSecureBoot,$SecureBootTemplate) { Effect firmware { $script:fake.firmware.SecureBoot=$EnableSecureBoot; $script:fake.firmware.SecureBootTemplate=$SecureBootTemplate } }
function Get-VMFirmware($VM) { return $script:fake.firmware }
function Get-VMSnapshot($VM) { return $script:fake.checkpoints }
function Get-VMDvdDrive($VM) { return $script:fake.dvds }
function Get-VMNetworkAdapterVlan($VMNetworkAdapter) { return $script:fake.vlan }
function Get-VMNetworkAdapter {
    param([Parameter(ValueFromPipeline=$true)]$VM)
    process { if ($VM) { $script:fake.adapters } }
}
function Get-VMHardDiskDrive {
    param([Parameter(ValueFromPipeline=$true)]$VM)
    process { if ($VM) { $script:fake.drives } }
}
function Get-CimInstance($ClassName) { if ($ClassName -ne 'Win32_UserProfile') { throw "Unexpected CIM read: $ClassName" } }
function Remove-VM {
    param([Parameter(ValueFromPipeline=$true)]$VM,[switch]$Force)
    process { Effect removeVm {
        $script:fake.vm=$null; $script:fake.drives=@(); $script:fake.adapters=@()
        if ($script:fake.onRemoveVm) { & $script:fake.onRemoveVm }
    } }
}
function Remove-VMSwitch {
    param([Parameter(ValueFromPipeline=$true)]$Switch,[switch]$Force)
    process { Effect removeSwitch { $script:fake.switch=$null } }
}
function Remove-LocalUser($SID) { Effect removeAccount { $script:fake.account=$null } }
$originalWrite = (Get-Command Write-LabEvent).ScriptBlock
function Write-LabEvent($Stream,$Event) {
    if ($Event.kind -eq 'created' -and $Event.key -eq 'configuration' -and $script:fake.fail -eq 'before:baseline-result') { throw 'Injected before:baseline-result' }
    & $originalWrite $Stream $Event
    if ($Event.kind -eq 'created' -and $Event.key -eq 'configuration' -and $script:fake.fail -eq 'after:baseline-result') { throw 'Injected after:baseline-result' }
}

function Fixture([string]$Name,[scriptblock]$Body) {
    $temp = Join-Path ([IO.Path]::GetTempPath()) ('ivy-rust-orchestration-' + [guid]::NewGuid())
    [void][IO.Directory]::CreateDirectory($temp)
    $script:fake=@{
        plan=[pscustomobject]@{schemaVersion=1;hostName='fixture';labId=[guid]::NewGuid().ToString();root=(Join-Path $temp 'reserved\lab');account='FixtureUser';vm='FixtureVm';switch='FixtureSwitch';subnet='192.168.84.0/30'}
        effects=[Collections.Generic.List[string]]::new(); fail=''; account=$null; switch=$null; disk=$null; vm=$null
        firmware=$null; drives=@(); adapters=@(); checkpoints=@(); dvds=@(); onRemoveVm=$null
        vlan=[pscustomobject]@{OperationMode='Untagged';AccessVlanId=0;NativeVlanId=0;AllowedVlanIdListString='';PrivateVlanMode='None';PrimaryVlanId=0;SecondaryVlanId=0;SecondaryVlanIdListString=''}
    }
    $ledger=Join-Path $temp 'ledger.jsonl'
    $disk=Join-Path $script:fake.plan.root 'vm\disk.vhdx'
    try { & $Body; Write-Output "Passed: $Name" }
    finally {
        # Delete exact test files and empty directories only. No host cmdlets or recursive cleanup.
        if ([IO.File]::Exists($disk)) { [IO.File]::Delete($disk) }
        if ([IO.File]::Exists($ledger)) { [IO.File]::Delete($ledger) }
        foreach ($dir in @((Join-Path $script:fake.plan.root 'vm'),$script:fake.plan.root,(Join-Path $temp 'reserved'),$temp)) {
            if ([IO.Directory]::Exists($dir)) { [IO.Directory]::Delete($dir) }
        }
    }
}
function Run([string]$Action) { Invoke-IvyRustLab $Action 'fixture-plan' $ledger }
function Provision { Check ((Run Provision).state -eq 'reserved-inert') 'Provision did not finish' }

Fixture 'initial and repeated Provision; initial and repeated Rollback' {
    Provision
    $count=$script:fake.effects.Count
    Provision
    Check ($count -eq $script:fake.effects.Count) 'Provision replay mutated resources'
    Check ((Run Rollback).state -eq 'rolled-back') 'Rollback did not finish'
    Check (-not [IO.File]::Exists($disk)) 'Disk remained after rollback'
    $count=$script:fake.effects.Count
    Check ((Run Rollback).state -eq 'rolled-back') 'Repeated rollback did not finish'
    Check ($count -eq $script:fake.effects.Count) 'Rollback replay mutated resources'
    Reject { Run Provision } 'Changed/removed'
}
Fixture 'same ID and same length but changed VHD content' {
    Provision; $count=$script:fake.effects.Count; $id=$script:fake.disk.DiskIdentifier
    [IO.File]::WriteAllBytes($disk,[byte[]](8,7,6,5,4,3,2,1))
    Reject { Run Provision } 'Disk content changed'
    Reject { Run Rollback } 'Disk content changed'
    Check ($id -eq $script:fake.disk.DiskIdentifier -and $count -eq $script:fake.effects.Count) 'Changed disk triggered mutations'
    Check ([IO.File]::Exists($disk)) 'Changed disk was removed'
}
Fixture 'grown VHD rejected before hashing/removal' {
    Provision; $count=$script:fake.effects.Count
    [IO.File]::AppendAllText($disk,'changed')
    Reject { Run Rollback } 'Disk content length changed'
    Check ($count -eq $script:fake.effects.Count) 'Grown disk triggered mutation'
}
Fixture 'disk changes during VM removal; retain disk and remaining resources' {
    Provision
    $script:fake.onRemoveVm={ [IO.File]::WriteAllBytes($disk,[byte[]](8,7,6,5,4,3,2,1)) }
    Reject { Run Rollback } 'Disk content changed'
    $count=$script:fake.effects.Count
    Reject { Run Rollback } 'Disk content changed'
    Check ($count -eq $script:fake.effects.Count -and [IO.File]::Exists($disk)) 'Changed disk was removed on resume'
    Check ($null -ne $script:fake.switch -and $null -ne $script:fake.account) 'Later resources removed after changed disk'
}
Fixture 'partial setup without final baseline cannot be adopted or removed' {
    Provision
    $lines=@([IO.File]::ReadAllLines($ledger))
    $boundary=0
    while (($lines[$boundary] | ConvertFrom-Json).PSObject.Properties.Name -notcontains 'key' -or
        ($lines[$boundary] | ConvertFrom-Json).key -ne 'configuration') { $boundary++ }
    [IO.File]::WriteAllText($ledger,($lines[0..($boundary-1)] -join "`n") + "`n")
    $count=$script:fake.effects.Count
    Reject { Run Provision } 'baseline is missing'
    Reject { Run Rollback } 'baseline is missing'
    Check ($count -eq $script:fake.effects.Count -and [IO.File]::Exists($disk)) 'Partial setup was adopted or removed'
}
$drifts=[ordered]@{
    checkpoint={ $script:fake.vm.CheckpointType='Production' }
    automaticCheckpoint={ $script:fake.vm.AutomaticCheckpointsEnabled=$true }
    snapshot={ $script:fake.checkpoints=@('checkpoint') }
    snapshotPath={ $script:fake.vm.SnapshotFileLocation='C:\changed' }
    pagingPath={ $script:fake.vm.SmartPagingFilePath='C:\changed' }
    secureBoot={ $script:fake.firmware.SecureBoot='Off' }
    secureBootTemplate={ $script:fake.firmware.SecureBootTemplate='Other' }
    bootOrder={ $script:fake.firmware.BootOrder[0].FirmwarePath='changed' }
    switch={ $script:fake.adapters[0].SwitchId=[guid]::NewGuid() }
    extraNic={ $script:fake.adapters+=($script:fake.adapters[0].PSObject.Copy()) }
    vlan={ $script:fake.vlan.AccessVlanId=42 }
    mac={ $script:fake.adapters[0].MacAddress='001122334499' }
    firmwareNetwork={ $script:fake.firmware.PreferredNetworkBootProtocol='IPv6' }
    automaticStart={ $script:fake.vm.AutomaticStartAction='Start' }
    automaticStop={ $script:fake.vm.AutomaticStopAction='Save' }
    memory={ $script:fake.vm.MemoryStartup=8GB }
    cpu={ $script:fake.vm.ProcessorCount=8 }
    extraDisk={ $script:fake.drives+=($script:fake.drives[0].PSObject.Copy()) }
    dvd={ $script:fake.dvds=@([pscustomobject]@{Path='fixture.iso';ControllerType='SCSI';ControllerNumber=0;ControllerLocation=1}) }
}
foreach($key in $drifts.Keys) {
    Fixture "VM configuration drift: $key" {
        Provision; $count=$script:fake.effects.Count
        & $drifts[$key]
        Reject { Run Provision } 'configuration changed|checkpoints'
        Reject { Run Rollback } 'configuration changed|checkpoints'
        Check ($count -eq $script:fake.effects.Count -and [IO.File]::Exists($disk)) 'Drift triggered destructive mutation'
    }
}
foreach($fault in 'before:vm','after:vm','before:firmware','after:firmware','before:baseline-result') {
    Fixture "Uncertain provisioning: $fault" {
        $script:fake.fail=$fault
        Reject { Run Provision } 'Injected'
        $script:fake.fail=''; $count=$script:fake.effects.Count
        Reject { Run Provision } 'Uncertain'
        Reject { Run Rollback } 'Uncertain'
        Check ($count -eq $script:fake.effects.Count) 'Uncertain operation replayed'
    }
}
Fixture 'baseline completion durable but caller loses response' {
    $script:fake.fail='after:baseline-result'; Reject { Run Provision } 'Injected'
    $script:fake.fail=''; $count=$script:fake.effects.Count
    Provision
    Check ($count -eq $script:fake.effects.Count) 'Durable completion replayed'
}
foreach($fault in 'before:removeVm','after:removeVm','after:removeSwitch') {
    Fixture "Uncertain rollback: $fault" {
        Provision; $script:fake.fail=$fault
        Reject { Run Rollback } 'Injected'
        $script:fake.fail=''; $count=$script:fake.effects.Count
        Reject { Run Rollback } 'Uncertain'
        Check ($count -eq $script:fake.effects.Count) 'Uncertain removal replayed'
    }
}
