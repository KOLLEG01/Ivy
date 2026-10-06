[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('list', 'identify', 'open')]
    [string] $Command,

    [int] $TabId = 0,
    [string] $Container = '',
    [string] $Url = '',
    [string] $BraveExecutable = '',
    [string] $UserDataDirectory = '',
    [string] $ProfileDirectory = 'Default',

    [ValidateRange(1, 50)]
    [int] $Attempts = 12,

    [ValidateRange(2, 80)]
    [int] $ObservationAttempts = 40
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Throw-BraveContainerError {
    param([string] $Code, [string] $Message)

    $errorValue = [System.InvalidOperationException]::new($Message)
    $errorValue.Data['Code'] = $Code
    throw $errorValue
}

function Get-PropertyValue {
    param([object] $Value, [string] $Name)

    if ($null -eq $Value) { return $null }
    $property = $Value.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Resolve-BraveExecutable {
    param([string] $ConfiguredPath)

    if (-not [string]::IsNullOrWhiteSpace($ConfiguredPath)) {
        $resolved = [System.IO.Path]::GetFullPath($ConfiguredPath)
        if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
            Throw-BraveContainerError 'brave_executable_not_found' "Brave executable was not found: $resolved"
        }
        return $resolved
    }

    $roots = @(
        $env:ProgramFiles,
        ${env:ProgramFiles(x86)},
        $env:LOCALAPPDATA
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    $channels = @('Brave-Browser', 'Brave-Browser-Beta', 'Brave-Browser-Nightly')
    foreach ($root in $roots) {
        foreach ($channel in $channels) {
            $candidate = Join-Path $root "BraveSoftware\$channel\Application\brave.exe"
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                return [System.IO.Path]::GetFullPath($candidate)
            }
        }
    }
    Throw-BraveContainerError 'brave_executable_not_found' 'Brave executable was not found. Pass -BraveExecutable explicitly.'
}

function Resolve-BraveProfile {
    param(
        [string] $ExecutableValue,
        [string] $UserDataValue,
        [string] $ProfileValue,
        [bool] $NeedExecutable
    )

    if ([string]::IsNullOrWhiteSpace($ProfileValue) -or $ProfileValue.Length -gt 128 -or
        $ProfileValue -match '[\x00-\x1f\x7f]' -or $ProfileValue -match '[\\/]' -or
        $ProfileValue -in @('.', '..') -or [System.IO.Path]::IsPathRooted($ProfileValue)) {
        Throw-BraveContainerError 'brave_profile_invalid' 'ProfileDirectory must name one Brave profile directory.'
    }

    $executable = $null
    if ($NeedExecutable -or [string]::IsNullOrWhiteSpace($UserDataValue)) {
        $executable = Resolve-BraveExecutable $ExecutableValue
    }

    if (-not [string]::IsNullOrWhiteSpace($UserDataValue)) {
        $userData = [System.IO.Path]::GetFullPath($UserDataValue)
    } else {
        $channel = if ($executable -match '(?i)Brave-Browser-Beta') {
            'Brave-Browser-Beta'
        } elseif ($executable -match '(?i)Brave-Browser-Nightly') {
            'Brave-Browser-Nightly'
        } else {
            'Brave-Browser'
        }
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
            Throw-BraveContainerError 'brave_profile_not_found' 'LOCALAPPDATA is unavailable. Pass -UserDataDirectory explicitly.'
        }
        $userData = Join-Path $env:LOCALAPPDATA "BraveSoftware\$channel\User Data"
    }

    if (-not (Test-Path -LiteralPath $userData -PathType Container)) {
        Throw-BraveContainerError 'brave_profile_not_found' "Brave user-data directory was not found: $userData"
    }
    $userData = (Resolve-Path -LiteralPath $userData).Path
    $profilePath = [System.IO.Path]::GetFullPath((Join-Path $userData $ProfileValue))
    $prefix = $userData.TrimEnd([char[]]@('\', '/')) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $profilePath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase) -or
        -not (Test-Path -LiteralPath $profilePath -PathType Container)) {
        Throw-BraveContainerError 'brave_profile_not_found' "Brave profile directory was not found: $profilePath"
    }

    return [pscustomobject]@{
        executablePath = $executable
        userDataDirectory = $userData
        profileDirectory = $ProfileValue
        profilePath = $profilePath
        preferencesPath = Join-Path $profilePath 'Preferences'
    }
}

function Get-CollectionValues {
    param([object] $Value)

    if ($null -eq $Value) { return @() }
    if ($Value -is [System.Array]) { return @($Value) }
    if ($null -ne (Get-PropertyValue $Value 'id') -or $null -ne (Get-PropertyValue $Value 'name')) {
        return @($Value)
    }
    return @($Value.PSObject.Properties | ForEach-Object { $_.Value })
}

function Get-BraveContainers {
    param([string] $PreferencesPath)

    if (-not (Test-Path -LiteralPath $PreferencesPath -PathType Leaf)) {
        Throw-BraveContainerError 'brave_preferences_not_found' "Brave Preferences file was not found: $PreferencesPath"
    }
    try {
        $root = [System.IO.File]::ReadAllText($PreferencesPath, [System.Text.Encoding]::UTF8) | ConvertFrom-Json
    } catch {
        Throw-BraveContainerError 'brave_preferences_invalid' 'Brave Preferences is not valid JSON.'
    }

    $accountValues = Get-PropertyValue $root 'account_values'
    $accountBrave = Get-PropertyValue $accountValues 'brave'
    $localBrave = Get-PropertyValue $root 'brave'
    $stores = @(
        (Get-PropertyValue $accountBrave 'containers'),
        (Get-PropertyValue $localBrave 'containers')
    )
    $records = [System.Collections.Generic.Dictionary[string, object]]::new([System.StringComparer]::Ordinal)
    foreach ($store in $stores) {
        if ($null -eq $store) { continue }
        foreach ($collection in @(
            [pscustomobject]@{ name = 'list'; source = 'configured' },
            [pscustomobject]@{ name = 'used'; source = 'used' }
        )) {
            $items = Get-CollectionValues (Get-PropertyValue $store $collection.name)
            foreach ($item in $items) {
                $id = [string](Get-PropertyValue $item 'id')
                $name = [string](Get-PropertyValue $item 'name')
                $id = $id.Trim()
                $name = $name.Trim()
                if (-not $id -or -not $name) { continue }
                if (-not $records.ContainsKey($id) -or $collection.source -eq 'configured') {
                    $records[$id] = [pscustomobject]@{ id = $id; name = $name; source = $collection.source }
                }
            }
        }
    }
    return @($records.Values | Sort-Object name, id)
}

function Read-SharedFileBytes {
    param([string] $Path)

    $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
    $stream = [System.IO.FileStream]::new($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, $share)
    $memory = [System.IO.MemoryStream]::new()
    try {
        $stream.CopyTo($memory)
        return ,$memory.ToArray()
    } finally {
        $memory.Dispose()
        $stream.Dispose()
    }
}

function Assert-BraveSessionHeader {
    param([byte[]] $Bytes)

    if ($Bytes.Length -lt 8 -or [System.Text.Encoding]::ASCII.GetString($Bytes, 0, 4) -ne 'SNSS') {
        Throw-BraveContainerError 'brave_session_invalid' 'Invalid Brave session signature.'
    }
    $version = [System.BitConverter]::ToInt32($Bytes, 4)
    if ($version -lt 1 -or $version -gt 3) {
        Throw-BraveContainerError 'brave_session_unsupported' "Unsupported Brave session version $version."
    }
}

function Read-SessionTabIds {
    param([byte[]] $Bytes)

    Assert-BraveSessionHeader $Bytes
    $tabIds = [System.Collections.Generic.HashSet[int]]::new()
    $offset = 8
    while ($offset + 3 -le $Bytes.Length) {
        $serializedSize = [System.BitConverter]::ToUInt16($Bytes, $offset)
        if ($serializedSize -lt 1 -or $offset + 2 + $serializedSize -gt $Bytes.Length) { break }
        $commandId = $Bytes[$offset + 2]
        $payloadStart = $offset + 3
        $payloadLength = $serializedSize - 1
        if ($commandId -eq 0 -and $payloadLength -eq 8) {
            [void]$tabIds.Add([System.BitConverter]::ToInt32($Bytes, $payloadStart + 4))
        } elseif ($commandId -eq 6 -and $payloadLength -ge 8) {
            $pickleLength = [System.BitConverter]::ToUInt32($Bytes, $payloadStart)
            if ($pickleLength -le $payloadLength - 4) {
                [void]$tabIds.Add([System.BitConverter]::ToInt32($Bytes, $payloadStart + 4))
            }
        }
        $offset += 2 + $serializedSize
    }
    return @($tabIds)
}

function Read-SessionTab {
    param(
        [byte[]] $Bytes,
        [int] $ExpectedTabId,
        [string] $ExpectedUrl = '',
        [string] $ExpectedContainerId = ''
    )

    Assert-BraveSessionHeader $Bytes

    $found = $false
    $windowId = $null
    $urlMatched = $false
    $containerIds = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    $latin1 = [System.Text.Encoding]::GetEncoding(28591)
    $offset = 8
    while ($offset + 3 -le $Bytes.Length) {
        $serializedSize = [System.BitConverter]::ToUInt16($Bytes, $offset)
        if ($serializedSize -lt 1 -or $offset + 2 + $serializedSize -gt $Bytes.Length) { break }
        $commandId = $Bytes[$offset + 2]
        $payloadStart = $offset + 3
        $payloadLength = $serializedSize - 1
        if ($commandId -eq 0 -and $payloadLength -eq 8) {
            $candidateWindowId = [System.BitConverter]::ToInt32($Bytes, $payloadStart)
            $candidateTabId = [System.BitConverter]::ToInt32($Bytes, $payloadStart + 4)
            if ($candidateTabId -eq $ExpectedTabId) {
                $found = $true
                if ($null -eq $windowId) { $windowId = $candidateWindowId }
            }
        } elseif ($commandId -eq 6 -and $payloadLength -ge 8) {
            $pickleLength = [System.BitConverter]::ToUInt32($Bytes, $payloadStart)
            $candidateTabId = [System.BitConverter]::ToInt32($Bytes, $payloadStart + 4)
            if ($candidateTabId -eq $ExpectedTabId -and $pickleLength -le $payloadLength - 4) {
                $found = $true
                $textLength = $payloadLength - 8
                if ($textLength -gt 0) {
                    $text = $latin1.GetString($Bytes, $payloadStart + 8, $textLength)
                    foreach ($match in [regex]::Matches($text, 'containers\+([A-Za-z0-9-]+):')) {
                        [void]$containerIds.Add($match.Groups[1].Value)
                    }
                    if ($ExpectedUrl -and $ExpectedContainerId) {
                        $marker = 'containers+' + $ExpectedContainerId + ':' + $ExpectedUrl
                        $markerPattern = [regex]::Escape($marker) + '(?![A-Za-z0-9._~:/?#\[\]@!$&''()*+,;=%-])'
                        if ([regex]::IsMatch($text, $markerPattern)) { $urlMatched = $true }
                    }
                }
            }
        }
        $offset += 2 + $serializedSize
    }
    return [pscustomobject]@{
        found = $found
        windowId = $windowId
        containerIds = @($containerIds)
        urlMatched = $urlMatched
    }
}

function Get-BraveSessionFiles {
    param([string] $ProfilePath)

    $sessionsPath = Join-Path $ProfilePath 'Sessions'
    if (-not (Test-Path -LiteralPath $sessionsPath -PathType Container)) { return @() }
    return @(Get-ChildItem -LiteralPath $sessionsPath -File | Where-Object {
        $_.Name.StartsWith('Session_') -or $_.Name.StartsWith('Tabs_')
    } | Sort-Object LastWriteTimeUtc -Descending)
}

function Get-CurrentSessionTabIds {
    param([string] $ProfilePath)

    $tabIds = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($file in @(Get-BraveSessionFiles $ProfilePath)) {
        try {
            foreach ($tabId in @(Read-SessionTabIds (Read-SharedFileBytes $file.FullName))) {
                [void]$tabIds.Add($tabId)
            }
        } catch {
            continue
        }
    }
    return @($tabIds)
}

function Find-TabInCurrentSessions {
    param(
        [string] $ProfilePath,
        [int] $ExpectedTabId,
        [string] $ExpectedUrl = '',
        [string] $ExpectedContainerId = ''
    )

    $found = $false
    $windowId = $null
    $urlMatched = $false
    $containerIds = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($file in @(Get-BraveSessionFiles $ProfilePath)) {
        try {
            $parsed = Read-SessionTab (Read-SharedFileBytes $file.FullName) $ExpectedTabId $ExpectedUrl $ExpectedContainerId
        } catch {
            continue
        }
        if (-not $parsed.found) { continue }
        $found = $true
        if ($null -eq $windowId -and $null -ne $parsed.windowId) { $windowId = $parsed.windowId }
        if ($parsed.urlMatched) { $urlMatched = $true }
        foreach ($id in $parsed.containerIds) { [void]$containerIds.Add($id) }
    }
    if (-not $found) { return $null }
    return [pscustomobject]@{
        tabId = $ExpectedTabId
        windowId = $windowId
        containerIds = @($containerIds)
        urlMatched = $urlMatched
    }
}

function Identify-BraveTabContainer {
    param([object] $Profile, [int] $ExpectedTabId, [int] $MaximumAttempts)

    if ($ExpectedTabId -le 0) {
        Throw-BraveContainerError 'container_tab_invalid' 'identify requires a positive native Brave tab ID.'
    }
    $match = $null
    for ($attempt = 0; $attempt -lt $MaximumAttempts; $attempt += 1) {
        $match = Find-TabInCurrentSessions $Profile.profilePath $ExpectedTabId
        if ($null -ne $match) { break }
        if ($attempt + 1 -lt $MaximumAttempts) { Start-Sleep -Milliseconds 250 }
    }
    if ($null -eq $match) {
        Throw-BraveContainerError 'container_tab_not_found' "Brave tab $ExpectedTabId is not present in the current profile session."
    }
    if ($match.containerIds.Count -gt 1) {
        Throw-BraveContainerError 'container_metadata_ambiguous' "Brave tab $ExpectedTabId has ambiguous container metadata."
    }

    $container = $null
    if ($match.containerIds.Count -eq 1) {
        $containerId = [string]$match.containerIds[0]
        $known = @(Get-BraveContainers $Profile.preferencesPath | Where-Object { $_.id -ceq $containerId })
        $container = if ($known.Count -eq 1) {
            $known[0]
        } else {
            [pscustomobject]@{ id = $containerId; name = $null; source = 'session' }
        }
    }
    return [pscustomobject]@{
        tabId = $ExpectedTabId
        windowId = $match.windowId
        profileDirectory = $Profile.profileDirectory
        container = $container
        context = if ($null -eq $container) { 'default' } else { 'container' }
        evidence = 'brave-session'
    }
}

function Observe-NewBraveTab {
    param(
        [string] $ProfilePath,
        [int[]] $KnownTabIds,
        [string] $ContainerId,
        [string] $ExpectedUrl,
        [int] $MaximumAttempts
    )

    $known = [System.Collections.Generic.HashSet[int]]::new()
    foreach ($tabId in $KnownTabIds) { [void]$known.Add($tabId) }
    $lastCandidateId = 0
    $stableSnapshots = 0
    for ($attempt = 0; $attempt -lt $MaximumAttempts; $attempt += 1) {
        $matches = @()
        foreach ($tabId in @(Get-CurrentSessionTabIds $ProfilePath)) {
            if ($tabId -le 0 -or $known.Contains($tabId)) { continue }
            $tab = Find-TabInCurrentSessions $ProfilePath $tabId $ExpectedUrl $ContainerId
            if ($null -ne $tab -and $tab.containerIds.Count -eq 1 -and
                $tab.containerIds[0] -ceq $ContainerId -and $tab.urlMatched) {
                $matches += $tab
            }
        }
        if ($matches.Count -gt 1) {
            return [pscustomobject]@{ status = 'ambiguous'; tabId = $null; windowId = $null }
        }
        if ($matches.Count -eq 1) {
            if ($matches[0].tabId -eq $lastCandidateId) {
                $stableSnapshots += 1
            } else {
                $lastCandidateId = $matches[0].tabId
                $stableSnapshots = 1
            }
            if ($stableSnapshots -ge 2) {
                return [pscustomobject]@{
                    status = 'observed'
                    tabId = $matches[0].tabId
                    windowId = $matches[0].windowId
                }
            }
        } else {
            $lastCandidateId = 0
            $stableSnapshots = 0
        }
        if ($attempt + 1 -lt $MaximumAttempts) { Start-Sleep -Milliseconds 250 }
    }
    return [pscustomobject]@{ status = 'unconfirmed'; tabId = $null; windowId = $null }
}

function Open-BraveContainerUrl {
    param([object] $Profile, [string] $ContainerName, [string] $UrlValue, [int] $MaximumObservationAttempts)

    if ([string]::IsNullOrWhiteSpace($ContainerName) -or $ContainerName.Length -gt 80 -or
        $ContainerName -match '[\x00-\x1f\x7f]') {
        Throw-BraveContainerError 'container_name_invalid' 'Container must be a nonempty exact Brave container name.'
    }
    $target = $null
    if (-not [System.Uri]::TryCreate($UrlValue, [System.UriKind]::Absolute, [ref]$target) -or
        $target.Scheme -notin @('http', 'https') -or -not [string]::IsNullOrEmpty($target.UserInfo)) {
        Throw-BraveContainerError 'container_url_invalid' 'URL must be an absolute HTTP(S) URL without embedded credentials.'
    }
    $containers = @(Get-BraveContainers $Profile.preferencesPath)
    $matches = @($containers | Where-Object { $_.name -ceq $ContainerName })
    if ($matches.Count -eq 0) {
        Throw-BraveContainerError 'container_not_found' "Brave container '$ContainerName' is not present in profile '$($Profile.profileDirectory)'."
    }
    if ($matches.Count -ne 1) {
        Throw-BraveContainerError 'container_name_ambiguous' "More than one Brave container is named '$ContainerName'."
    }

    $knownTabIds = @(Get-CurrentSessionTabIds $Profile.profilePath)
    $arguments = @(
        "--profile-directory=`"$($Profile.profileDirectory)`"",
        "--container=`"$ContainerName`"",
        "`"$($target.AbsoluteUri)`""
    )
    $process = Start-Process -FilePath $Profile.executablePath -ArgumentList $arguments -PassThru
    if (-not $process.WaitForExit(10000)) {
        Throw-BraveContainerError 'container_open_timeout' 'Brave did not return from the container launch request within 10 seconds.'
    }
    if ($process.ExitCode -ne 0) {
        Throw-BraveContainerError 'container_open_failed' "Brave container launcher exited with code $($process.ExitCode)."
    }
    $observation = Observe-NewBraveTab $Profile.profilePath $knownTabIds $matches[0].id $target.AbsoluteUri $MaximumObservationAttempts
    return [pscustomobject]@{
        launched = $true
        container = $ContainerName
        containerId = $matches[0].id
        url = $target.AbsoluteUri
        profileDirectory = $Profile.profileDirectory
        tabId = $observation.tabId
        windowId = $observation.windowId
        tabObservation = $observation.status
    }
}

try {
    if ($env:OS -ne 'Windows_NT') {
        Throw-BraveContainerError 'platform_unsupported' 'This helper requires Windows and a local Brave profile.'
    }
    $operation = $Command.ToLowerInvariant()
    $profile = Resolve-BraveProfile $BraveExecutable $UserDataDirectory $ProfileDirectory ($operation -eq 'open')
    $result = switch ($operation) {
        'list' {
            [pscustomobject]@{
                profileDirectory = $profile.profileDirectory
                containers = @(Get-BraveContainers $profile.preferencesPath)
            }
        }
        'identify' { Identify-BraveTabContainer $profile $TabId $Attempts }
        'open' { Open-BraveContainerUrl $profile $Container $Url $ObservationAttempts }
    }
    [Console]::Out.WriteLine(($result | ConvertTo-Json -Depth 8 -Compress))
} catch {
    $code = [string]$_.Exception.Data['Code']
    if ([string]::IsNullOrWhiteSpace($code)) { $code = 'brave_container_failed' }
    $failure = [pscustomobject]@{ code = $code; message = $_.Exception.Message }
    [Console]::Error.WriteLine(($failure | ConvertTo-Json -Compress))
    exit 1
}
