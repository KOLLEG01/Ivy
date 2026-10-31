param([string]$Mode, [string]$FixtureConfiguration, [Parameter(ValueFromRemainingArguments=$true)][object[]]$Remaining)
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$ivyUtf8 = [Text.UTF8Encoding]::new($false)
if ($Mode -eq 'child') {
  $ivyConfig = Get-Content -LiteralPath $FixtureConfiguration -Raw | ConvertFrom-Json
  $ivyRoot = [string]$ivyConfig.dataRoot
  $null = [IO.Directory]::CreateDirectory($ivyRoot)
  $ivyBoot = [Guid]::NewGuid().ToString(); $ivyLaunch = [Guid]::NewGuid().ToString()
  $ivyHealth = @{schemaVersion=1;instanceId=$ivyConfig.instanceId;componentId=$ivyConfig.componentId;buildId=$ivyConfig.buildId;
    pid=$PID;bootId=$ivyBoot;launchId=$ivyLaunch;startedAt=[DateTime]::UtcNow.ToString('o');observedAt=[DateTime]::UtcNow.ToString('o');
    ready=$true;generation=$null;details=''}
  [IO.File]::WriteAllText((Join-Path $ivyRoot 'health.tmp'),($ivyHealth | ConvertTo-Json -Compress),$ivyUtf8)
  Move-Item -LiteralPath (Join-Path $ivyRoot 'health.tmp') -Destination (Join-Path $ivyRoot 'health.json') -Force
  $ivyDeadline = [DateTime]::UtcNow.AddSeconds(120)
  while (-not (Test-Path -LiteralPath (Join-Path $ivyRoot 'release'))) {
    $ivyControlPath = Join-Path $ivyRoot 'control.json'
    if (Test-Path -LiteralPath $ivyControlPath) {
      $ivyControl = Get-Content -LiteralPath $ivyControlPath -Raw | ConvertFrom-Json
      if ($ivyControl.instanceId -ceq $ivyConfig.instanceId -and $ivyControl.bootId -ceq $ivyBoot -and
          $ivyControl.launchId -ceq $ivyLaunch -and $ivyControl.action -ceq 'shutdown') {
        [IO.File]::WriteAllText((Join-Path $ivyRoot 'draining'),'retained work',$ivyUtf8)
      } else { throw 'Unfenced control request.' }
    }
    if ([DateTime]::UtcNow -gt $ivyDeadline) { throw 'Fixture was not released.' }
    Start-Sleep -Milliseconds 50
  }
  [IO.File]::WriteAllText((Join-Path $ivyRoot 'completed'),'work completed',$ivyUtf8)
  exit 0
}
$ivyData = [Console]::In.ReadToEnd() | ConvertFrom-Json
$ivyPlan = $ivyData.plan
if ($ivyPlan.hostId -cne 'cooperative-bootstrap-fixture' -or $ivyPlan.installationId -cnotmatch '^ivy-next-[0-9a-f]{12}$') { throw 'Not a fixture plan.' }
$ivyScheduler = New-Object -ComObject 'Schedule.Service'; $ivyScheduler.Connect(); $ivyFolder = $ivyScheduler.GetFolder('\')
$ivyDescription = 'IvyNext immutable bootstrap ' + $ivyPlan.candidateId + ' host ' + $ivyPlan.hostId
if ($Mode -eq 'cleanup') {
  foreach ($ivyProcess in $ivyPlan.processes) {
    $ivyTask = $null; try { $ivyTask = $ivyFolder.GetTask([string]$ivyProcess.name) } catch { if ($_.Exception.HResult -ne -2147024894) { throw } }
    if ($null -eq $ivyTask) { continue }
    if ($ivyTask.Definition.RegistrationInfo.Description -cne $ivyDescription) { throw 'Fixture ownership changed.' }
    $ivyConfig = Get-Content -LiteralPath ([string]$ivyProcess.configPath) -Raw | ConvertFrom-Json
    [IO.File]::WriteAllText((Join-Path ([string]$ivyConfig.dataRoot) 'release'),'cleanup',$ivyUtf8)
    $ivyDeadline = [DateTime]::UtcNow.AddSeconds(5)
    while ($ivyTask.State -notin @(1,3) -and [DateTime]::UtcNow -lt $ivyDeadline) { Start-Sleep -Milliseconds 50 }
    if ($ivyTask.State -notin @(1,3)) { $ivyTask.Stop(0) }
    $ivyFolder.DeleteTask([string]$ivyProcess.name,0)
  }
  exit 0
}
if ($Mode -ne 'prepare') { throw 'Unexpected fixture mode.' }
$ivyTokens = $null; $ivyErrors = $null
$ivyAst = [Management.Automation.Language.Parser]::ParseFile([string]$ivyData.source,[ref]$ivyTokens,[ref]$ivyErrors)
if ($ivyErrors.Count -ne 0) { throw 'Production scheduler script failed to parse.' }
foreach ($ivyFunction in $ivyAst.FindAll({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst]},$true)) {
  Invoke-Expression $ivyFunction.Extent.Text
}
$ivySid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
foreach ($ivyProcess in $ivyPlan.processes) {
  $ivyDef = $ivyScheduler.NewTask(0); $ivyDef.RegistrationInfo.Description = $ivyDescription
  $ivyDef.Principal.UserId = $ivySid; $ivyDef.Principal.LogonType = 3; $ivyDef.Principal.RunLevel = 0
  $ivyDef.Settings.Enabled = $true; $ivyDef.Settings.Hidden = $true; $ivyDef.Settings.AllowDemandStart = $true
  $ivyDef.Settings.DisallowStartIfOnBatteries = $false; $ivyDef.Settings.StopIfGoingOnBatteries = $false
  $ivyDef.Settings.MultipleInstances = 2; $ivyDef.Settings.ExecutionTimeLimit = 'PT0S'
  $ivyAction = $ivyDef.Actions.Create(0); $ivyAction.Path = Get-IvyHostLauncher $ivyPlan
  $ivyConfigArg = if ($ivyProcess.componentId -ceq 'host-executor') { [string]$ivyPlan.configPath } else { [string]$ivyProcess.configPath }
  $ivyOwn = @(); if ($ivyProcess.componentId -ceq 'host-executor') { $ivyOwn = @('--instance-config',[string]$ivyProcess.configPath) }
  $ivyBreakaway = @(); if ($ivyProcess.allowWindowsBreakaway -eq $true) { $ivyBreakaway = @('--allow-breakaway') }
  $ivyAction.Arguments = ((@('--parent','0') + $ivyBreakaway + @([string]$ivyProcess.entrypoint.executable) + @($ivyProcess.entrypoint.args) + @('--config',$ivyConfigArg) + $ivyOwn) | ForEach-Object { Quote-IvyArgument $_ }) -join ' '
  $ivyAction.WorkingDirectory = if ($ivyProcess.artifactRoot) { [string]$ivyProcess.artifactRoot } else { [string]$ivyPlan.artifactRoot }
  $ivyTask = $ivyFolder.RegisterTaskDefinition([string]$ivyProcess.name,$ivyDef,2,$ivySid,$null,3,$null)
  $null = $ivyTask.Run($null)
}
