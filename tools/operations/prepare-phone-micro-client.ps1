#Requires -Version 7.0
# Emit VoiceInputSettings for the reviewed installed client; no process/device actions.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$ClientDirectory,
    [ValidateRange(1024,65535)][int]$Port = 3241
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (![IO.Path]::IsPathFullyQualified($ClientDirectory) -or ![IO.Directory]::Exists($ClientDirectory)) { throw 'Explicit absolute client directory required.' }
$microPinsPath = Join-Path $PSScriptRoot '../../services/phone-bridge/native/phone-runtime/micro-usbip-client.json'
$microPins = Get-Content -LiteralPath $microPinsPath -Raw | ConvertFrom-Json -AsHashtable
if ($microPins.Count -ne 3 -or @('usbip.exe','libusbip.dll','resources.dll').Where({ !$microPins.ContainsKey($_) }).Count -ne 0) {
    throw 'Exact reviewed client manifest required.'
}
$microLeases = [Collections.Generic.List[IO.FileStream]]::new()
try {
    foreach ($microName in $microPins.Keys) {
        if ($microPins[$microName] -cnotmatch '^[0-9a-f]{64}$') { throw 'Invalid reviewed client hash.' }
        $microLease = [IO.FileStream]::new((Join-Path $ClientDirectory $microName), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        $microLeases.Add($microLease)
        $microHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($microLease)).ToLowerInvariant()
        if ($microHash -cne $microPins[$microName]) { throw "Client differs from the reviewed release: $microName" }
    }
    [ordered]@{
        micro = [ordered]@{
            usbipExecutable = [IO.Path]::GetFullPath((Join-Path $ClientDirectory 'usbip.exe'))
            executableHash = 'sha256:' + $microPins['usbip.exe']; port = $Port
        }
        hotkeyFallback = $null
    } | ConvertTo-Json -Depth 4
} finally {
    foreach ($microLease in $microLeases) { $microLease.Dispose() }
}
