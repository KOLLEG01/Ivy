param([Parameter(Mandatory=$true)][string]$PlanPath, [switch]$ReplaceLegacy)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-principal.ps1')
$ivyPlan = Get-Content -LiteralPath $PlanPath -Raw | ConvertFrom-Json
if ($ivyPlan.schemaVersion -ne 1 -or $ivyPlan.os -ne 'win32' -or $ivyPlan.installationId -notmatch '^ivy-next-[0-9a-f]{12}$') { throw 'Invalid IvyNext bootstrap plan.' }
$ivyLauncher = Join-Path $ivyPlan.artifactRoot 'dist\native\ivy-host-job.exe'
if (-not (Test-Path -LiteralPath $ivyLauncher -PathType Leaf)) { throw 'Missing verified IvyNext bootstrap executable.' }
function Quote-IvyArgument([string]$Value) {
  '"' + ([regex]::Replace([regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1')) + '"'
}
$ivyIdentity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$ivySid = $ivyIdentity.User.Value
$ivyScheduler = New-Object -ComObject 'Schedule.Service'
$ivyScheduler.Connect()
$ivyFolder = $ivyScheduler.GetFolder('\')
$ivyInstalled = @()
$ivyDesired = @{}
$ivySpecs = @()
foreach ($ivyProcess in @($ivyPlan.processes)) {
  if ($ivyProcess.name -notmatch ('^' + [regex]::Escape($ivyPlan.installationId) + '-[0-9a-f]{12}$')) { throw 'OS name is outside this IvyNext installation.' }
  $ivyBreakaway = @(); if ($ivyProcess.allowWindowsBreakaway -eq $true) { $ivyBreakaway = @('--allow-breakaway') }
  $ivyArtifact = if ($ivyProcess.artifactRoot) { [string]$ivyProcess.artifactRoot } else { [string]$ivyPlan.artifactRoot }
  $ivyExecutable = if ($ivyProcess.entrypoint -and $ivyProcess.entrypoint.executable -ne 'node') { [string]$ivyProcess.entrypoint.executable } else { [string]$ivyPlan.nodeExecutable }
  $ivyArgs = if ($ivyProcess.entrypoint) { @($ivyProcess.entrypoint.args) } elseif ($ivyProcess.componentId -eq 'host-executor') { @('dist\packages\host-runtime\src\executor.js') } else { @('dist\services\' + $ivyProcess.componentId + '\src\main.js') }
  $ivyConfig = if ($ivyProcess.configPath) { [string]$ivyProcess.configPath } else { [string]$ivyPlan.configPath }
  $ivyConfigArgument = if ($ivyProcess.componentId -eq 'host-executor') { [string]$ivyPlan.configPath } else { $ivyConfig }
  $ivyOwnConfig = if ($ivyProcess.componentId -eq 'host-executor' -and $ivyProcess.configPath) { @('--instance-config', $ivyConfig) } else { @() }
  $ivyArguments = @('--parent', '0') + $ivyBreakaway + @($ivyExecutable) + $ivyArgs + @('--config', $ivyConfigArgument) + $ivyOwnConfig
  $ivyCommandLine = ($ivyArguments | ForEach-Object { Quote-IvyArgument $_ }) -join ' '
  $ivyDescription = 'IvyNext immutable bootstrap ' + $ivyPlan.candidateId + ' host ' + $ivyPlan.hostId
  $ivySpecs += [pscustomobject]@{ Process=$ivyProcess; Name=[string]$ivyProcess.name; Artifact=$ivyArtifact; CommandLine=$ivyCommandLine; Description=$ivyDescription }
  $ivyDesired[[string]$ivyProcess.name] = $true
}

$ivyPattern = '^' + [regex]::Escape($ivyPlan.installationId) + '-[0-9a-f]{12}$'
# The root COM collection can omit tasks registered by another principal on
# some Windows versions. Enumerate names through the scheduler cmdlet, then
# resolve each exact task through the COM API used for mutation.
$ivyExistingNames = @(Get-ScheduledTask -TaskPath '\' -ErrorAction Stop |
  Where-Object { $_.TaskName -match $ivyPattern } |
  Select-Object -ExpandProperty TaskName)
$ivyRemoveNames = @()

# Validate every existing definition before stopping or deleting any task. A
# Guardian task needs the explicit transition switch; a non-Guardian Ivy task
# is safe to replace only when it is already owned by this bootstrap shape.
foreach ($ivyName in $ivyExistingNames) {
  $ivyTask = $ivyFolder.GetTask([string]$ivyName)
  $ivyDefinition = $ivyTask.Definition
  $ivyAction = if ($ivyDefinition.Actions.Count -eq 1) { $ivyDefinition.Actions.Item(1) } else { $null }
  $ivyDescription = [string]$ivyDefinition.RegistrationInfo.Description
  $ivyHasGuardian = $null -ne $ivyAction -and ([string]$ivyAction.Arguments -match 'guardian\.js')
  if ($ivyHasGuardian) {
    if ($ivyDescription -notlike ('IvyNext immutable bootstrap * host ' + $ivyPlan.hostId) -or
        $ivyAction.Path -notlike '*\ivy-host-job.exe') {
      throw ('Existing OS task is not an explicitly recognized legacy Ivy Guardian task: ' + $ivyName)
    }
    if (-not $ReplaceLegacy) { throw ('Legacy Ivy Guardian task requires explicit owner replacement: ' + $ivyName) }
    $ivyRemoveNames += [string]$ivyName
    continue
  }
  $ivyManaged = $ivyDefinition.Actions.Count -eq 1 -and
    $ivyDescription -like ('IvyNext immutable bootstrap * host ' + $ivyPlan.hostId) -and
    $ivyAction.Path -like '*\ivy-host-job.exe' -and
    (Resolve-IvyPrincipalSid $ivyDefinition.Principal.UserId) -eq $ivySid -and
    $ivyDefinition.Principal.LogonType -eq 3 -and $ivyDefinition.Principal.RunLevel -eq 0
  if (-not $ivyManaged) { throw ('Existing OS task is not an Ivy-owned bootstrap task: ' + $ivyName) }
  $ivySpec = @($ivySpecs | Where-Object Name -eq ([string]$ivyName) | Select-Object -First 1)
  $ivyExact = $null -ne $ivySpec -and
    $ivyDefinition.RegistrationInfo.Description -eq $ivySpec.Description -and
    $ivyAction.Path -eq $ivyLauncher -and $ivyAction.Arguments -eq $ivySpec.CommandLine -and
    $ivyAction.WorkingDirectory -eq $ivySpec.Artifact -and
    $ivyDefinition.Settings.Enabled -eq $true
  if (-not $ivyExact) { $ivyRemoveNames += [string]$ivyName }
}

foreach ($ivyName in @($ivyRemoveNames | Select-Object -Unique)) {
  $ivyCurrent = $ivyFolder.GetTask([string]$ivyName)
  $ivyCurrent.Enabled = $false
  if ($ivyCurrent.State -eq 4) { $null = $ivyCurrent.Stop(0) }
  $ivyDeadline = [DateTime]::UtcNow.AddSeconds(30)
  while ($true) {
    $ivyCurrent = $ivyFolder.GetTask([string]$ivyName)
    if ($ivyCurrent.State -notin @(2, 4)) { break }
    if ([DateTime]::UtcNow -ge $ivyDeadline) { throw ('Ivy OS task did not establish a stopped outcome: ' + $ivyName) }
    Start-Sleep -Milliseconds 100
  }
  $ivyFolder.DeleteTask([string]$ivyName, 0)
}

foreach ($ivySpec in $ivySpecs) {
  $ivyProcess = $ivySpec.Process
  $ivyArtifact = $ivySpec.Artifact
  $ivyCommandLine = $ivySpec.CommandLine
  $ivyDescription = $ivySpec.Description
  $ivyExisting = $null
  try { $ivyExisting = $ivyFolder.GetTask($ivyProcess.name) } catch { if ($_.Exception.HResult -ne -2147024894) { throw } }
  if ($null -ne $ivyExisting) {
    # This task was validated and retained above.
  }
  if ($null -eq $ivyExisting) {
    $ivyDefinition = $ivyScheduler.NewTask(0)
    $ivyDefinition.RegistrationInfo.Description = $ivyDescription
    $ivyDefinition.Principal.UserId = $ivySid
    $ivyDefinition.Principal.LogonType = 3
    $ivyDefinition.Principal.RunLevel = 0
    $ivySettings = $ivyDefinition.Settings
    $ivySettings.Enabled = $true
    # Scheduler default 7 starts below-normal work and can starve health checks during builds.
    $ivySettings.Priority = 6
    $ivySettings.Hidden = $true
    $ivySettings.AllowDemandStart = $true
    $ivySettings.StartWhenAvailable = $true
    $ivySettings.DisallowStartIfOnBatteries = $false
    $ivySettings.StopIfGoingOnBatteries = $false
    $ivySettings.MultipleInstances = 2
    $ivySettings.ExecutionTimeLimit = 'PT0S'
    $ivySettings.RestartInterval = 'PT1M'
    $ivySettings.RestartCount = 999
    $ivyLogon = $ivyDefinition.Triggers.Create(9)
    $ivyLogon.UserId = $ivySid
    $ivyTime = $ivyDefinition.Triggers.Create(1)
    $ivyTime.StartBoundary = [DateTime]::Now.AddSeconds(5).ToString('yyyy-MM-ddTHH:mm:ss')
    $ivyTime.Repetition.Interval = 'PT1M'
    $ivyTime.Repetition.StopAtDurationEnd = $false
    $ivyAction = $ivyDefinition.Actions.Create(0)
    $ivyAction.Path = $ivyLauncher
    $ivyAction.Arguments = $ivyCommandLine
    $ivyAction.WorkingDirectory = $ivyArtifact
    $ivyExisting = $ivyFolder.RegisterTaskDefinition($ivyProcess.name, $ivyDefinition, 2, $ivySid, $null, 3, $null)
  }
  $null = $ivyExisting.Run($null)
  $ivyInstalled += $ivyProcess.name
}
@{schemaVersion=1;ok=$true;code='bootstrap_installed';data=@{tasks=$ivyInstalled;user=$ivyIdentity.Name;logonRequired=$true}} | ConvertTo-Json -Depth 4 -Compress
