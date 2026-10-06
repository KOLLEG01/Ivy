#Requires -Version 7.0
# Explicit local package verification entrypoint; never passes -Download or -Install.
param([Parameter(Mandatory=$true)][string]$PackagePath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$audioTestRoot = Join-Path ([IO.Path]::GetTempPath()) ('ivy-audio-setup-test-' + [guid]::NewGuid().ToString())
[IO.Directory]::CreateDirectory($audioTestRoot) | Out-Null
$audioSetupScript = Join-Path $PSScriptRoot '../tools/operations/prepare-phone-audio-driver.ps1'
function Assert-AudioRejected([scriptblock]$Action) {
    $audioRejected = $false
    try { & $Action | Out-Null } catch { $audioRejected = $true }
    if (!$audioRejected) { throw 'Expected setup refusal.' }
}
try {
    $audioBadPackage = Join-Path $audioTestRoot 'wrong.zip'
    [IO.File]::WriteAllText($audioBadPackage, 'synthetic invalid package')
    $audioBadOutput = Join-Path $audioTestRoot 'rejected'
    Assert-AudioRejected { & $audioSetupScript -PackagePath $audioBadPackage -OutputDirectory $audioBadOutput }
    if (Test-Path -LiteralPath $audioBadOutput) { throw 'Invalid package created an extraction directory.' }
    Assert-AudioRejected { & $audioSetupScript -PackagePath 'relative.zip' -OutputDirectory $audioBadOutput }
    Assert-AudioRejected { & $audioSetupScript -PackagePath (Join-Path $audioTestRoot 'missing.zip') -OutputDirectory $audioBadOutput }
    $audioPrepared = Join-Path $audioTestRoot 'prepared'
    $audioReport = (& $audioSetupScript -PackagePath $PackagePath -OutputDirectory $audioPrepared) | ConvertFrom-Json
    if ($audioReport.state -ne 'prepared' -or $audioReport.installerExitCode -ne $null -or $audioReport.restartRequired -or
        $audioReport.packageHash -cne 'sha256:b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb') { throw 'Unexpected driver preparation result.' }
    Assert-AudioRejected { & $audioSetupScript -PackagePath $PackagePath -OutputDirectory $audioPrepared }
    $audioReleased = [IO.FileStream]::new($audioReport.installerPath, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $audioReleased.Dispose()
    'phone_audio_driver_preparation_passed: pinned actual archive, invalid hash/path, no overwrite, released file leases; no install/download'
} finally {
    $audioResolved = (Resolve-Path -LiteralPath $audioTestRoot).Path
    $audioExpected = [IO.Path]::GetFullPath($audioTestRoot)
    if ($audioResolved -ne $audioExpected -or !([IO.Path]::GetFileName($audioResolved)).StartsWith('ivy-audio-setup-test-')) { throw 'Unexpected fixture cleanup target.' }
    Remove-Item -LiteralPath $audioResolved -Recurse -Force
}
