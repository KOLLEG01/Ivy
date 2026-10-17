# Stage one only: reserve an inert lab. No OS image, boot, IP configuration, or services.
[CmdletBinding()]
param(
    [ValidateSet('Inspect', 'Provision', 'Rollback')][string]$Mode = 'Inspect',
    [string]$PlanPath,
    [string]$LedgerPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-LabFileHash([string]$Path) {
    # Some managed Windows PowerShell environments do not expose Get-FileHash.
    $sha = [Security.Cryptography.SHA256]::Create()
    $file = [IO.File]::OpenRead($Path)
    try { return [BitConverter]::ToString($sha.ComputeHash($file)).Replace('-','').ToLowerInvariant() }
    finally { $file.Dispose(); $sha.Dispose() }
}

function Get-LabPath([string]$Path) {
    if ($Path -notmatch '^[A-Za-z]:[\\/]' -or $Path -match '[*?\[\]]|(^|[\\/])\.\.?([\\/]|$)|:.*:') {
        throw "Expected an absolute, literal local path: $Path"
    }
    $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    for ($cursor = $full; $cursor; $cursor = [IO.Path]::GetDirectoryName($cursor)) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse point: $cursor" }
        }
    }
    return $full
}

function Get-LabRange([string]$Prefix) {
    if ($Prefix -notmatch '^(\d+\.\d+\.\d+\.\d+)/(\d{1,2})$') { throw "Invalid IPv4 prefix: $Prefix" }
    $ip = [Net.IPAddress]::Parse($Matches[1]).GetAddressBytes()
    $bits = [int]$Matches[2]
    if ($ip.Length -ne 4 -or $bits -gt 32) { throw "Invalid IPv4 prefix: $Prefix" }
    $number = [uint64]$ip[0] * 16777216 + [uint64]$ip[1] * 65536 + [uint64]$ip[2] * 256 + $ip[3]
    $size = [uint64][Math]::Pow(2, 32 - $bits)
    $start = $number - ($number % $size)
    return @($start, ($start + $size - 1))
}

function Test-LabOverlap([string]$Left, [string]$Right) {
    $a = Get-LabRange $Left
    $b = Get-LabRange $Right
    return ($a[0] -le $b[1] -and $b[0] -le $a[1])
}

function Read-LabPlan([string]$Path) {
    $p = Get-Content -LiteralPath (Get-LabPath $Path) -Raw | ConvertFrom-Json
    $keys = @('schemaVersion','hostName','labId','root','account','vm','switch','subnet')
    if (@($p.PSObject.Properties.Name | Where-Object { $_ -notin $keys }).Count -or
        @($keys | Where-Object { $_ -notin $p.PSObject.Properties.Name }).Count) { throw 'Unexpected plan fields' }
    if ($p.schemaVersion -ne 1 -or $p.hostName -ne $env:COMPUTERNAME) { throw 'Plan version/host mismatch' }
    [void][guid]::Parse($p.labId)
    foreach ($name in @($p.account, $p.vm, $p.switch)) {
        if ($name -notmatch '^[A-Za-z][A-Za-z0-9-]{1,63}$') { throw 'Invalid resource name' }
    }
    if ($p.account.Length -gt 20) { throw 'Local account name exceeds 20 characters' }
    $p.root = Get-LabPath $p.root
    # Require two named levels below a drive; neither the drive nor a general user home is a lab root.
    if ($p.root -notmatch '^[A-Za-z]:\\[^\\]+\\[^\\]+$') { throw 'Lab root must have two named path segments' }
    if ($p.subnet -notmatch '/30$' -or -not (
        (Test-LabOverlap $p.subnet '10.0.0.0/8') -or
        (Test-LabOverlap $p.subnet '172.16.0.0/12') -or
        (Test-LabOverlap $p.subnet '192.168.0.0/16'))) { throw 'Expected a private IPv4 /30' }
    $range = Get-LabRange $p.subnet
    if ((Get-LabRange ($p.subnet.Split('/')[0] + '/32'))[0] -ne $range[0]) { throw 'Subnet is not network-aligned' }
    return $p
}

function Assert-LabCapacity($Snapshot, $Plan) {
    # Fixed 4 GiB guest plus at least 4 GiB currently free host headroom. No reclaim or peer stops.
    if ([uint64]$Snapshot.freeMemoryKiB -lt 8388608) { throw 'Insufficient RAM: require 8 GiB free before provisioning' }
    if ([uint64]$Snapshot.freeDiskBytes -lt 120GB) { throw 'Insufficient disk: require 120 GiB free' }
    foreach ($prefix in $Snapshot.prefixes) {
        if ($prefix -ne '0.0.0.0/0' -and (Test-LabOverlap $Plan.subnet $prefix)) {
            throw "Subnet overlaps an existing route/address: $prefix"
        }
    }
    if (@($Snapshot.listeners).Count) { throw 'A reserved lab TCP port is already listening' }
}

function Get-LabSnapshot($Plan) {
    $os = Get-CimInstance Win32_OperatingSystem
    $drive = Get-Volume -DriveLetter $Plan.root.Substring(0,1)
    $prefixes = @((Get-NetRoute -AddressFamily IPv4).DestinationPrefix)
    $prefixes += @(Get-NetIPAddress -AddressFamily IPv4 | ForEach-Object { "$($_.IPAddress)/$($_.PrefixLength)" })
    return [pscustomobject]@{
        observedAt = [DateTime]::UtcNow.ToString('o')
        freeMemoryKiB = $os.FreePhysicalMemory
        freeDiskBytes = $drive.SizeRemaining
        prefixes = $prefixes
        listeners = @(Get-NetTCPConnection -State Listen | Where-Object LocalPort -in 39181,39182,39281,39282,39381,39382 |
            Select-Object LocalAddress,LocalPort,OwningProcess)
    }
}

function Read-LabJournal([string]$Text, [string]$Identity) {
    if (-not $Text.EndsWith("`n")) { throw 'Incomplete ledger; reconcile original operation before retrying' }
    $lines = @($Text.TrimEnd("`n").Split("`n") | ForEach-Object { $_ | ConvertFrom-Json })
    if ($lines[0].kind -ne 'header' -or $lines[0].identity -ne $Identity) { throw 'Ledger plan/host/script mismatch' }
    $state = @{}
    for ($i = 1; $i -lt $lines.Count; $i++) {
        $event = $lines[$i]
        if ($event.kind -eq 'pending') {
            if ($state.ContainsKey($event.key) -and $state[$event.key].kind -eq 'pending') { throw 'Duplicate pending operation' }
        } elseif ($event.kind -in 'created','removed') {
            if (-not $state.ContainsKey($event.key) -or $state[$event.key].kind -ne 'pending' -or
                $state[$event.key].operationId -ne $event.operationId) { throw 'Ledger completion without matching intent' }
        } else { throw 'Unknown ledger event' }
        $state[$event.key] = $event
    }
    if (@($state.Values | Where-Object kind -eq 'pending').Count) {
        throw 'Uncertain operation in ledger; inspect its original intent and reconcile before any mutation'
    }
    return $state
}

function Write-LabEvent($Stream, $Event) {
    $Event.at = [DateTime]::UtcNow.ToString('o')
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Event | ConvertTo-Json -Depth 8 -Compress) + "`n")
    [void]$Stream.Seek(0, [IO.SeekOrigin]::End)
    $Stream.Write($bytes, 0, $bytes.Length)
    $Stream.Flush($true)
}

function Invoke-LabStep($Stream, $State, [string]$Key, [scriptblock]$Read, [scriptblock]$Create) {
    $current = & $Read
    if ($State.ContainsKey($Key)) {
        if ($State[$Key].kind -eq 'pending') { throw "Uncertain operation: $Key" }
        if ($State[$Key].kind -ne 'created') { throw "Resource was rolled back: $Key" }
        if ($null -eq $current -or ($current | ConvertTo-Json -Compress) -cne ($State[$Key].value | ConvertTo-Json -Compress)) {
            throw "Recorded resource changed: $Key"
        }
        return
    }
    if ($null -ne $current) { throw "Unowned resource collision: $Key" }
    $operationId = [guid]::NewGuid().ToString()
    $intent = @{ kind='pending'; key=$Key; operationId=$operationId; action='create' }
    Write-LabEvent $Stream $intent
    $State[$Key] = [pscustomobject]$intent
    & $Create | Out-Null
    $value = & $Read
    if ($null -eq $value) { throw "Creation has no observable result: $Key" }
    $event = @{ kind='created'; key=$Key; operationId=$operationId; value=$value }
    Write-LabEvent $Stream $event
    $State[$Key] = [pscustomobject]$event
}

function Get-LabDirectory([string]$Path) {
    [void](Get-LabPath $Path)
    if (Test-Path -LiteralPath $Path) {
        $item = Get-Item -LiteralPath $Path -Force
        if (-not $item.PSIsContainer) { throw "Not a directory: $Path" }
        return [pscustomobject]@{ path=$item.FullName; createdAt=$item.CreationTimeUtc.ToString('o') }
    }
}

function New-LabDirectory([string]$Path) {
    $item = New-Item -ItemType Directory -Path $Path
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in 'S-1-5-18','S-1-5-32-544') {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
            [Security.Principal.SecurityIdentifier]::new($sid), 'FullControl',
            'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    }
    $item.SetAccessControl($acl)
}

function Get-LabAccount($Plan) {
    $user = @(Get-LocalUser | Where-Object Name -eq $Plan.account)
    if ($user.Count) {
        return [pscustomobject]@{ sid=$user[0].SID.Value; name=$user[0].Name; enabled=$user[0].Enabled; description=$user[0].Description }
    }
}

function Get-LabSwitch($Plan) {
    $switches = @(Get-VMSwitch | Where-Object Name -eq $Plan.switch)
    if ($switches.Count -gt 1) { throw 'Ambiguous switch name' }
    if ($switches.Count) {
        return [pscustomobject]@{ id=$switches[0].Id.ToString(); name=$switches[0].Name; type=$switches[0].SwitchType.ToString(); notes=$switches[0].Notes }
    }
}

function Get-LabDisk([string]$Path) {
    [void](Get-LabPath $Path)
    if (Test-Path -LiteralPath $Path) {
        $disk = Get-VHD -Path $Path
        if ($disk.Attached) { throw 'Lab disk is attached; rollback/provisioning requires an inert disk' }
        return [pscustomobject]@{ id=$disk.DiskIdentifier.ToString(); path=$disk.Path; size=$disk.Size; type=$disk.VhdType.ToString() }
    }
}

function Get-LabVm($Plan) {
    $vms = @(Get-VM | Where-Object Name -eq $Plan.vm)
    if ($vms.Count -gt 1) { throw 'Ambiguous VM name' }
    if ($vms.Count) {
        $vm = $vms[0]
        if ($vm.State.ToString() -ne 'Off') { throw 'Lab VM must be Off; no automatic stop is permitted' }
        return [pscustomobject]@{ id=$vm.Id.ToString(); name=$vm.Name; path=$vm.Path; generation=$vm.Generation }
    }
}

function Get-LabDiskContent([string]$Path, $ExpectedLength = $null) {
    [void](Get-LabPath $Path)
    # Hash the initially small, unused dynamic VHD only; reject growth before reading its bytes.
    $file = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        if ($file.Length -gt 64MB -or ($null -ne $ExpectedLength -and $file.Length -ne $ExpectedLength)) {
            throw 'Disk content length changed or exceeds the inert 64 MiB baseline limit'
        }
        return [pscustomobject]@{ length=$file.Length; sha256=[BitConverter]::ToString($sha.ComputeHash($file)).Replace('-','').ToLowerInvariant() }
    } finally { $sha.Dispose(); $file.Dispose() }
}

function Get-LabVmConfiguration($Plan, $VmId) {
    $vm = Get-VM -Id ([guid]$VmId)
    if ($vm.State.ToString() -ne 'Off') { throw 'Lab VM must be Off' }
    if (@(Get-VMSnapshot -VM $vm).Count) { throw 'VM has checkpoints; preserve for manual review' }
    $firmware = Get-VMFirmware -VM $vm
    $adapters = @(Get-VMNetworkAdapter -VM $vm | Sort-Object Id | ForEach-Object {
        $vlan = Get-VMNetworkAdapterVlan -VMNetworkAdapter $_
        [pscustomobject]@{
            id=$_.Id.ToString(); name=$_.Name; switchId=$_.SwitchId.ToString(); connected=$_.Connected
            mac=$_.MacAddress; dynamicMac=$_.DynamicMacAddressEnabled
            spoofing=$_.MacAddressSpoofing.ToString(); dhcpGuard=$_.DhcpGuard.ToString()
            routerGuard=$_.RouterGuard.ToString(); mirroring=$_.PortMirroringMode.ToString()
            vlan=($vlan | Select-Object OperationMode,AccessVlanId,NativeVlanId,AllowedVlanIdListString,PrivateVlanMode,PrimaryVlanId,SecondaryVlanId,SecondaryVlanIdListString)
        }
    })
    return [pscustomobject]@{
        automaticStart=$vm.AutomaticStartAction.ToString(); automaticStop=$vm.AutomaticStopAction.ToString()
        checkpointType=$vm.CheckpointType.ToString(); automaticCheckpoints=$vm.AutomaticCheckpointsEnabled
        snapshotPath=$vm.SnapshotFileLocation; pagingPath=$vm.SmartPagingFilePath
        dynamicMemory=$vm.DynamicMemoryEnabled; memory=$vm.MemoryStartup; processors=$vm.ProcessorCount
        secureBoot=$firmware.SecureBoot.ToString(); secureBootTemplate=$firmware.SecureBootTemplate
        networkBoot=$firmware.PreferredNetworkBootProtocol.ToString()
        bootOrder=@($firmware.BootOrder | Select-Object BootType,FirmwarePath,Description)
        disks=@(Get-VMHardDiskDrive -VM $vm | Sort-Object ControllerNumber,ControllerLocation |
            Select-Object Path,DiskNumber,ControllerType,ControllerNumber,ControllerLocation)
        dvds=@(Get-VMDvdDrive -VM $vm | Sort-Object ControllerNumber,ControllerLocation |
            Select-Object Path,ControllerType,ControllerNumber,ControllerLocation)
        adapters=$adapters
    }
}

function Assert-LabDesiredConfiguration($Config, [string]$VmDirectory, [string]$DiskPath, [string]$SwitchId) {
    if ($Config.automaticStart -ne 'Nothing' -or $Config.automaticStop -ne 'ShutDown' -or
        $Config.checkpointType -ne 'Disabled' -or $Config.automaticCheckpoints -or
        (Get-LabPath $Config.snapshotPath) -ne $VmDirectory -or (Get-LabPath $Config.pagingPath) -ne $VmDirectory -or
        $Config.dynamicMemory -or $Config.memory -ne 4GB -or $Config.processors -ne 4 -or
        $Config.secureBoot -ne 'On' -or $Config.secureBootTemplate -ne 'MicrosoftUEFICertificateAuthority' -or
        $Config.disks.Count -ne 1 -or $Config.disks[0].Path -ne $DiskPath -or
        @($Config.dvds | Where-Object Path).Count -or $Config.adapters.Count -ne 1 -or
        $Config.adapters[0].switchId -ne $SwitchId -or -not $Config.adapters[0].connected) {
        throw 'VM configuration does not match inert setup'
    }
}

function Assert-LabBaseline($Plan, $State, [string]$DiskPath) {
    $hasVm = $State.ContainsKey('vm') -and $State.vm.kind -eq 'created'
    $hasDisk = $State.ContainsKey('disk') -and $State.disk.kind -eq 'created'
    if (-not $hasVm -and -not $hasDisk) { return }
    if (-not $State.ContainsKey('configuration') -or $State.configuration.kind -ne 'created') {
        throw 'Final inert baseline is missing; reconcile partial setup before removal'
    }
    $baseline = $State.configuration.value
    if ($hasDisk) {
        $content = Get-LabDiskContent $DiskPath $baseline.disk.length
        if ($content.sha256 -cne $baseline.disk.sha256) { throw 'Disk content changed from final inert baseline' }
    }
    if ($hasVm) {
        $config = Get-LabVmConfiguration $Plan $State.vm.value.id
        if (($config | ConvertTo-Json -Depth 12 -Compress) -cne ($baseline.vm | ConvertTo-Json -Depth 12 -Compress)) {
            throw 'VM configuration changed from final inert baseline'
        }
    }
}

function Assert-LabAdministrator {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run the reviewed command from an elevated PowerShell; this script never elevates itself' }
}

function Protect-LabJournal($Stream) {
    $acl = [Security.AccessControl.FileSecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in 'S-1-5-18','S-1-5-32-544') {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
            [Security.Principal.SecurityIdentifier]::new($sid), 'FullControl', 'Allow'))
    }
    $Stream.SetAccessControl($acl)
}

function Invoke-IvyRustLab([string]$Action, [string]$InputPath, [string]$JournalPath) {
    if ($PSVersionTable.PSEdition -ne 'Desktop') { throw 'Use Windows PowerShell 5.1 for this host provisioning script' }
    $plan = Read-LabPlan $InputPath
    $parent = [IO.Path]::GetDirectoryName($plan.root)
    $vmDirectory = Join-Path $plan.root 'vm'
    $diskPath = Join-Path $vmDirectory 'disk.vhdx'
    $snapshot = Get-LabSnapshot $plan
    if ($Action -eq 'Inspect') {
        $blocker = $null
        try { Assert-LabCapacity $snapshot $plan } catch { $blocker = $_.Exception.Message }
        return [pscustomobject]@{ snapshot=$snapshot; capacityBlocker=$blocker; account=(Get-LabAccount $plan);
            vm=(Get-LabVm $plan); switch=(Get-LabSwitch $plan); root=(Get-LabDirectory $plan.root) }
    }
    Assert-LabAdministrator
    $JournalPath = Get-LabPath $JournalPath
    if ($JournalPath.StartsWith($parent + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Ledger must be outside the lab tree' }
    if (-not (Test-Path -LiteralPath ([IO.Path]::GetDirectoryName($JournalPath)) -PathType Container)) { throw 'Ledger parent must already exist' }
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $machine = (Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Cryptography').MachineGuid
        $scriptHash = Get-LabFileHash $PSCommandPath
        $identity = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes(
            ($plan | ConvertTo-Json -Compress) + $machine + $scriptHash))).Replace('-','').ToLowerInvariant()
    } finally { $sha.Dispose() }
    if ($Action -eq 'Provision') { Assert-LabCapacity $snapshot $plan }
    $exists = Test-Path -LiteralPath $JournalPath
    if ($Action -eq 'Rollback' -and -not $exists) { throw 'Rollback requires the original ledger' }
    # Keep the exclusive file handle for the whole operation. Never replace or truncate evidence.
    $mode = if ($exists) { [IO.FileMode]::Open } else { [IO.FileMode]::CreateNew }
    $stream = [IO.FileStream]::new($JournalPath, $mode, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        if (-not $exists) {
            Protect-LabJournal $stream
            Write-LabEvent $stream @{ kind='header'; identity=$identity; plan=$plan; machine=$machine; scriptHash=$scriptHash; snapshot=$snapshot }
        }
        [void]$stream.Seek(0, [IO.SeekOrigin]::Begin)
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $true, 4096, $true)
        try { $state = Read-LabJournal $reader.ReadToEnd() $identity } finally { $reader.Dispose() }
        $readers = [ordered]@{
            parent = { Get-LabDirectory $parent }
            root = { Get-LabDirectory $plan.root }
            vmDirectory = { Get-LabDirectory $vmDirectory }
            account = { Get-LabAccount $plan }
            switch = { Get-LabSwitch $plan }
            disk = { Get-LabDisk $diskPath }
            vm = { Get-LabVm $plan }
        }
        if ($Action -eq 'Provision') {
            if ($state.ContainsKey('disk') -or $state.ContainsKey('vm')) { Assert-LabBaseline $plan $state $diskPath }
            # Check every collision before creating any resource. Existing parent directories are not adopted.
            foreach ($key in $readers.Keys) {
                $current = & $readers[$key]
                if ($key -eq 'parent' -and -not $state.ContainsKey($key)) { continue }
                if ($state.ContainsKey($key)) {
                    if ($state[$key].kind -ne 'created' -or $null -eq $current -or
                        ($current | ConvertTo-Json -Compress) -cne ($state[$key].value | ConvertTo-Json -Compress)) { throw "Changed/removed resource: $key" }
                } elseif ($null -ne $current) { throw "Unowned resource collision: $key" }
            }
            if (-not (Test-Path -LiteralPath $parent) -or $state.ContainsKey('parent')) {
                Invoke-LabStep $stream $state parent $readers.parent { New-LabDirectory $parent }
            }
            Invoke-LabStep $stream $state root $readers.root { New-LabDirectory $plan.root }
            Invoke-LabStep $stream $state vmDirectory $readers.vmDirectory { New-LabDirectory $vmDirectory }
            Invoke-LabStep $stream $state account $readers.account {
                $random = New-Object byte[] 32
                $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
                try { $rng.GetBytes($random) } finally { $rng.Dispose() }
                $password = [Security.SecureString]::new()
                foreach ($character in ('aA1!' + [Convert]::ToBase64String($random)).ToCharArray()) { $password.AppendChar($character) }
                $password.MakeReadOnly()
                try { New-LocalUser -Name $plan.account -Password $password -Disabled -Description $plan.labId }
                finally { $password.Dispose(); [Array]::Clear($random,0,$random.Length) }
            }
            Invoke-LabStep $stream $state switch $readers.switch { New-VMSwitch -Name $plan.switch -SwitchType Internal -Notes $plan.labId }
            Invoke-LabStep $stream $state disk $readers.disk { New-VHD -Path $diskPath -SizeBytes 100GB -Dynamic }
            Invoke-LabStep $stream $state vm $readers.vm {
                New-VM -Name $plan.vm -Generation 2 -MemoryStartupBytes 4GB -Path $vmDirectory -VHDPath $diskPath -SwitchName $plan.switch
            }
            # Separate intent covers configuration; a crash between setters requires original-ledger reconciliation.
            if (-not $state.ContainsKey('configuration')) {
                $operation = [guid]::NewGuid().ToString()
                Write-LabEvent $stream @{kind='pending';key='configuration';operationId=$operation;action='configure-inert-vm'}
                $vm = Get-VM -Id ([guid]$state.vm.value.id)
                Set-VM -VM $vm -AutomaticStartAction Nothing -AutomaticStopAction ShutDown -CheckpointType Disabled -AutomaticCheckpointsEnabled $false -SnapshotFileLocation $vmDirectory -SmartPagingFilePath $vmDirectory
                Set-VMMemory -VM $vm -DynamicMemoryEnabled $false -StartupBytes 4GB
                Set-VMProcessor -VM $vm -Count 4
                Set-VMFirmware -VM $vm -EnableSecureBoot On -SecureBootTemplate MicrosoftUEFICertificateAuthority
                $config = Get-LabVmConfiguration $plan $state.vm.value.id
                Assert-LabDesiredConfiguration $config $vmDirectory $diskPath $state.switch.value.id
                $event = @{kind='created';key='configuration';operationId=$operation;value=[pscustomobject]@{
                    vm=$config; disk=(Get-LabDiskContent $diskPath)
                }}
                Write-LabEvent $stream $event
                $state.configuration = [pscustomobject]$event
            }
            Assert-LabBaseline $plan $state $diskPath
            return [pscustomobject]@{ state='reserved-inert'; ledger=$JournalPath; labId=$plan.labId; memoryGiB=4; booted=$false }
        }
        # Rollback is only for this inert preparation stage. Never stop a VM or remove changed identities.
        Assert-LabBaseline $plan $state $diskPath
        foreach ($key in $readers.Keys) {
            if ($state.ContainsKey($key) -and $state[$key].kind -eq 'created') {
                $current = & $readers[$key]
                if ($null -eq $current -or ($current | ConvertTo-Json -Compress) -cne ($state[$key].value | ConvertTo-Json -Compress)) {
                    throw "Rollback identity mismatch: $key"
                }
            }
        }
        if ($state.ContainsKey('vm') -and $state.vm.kind -eq 'created') {
            $vm = Get-VM -Id ([guid]$state.vm.value.id)
            if (@(Get-VMSnapshot -VM $vm).Count) { throw 'VM has checkpoints; preserve for manual review' }
            $disks = @(Get-VMHardDiskDrive -VM $vm)
            if ($disks.Count -ne 1 -or $disks[0].Path -ne $diskPath) { throw 'VM disk membership changed' }
        }
        if ($state.ContainsKey('switch') -and $state.switch.kind -eq 'created') {
            $users = @(Get-VM | Get-VMNetworkAdapter | Where-Object SwitchId -eq ([guid]$state.switch.value.id))
            foreach ($user in $users) {
                if (-not $state.ContainsKey('vm') -or $state.vm.kind -ne 'created' -or $user.VMId.ToString() -ne $state.vm.value.id) { throw 'Switch has an unrelated consumer' }
            }
        }
        if ($state.ContainsKey('disk') -and $state.disk.kind -eq 'created') {
            foreach ($drive in @(Get-VM | Get-VMHardDiskDrive | Where-Object Path -eq $diskPath)) {
                if (-not $state.ContainsKey('vm') -or $state.vm.kind -ne 'created' -or $drive.VMId.ToString() -ne $state.vm.value.id) { throw 'Disk has an unrelated consumer' }
            }
        }
        if ($state.ContainsKey('account') -and $state.account.kind -eq 'created') {
            if (@(Get-CimInstance Win32_UserProfile | Where-Object SID -eq $state.account.value.sid).Count) {
                throw 'Account has a profile; stage-one rollback cannot remove a used account'
            }
        }
        $removers = [ordered]@{
            vm = { Get-VM -Id ([guid]$state.vm.value.id) | Remove-VM -Force }
            disk = { [void](Get-LabPath $diskPath); Remove-Item -LiteralPath $diskPath }
            switch = { Get-VMSwitch -Id ([guid]$state.switch.value.id) | Remove-VMSwitch -Force }
            account = { Remove-LocalUser -SID ([Security.Principal.SecurityIdentifier]::new($state.account.value.sid)) }
        }
        foreach ($key in $removers.Keys) {
            if (-not $state.ContainsKey($key) -or $state[$key].kind -eq 'removed') { continue }
            Assert-LabBaseline $plan $state $diskPath
            $current = & $readers[$key]
            if ($null -eq $current -or ($current | ConvertTo-Json -Compress) -cne ($state[$key].value | ConvertTo-Json -Compress)) { throw "Resource changed before removal: $key" }
            $operation = [guid]::NewGuid().ToString()
            Write-LabEvent $stream @{kind='pending';key=$key;operationId=$operation;action='remove';value=$state[$key].value}
            & $removers[$key] | Out-Null
            if ($null -ne (& $readers[$key])) { throw "Resource remains after removal: $key" }
            $event = @{kind='removed';key=$key;operationId=$operation;value=$state[$key].value}
            Write-LabEvent $stream $event
            $state[$key] = [pscustomobject]$event
        }
        # Retain directories and Hyper-V residual files for evidence. No recursive deletion or root reuse.
        return [pscustomobject]@{ state='rolled-back'; ledger=$JournalPath; retainedDirectories=@($parent,$plan.root,$vmDirectory) }
    } finally { $stream.Dispose() }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-IvyRustLab $Mode $PlanPath $LedgerPath | ConvertTo-Json -Depth 8
}
