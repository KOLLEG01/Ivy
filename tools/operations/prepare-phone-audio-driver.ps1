#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$PackagePath,
    [Parameter(Mandatory=$true)][string]$OutputDirectory,
    [switch]$Download,
    [switch]$Install
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# Same pinned manufacturer package as IvySIP. This setup command is separate from service startup.
$audioPackageHash = 'b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb'
$audioPackageUrl = 'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip'
foreach ($audioPath in @($PackagePath, $OutputDirectory)) {
    if (![IO.Path]::IsPathFullyQualified($audioPath)) { throw 'Explicit absolute package and output paths required.' }
}
$PackagePath = [IO.Path]::GetFullPath($PackagePath)
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if ([IO.File]::Exists($OutputDirectory) -or [IO.Directory]::Exists($OutputDirectory)) { throw 'Driver preparation requires a fresh output directory.' }
if (![IO.File]::Exists($PackagePath)) {
    if (!$Download) { throw 'Pinned package is missing; use -Download to fetch it from the manufacturer.' }
    $audioPackageParent = [IO.Path]::GetDirectoryName($PackagePath)
    if (![IO.Directory]::Exists($audioPackageParent)) { throw 'Package parent directory must already exist.' }
    # A failed download stays at its unique partial path; never overwrite an existing package.
    $audioPartial = $PackagePath + '.' + [guid]::NewGuid().ToString() + '.partial'
    $audioHandler = [Net.Http.HttpClientHandler]::new(); $audioHandler.AllowAutoRedirect = $false
    $audioHttp = [Net.Http.HttpClient]::new($audioHandler); $audioHttp.Timeout = [TimeSpan]::FromSeconds(120)
    $audioDownloadStop = [Threading.CancellationTokenSource]::new(120000)
    try {
        $audioResponse = $audioHttp.GetAsync($audioPackageUrl, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $audioDownloadStop.Token).GetAwaiter().GetResult()
        try {
            $audioResponse.EnsureSuccessStatusCode() | Out-Null
            $audioInput = $audioResponse.Content.ReadAsStream()
            $audioOutput = [IO.FileStream]::new($audioPartial, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try {
                $audioBuffer = [byte[]]::new(65536); [long]$audioBytes = 0
                while (($audioCount = $audioInput.ReadAsync($audioBuffer, 0, $audioBuffer.Length, $audioDownloadStop.Token).GetAwaiter().GetResult()) -gt 0) {
                    $audioBytes += $audioCount
                    if ($audioBytes -gt 33554432) { throw 'Driver package exceeds its download bound.' }
                    $audioOutput.Write($audioBuffer, 0, $audioCount)
                }
            } finally { $audioInput.Dispose(); $audioOutput.Dispose() }
        } finally { $audioResponse.Dispose() }
        if ((Get-FileHash -LiteralPath $audioPartial -Algorithm SHA256).Hash.ToLowerInvariant() -cne $audioPackageHash) { throw 'Downloaded driver package differs from the pinned release.' }
        [IO.File]::Move($audioPartial, $PackagePath, $false)
    } finally { $audioDownloadStop.Dispose(); $audioHttp.Dispose(); $audioHandler.Dispose() }
}
$audioLease = [IO.FileStream]::new($PackagePath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
$audioFileLeases = [Collections.Generic.List[IO.FileStream]]::new()
try {
    if ($audioLease.Length -gt 33554432 -or [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($audioLease)).ToLowerInvariant() -cne $audioPackageHash) {
        throw 'Driver package differs from the pinned release.'
    }
    $audioLease.Position = 0
    $audioArchive = [IO.Compression.ZipArchive]::new($audioLease, [IO.Compression.ZipArchiveMode]::Read, $true)
    $audioExpectedFiles = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
    try {
        if ($audioArchive.Entries.Count -gt 256) { throw 'Driver archive entry limit exceeded.' }
        [long]$audioExpandedBytes = 0
        $audioPrefix = $OutputDirectory.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
        foreach ($audioEntry in $audioArchive.Entries) {
            $audioExpandedBytes += $audioEntry.Length
            $audioTarget = [IO.Path]::GetFullPath([IO.Path]::Combine($OutputDirectory, $audioEntry.FullName))
            if ($audioExpandedBytes -gt 134217728 -or !$audioTarget.StartsWith($audioPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Driver archive leaves its bounded output directory.' }
            if ($audioEntry.Name.Length -gt 0) {
                $audioEntryStream = $audioEntry.Open()
                try { $audioExpectedFiles.Add($audioTarget, [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($audioEntryStream))) }
                finally { $audioEntryStream.Dispose() }
            }
        }
        [IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
        [IO.Compression.ZipFileExtensions]::ExtractToDirectory($audioArchive, $OutputDirectory)
    } finally { $audioArchive.Dispose() }
    $audioInstaller = Join-Path $OutputDirectory 'VBCABLE_Setup_x64.exe'
    if (![IO.File]::Exists($audioInstaller)) { throw 'Pinned x64 installer is missing.' }
    # Keep package contents immutable while verifying and optionally executing the vendor UI.
    foreach ($audioFilePath in $audioExpectedFiles.Keys) {
        $audioFileLease = [IO.FileStream]::new($audioFilePath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        $audioFileLeases.Add($audioFileLease)
        if ([Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($audioFileLease)) -cne $audioExpectedFiles[$audioFilePath]) { throw 'Extracted driver content changed before verification.' }
    }
    if (@(Get-ChildItem -LiteralPath $OutputDirectory -Recurse -File).Count -ne $audioExpectedFiles.Count) { throw 'Unexpected files in prepared driver directory.' }
    $audioSignature = Get-AuthenticodeSignature -LiteralPath $audioInstaller
    $audioState = 'prepared'; $audioExitCode = $null
    if ($Install) {
        if ($audioSignature.Status -ne 'Valid') { throw 'Installer Authenticode trust must be valid before execution.' }
        # -Install explicitly requests the interactive manufacturer installer and its UAC dialog.
        $audioProcess = Start-Process -FilePath $audioInstaller -WorkingDirectory $OutputDirectory -Verb RunAs -WindowStyle Normal -PassThru -Wait
        $audioExitCode = $audioProcess.ExitCode
        if ($audioExitCode -notin @(0,3010)) { throw "Original driver installer failed with exit code $audioExitCode; inspect its result before another attempt." }
        $audioState = 'installer_completed_restart_required'
    }
    [ordered]@{ schemaVersion = 1; state = $audioState; packageHash = 'sha256:' + $audioPackageHash;
        directory = $OutputDirectory; installerPath = $audioInstaller; signatureStatus = $audioSignature.Status.ToString();
        installerExitCode = $audioExitCode; restartRequired = [bool]$Install } | ConvertTo-Json -Depth 3
} finally {
    foreach ($audioFileLease in $audioFileLeases) { $audioFileLease.Dispose() }
    $audioLease.Dispose()
}
