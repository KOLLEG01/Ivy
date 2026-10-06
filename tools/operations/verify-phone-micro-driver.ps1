#Requires -Version 7.0
# Read-only setup preflight. Never executes the installer, installs/loads a driver,
# imports a certificate, changes signing policy, or restarts a device/application.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$InstallerPath,
    [Parameter(Mandatory=$true)][string]$DriverDirectory,
    [Parameter(Mandatory=$true)][string]$SignToolPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'Windows signature verification is required.' }
$microExpectedInstaller = '81f426741f7ee2ed991febe24a22daca8400b6ae2f171054e3fb404897e15d39'
$microExpectedDrivers = [ordered]@{
    'usbip2_filter.cat' = 'bb2183d204b64eaa52c6eac86c8cfb859ad0a5a5001af5e6b197bf04cc49ac2e'
    'usbip2_filter.inf' = 'a1f07bbf5cfa7734a52046534dd66ee2333d34aa53912356179a6eaf25d1d60e'
    'usbip2_filter.sys' = '671110e3fe09628d3d7f416b5ee35172ecf5046f90be6e8735631105d38fdec5'
    'usbip2_ude.cat' = '99c6807a2d05fd01a689389be81d5088bb4777831adb3c7e119b14a1f5b088c7'
    'usbip2_ude.inf' = 'd975ac1ae14246611824518ceb05fa45227176d8e366366220f58a7ab977c379'
    'usbip2_ude.sys' = 'd4d98db62d78a5b6d32eec3f8f1f9f6b0d2eda925a56c96367c5b7985ab52cd8'
}
$microLocks = [Collections.Generic.List[IO.FileStream]]::new()
function Lock-MicroFile([string]$Path) {
    if (![IO.Path]::IsPathFullyQualified($Path) -or ![IO.File]::Exists($Path)) { throw 'An explicit absolute existing file is required.' }
    $microFile = [IO.FileStream]::new($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $microLocks.Add($microFile)
    $microSha = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($microSha.ComputeHash($microFile))).Replace('-','').ToLowerInvariant() }
    finally { $microSha.Dispose() }
}
try {
    if ((Lock-MicroFile $InstallerPath) -cne $microExpectedInstaller) { throw 'Installer differs from the reviewed signed release.' }
    $microSignature = Get-AuthenticodeSignature -LiteralPath $InstallerPath
    if ($microSignature.Status -ne 'Valid' -or $microSignature.SignerCertificate.Thumbprint -cne '9AC56B6C76141395D74FFF6652818376E80B9C95') {
        throw 'Reviewed installer signer is not valid on this host.'
    }
    if (![IO.Path]::IsPathFullyQualified($DriverDirectory) -or ![IO.Directory]::Exists($DriverDirectory)) { throw 'Explicit extracted driver directory required.' }
    if ([IO.Path]::GetFileName($SignToolPath) -ine 'signtool.exe') { throw 'Explicit Windows SDK signtool.exe required.' }
    $null = Lock-MicroFile $SignToolPath
    $microToolSignature = Get-AuthenticodeSignature -LiteralPath $SignToolPath
    if ($microToolSignature.Status -ne 'Valid' -or $microToolSignature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation(?:,|$)') {
        throw 'SignTool must have a valid Microsoft signature.'
    }
    $microFiles = [ordered]@{}
    foreach ($microName in $microExpectedDrivers.Keys) {
        $microPath = Join-Path $DriverDirectory $microName
        $microHash = Lock-MicroFile $microPath
        if ($microHash -cne $microExpectedDrivers[$microName]) { throw "Driver package file differs from the reviewed release: $microName" }
        $microFiles[$microName] = 'sha256:' + $microHash
        if ($microName.EndsWith('.cat')) {
            $microCatalogSignature = Get-AuthenticodeSignature -LiteralPath $microPath
            if ($microCatalogSignature.Status -ne 'Valid' -or $microCatalogSignature.SignerCertificate.Subject -notmatch 'CN=Microsoft Windows Hardware Compatibility Publisher(?:,|$)') {
                throw "Catalog does not have a valid Microsoft hardware publisher signature: $microName"
            }
        }
    }
    foreach ($microStem in @('usbip2_filter', 'usbip2_ude')) {
        foreach ($microExtension in @('.sys', '.inf')) {
            $microArguments = @('verify', '/kp', '/c', (Join-Path $DriverDirectory ($microStem + '.cat')), (Join-Path $DriverDirectory ($microStem + $microExtension)))
            $microVerification = & $SignToolPath @microArguments 2>&1
            if ($LASTEXITCODE -ne 0) { throw "Kernel policy/catalog membership verification failed: $microStem$microExtension`n$microVerification" }
        }
    }
    [ordered]@{
        release = 'v.0.9.8.0'; installerHash = 'sha256:' + $microExpectedInstaller
        driverFiles = $microFiles; kernelCatalogChecks = 4; installed = $false
    } | ConvertTo-Json -Depth 5
} finally {
    foreach ($microLock in $microLocks) { $microLock.Dispose() }
}
