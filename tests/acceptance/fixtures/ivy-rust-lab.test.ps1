$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../../../tools/operations/ivy-rust-lab.ps1')

function Assert-Equal($Actual, $Expected) {
    if ($Actual -cne $Expected) { throw "Expected '$Expected', got '$Actual'" }
}
function Assert-Throws([scriptblock]$Body, [string]$Pattern) {
    try { & $Body } catch {
        if ($_.Exception.Message -notmatch $Pattern) { throw }
        return
    }
    throw "Expected failure: $Pattern"
}

# Enclosing VPN routes and differently aligned prefixes must be detected, not only exact /30 strings.
Assert-Equal (Test-LabOverlap '192.168.84.0/30' '192.168.0.0/16') $true
Assert-Equal (Test-LabOverlap '192.168.84.0/30' '192.168.84.2/32') $true
Assert-Equal (Test-LabOverlap '192.168.84.0/30' '192.168.84.4/30') $false
Assert-Equal (Test-LabOverlap '10.1.2.0/30' '10.0.0.0/8') $true
Assert-Throws { Get-LabPath 'C:\lab\..\outside' } 'absolute'
Assert-Throws { Get-LabPath 'C:\lab\file:stream' } 'absolute'
Assert-Throws { Get-LabPath '\\server\share\lab' } 'absolute'

$plan = [pscustomobject]@{subnet='192.168.84.0/30'}
$snapshot = [pscustomobject]@{freeMemoryKiB=8388608; freeDiskBytes=120GB; prefixes=@('0.0.0.0/0'); listeners=@()}
Assert-LabCapacity $snapshot $plan
$snapshot.freeMemoryKiB = 8388607
Assert-Throws { Assert-LabCapacity $snapshot $plan } 'RAM'
$snapshot.freeMemoryKiB = 8388608
$snapshot.prefixes = @('0.0.0.0/0','192.168.0.0/16')
Assert-Throws { Assert-LabCapacity $snapshot $plan } 'overlaps'
$snapshot.prefixes = @()
$snapshot.listeners = @(39181)
Assert-Throws { Assert-LabCapacity $snapshot $plan } 'TCP'

$header = '{"kind":"header","identity":"fixture"}' + "`n"
$pending = '{"kind":"pending","key":"account","operationId":"original","action":"create"}' + "`n"
$complete = '{"kind":"created","key":"account","operationId":"original","value":{"sid":"test"}}' + "`n"
Assert-Throws { Read-LabJournal ($header + $pending) fixture } 'Uncertain'
Assert-Throws { Read-LabJournal ($header + $pending + $complete.TrimEnd("`n")) fixture } 'Incomplete'
Assert-Throws { Read-LabJournal ($header + $complete) fixture } 'without matching'
Assert-Throws { Read-LabJournal ($header + $pending + $complete.Replace('original','new')) fixture } 'without matching'
Assert-Throws { Read-LabJournal $header wrong } 'mismatch'
Assert-Equal (Read-LabJournal ($header + $pending + $complete) fixture).account.value.sid test

# Real flushed journal; fixture effects only. Prove no replay on success, collision, drift or lost result.
$temp = Join-Path ([IO.Path]::GetTempPath()) ('ivy-rust-ledger-' + [guid]::NewGuid())
[void][IO.Directory]::CreateDirectory($temp)
$path = Join-Path $temp 'ledger.jsonl'
$stream = [IO.FileStream]::new($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
try {
    Write-LabEvent $stream @{kind='header';identity='fixture'}
    $state = @{}
    $fake = @{ current=$null; effects=0 }
    $read = { $fake.current }
    $create = { $fake.effects++; $fake.current=[pscustomobject]@{id='original'} }
    Invoke-LabStep $stream $state account $read $create
    Invoke-LabStep $stream $state account $read $create
    Assert-Equal $fake.effects 1
    $fake.current = [pscustomobject]@{id='replacement'}
    Assert-Throws { Invoke-LabStep $stream $state account $read $create } 'changed'
    Assert-Throws { Invoke-LabStep $stream $state unowned $read $create } 'collision'
    Assert-Equal $fake.effects 1
    $fake.current = $null
    Assert-Throws { Invoke-LabStep $stream $state uncertain $read { $fake.effects++; throw 'lost reply' } } 'lost reply'
    Assert-Throws { Invoke-LabStep $stream $state uncertain $read $create } 'Uncertain'
    Assert-Equal $fake.effects 2
} finally { $stream.Dispose() }
try {
    Assert-Throws { Read-LabJournal ([IO.File]::ReadAllText($path)) fixture } 'Uncertain'
} finally {
    # Exact fixture files and empty directory only; never recurse through a computed path.
    [IO.File]::Delete($path)
    [IO.Directory]::Delete($temp)
}
Write-Output 'Lab guard and durable-ledger checks passed.'
