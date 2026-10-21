$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot '../../services/phone-bridge/native/phone-runtime/NativeOwnerLease.cs'
$imports = "using System;`nusing System.IO;`nusing System.Collections.Generic;`n"
Add-Type -TypeDefinition ($imports + [IO.File]::ReadAllText($source))
$root = $env:IVY_PHONE_LEASE_ROOT
$epoch = $env:IVY_PHONE_LEASE_EPOCH
$attempt = 0
while ($true) {
    $ok = $false
    try { [Ivy.PhoneBridge.NativeOwnerLease]::Acquire($root, $epoch); $ok = $true } catch { }
    $second = $null
    if ($ok -and $env:IVY_PHONE_LEASE_SECOND_ROOT) {
        try { [Ivy.PhoneBridge.NativeOwnerLease]::Acquire($env:IVY_PHONE_LEASE_SECOND_ROOT, $epoch); $second = $true } catch { $second = $false }
    }
    $result = [ordered]@{ ok = $ok; second = $second }
    [IO.File]::WriteAllText((Join-Path $env:IVY_PHONE_LEASE_RESULT_ROOT "result-$attempt.json"), (ConvertTo-Json -InputObject $result -Compress))
    if (-not $env:IVY_PHONE_LEASE_HOLD) { break }
    if ([Console]::OpenStandardInput().ReadByte() -ne 114) { break }
    $attempt++
}
