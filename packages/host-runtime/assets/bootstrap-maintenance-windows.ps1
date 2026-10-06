$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-principal.ps1')
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
function Quote-IvyArgument([string]$Value) {
  '"' + ([regex]::Replace([regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1')) + '"'
}
function Read-IvyXml([string]$Content) {
  if ($Content.Length -gt 65536) { throw 'Definition is too large.' }
  $ivyDocument = [System.Xml.XmlDocument]::new()
  $ivyDocument.XmlResolver = $null
  $ivyDocument.LoadXml($Content)
  if ($null -ne $ivyDocument.DocumentType) { throw 'DTD is not supported.' }
  return ,$ivyDocument
}
function Get-IvyHash([string]$Content) {
  $ivyDocument = Read-IvyXml $Content
  $ivyEnabled = $ivyDocument.SelectSingleNode("/*[local-name()='Task']/*[local-name()='Settings']/*[local-name()='Enabled']")
  if ($null -ne $ivyEnabled) { $null = $ivyEnabled.ParentNode.RemoveChild($ivyEnabled) }
  $ivySha = [Security.Cryptography.SHA256]::Create()
  try { 'sha256:' + [BitConverter]::ToString($ivySha.ComputeHash([Text.Encoding]::UTF8.GetBytes($ivyDocument.OuterXml))).Replace('-', '').ToLowerInvariant() } finally { $ivySha.Dispose() }
}
function Assert-IvyDefinition($Definition) {
  if ((Get-IvyHash $Definition.content) -cne $Definition.hash) { throw 'Retained definition changed.' }
}
function Get-IvyOwner($Instance, $Plans) {
  if ($Plans.Count -lt 1 -or $Instance.name -cnotmatch ('^' + [regex]::Escape($Plans[0].installationId) + '-[0-9a-f]{12}$') -or $Plans[0].installationId -cnotmatch '^ivy-next-[0-9a-f]{12}$') { throw 'Invalid owned task name.' }
  $ivyTask = $ivyFolder.GetTask($Instance.name)
  $ivyDefinition = $ivyTask.Definition
  if ($ivyDefinition.Actions.Count -ne 1) { throw 'Unexpected task actions.' }
  $ivyAction = $ivyDefinition.Actions.Item(1)
  $ivyMatches = @($Plans | Where-Object {
    $ivyPlan = $_
    $ivyProcess = @($ivyPlan.processes | Where-Object { $_.instanceId -ceq $Instance.instanceId })
    if ($ivyProcess.Count -ne 1) { return $false }
    $ivyProcess = $ivyProcess[0]
    $ivyBreakaway = @(); if ($ivyProcess.allowWindowsBreakaway -eq $true) { $ivyBreakaway = @('--allow-breakaway') }
    $ivyArtifact = if ($ivyProcess.artifactRoot) { [string]$ivyProcess.artifactRoot } else { [string]$ivyPlan.artifactRoot }
    $ivyExecutable = if ($ivyProcess.entrypoint -and $ivyProcess.entrypoint.executable -ne 'node') { [string]$ivyProcess.entrypoint.executable } else { [string]$ivyPlan.nodeExecutable }
    $ivyArgs = if ($ivyProcess.entrypoint) { @($ivyProcess.entrypoint.args) } elseif ($ivyProcess.componentId -eq 'host-executor') { @('dist\packages\host-runtime\src\executor.js') } else { @('dist\services\' + $ivyProcess.componentId + '\src\main.js') }
    $ivyConfig = if ($ivyProcess.componentId -eq 'host-executor') { [string]$ivyPlan.configPath } elseif ($ivyProcess.configPath) { [string]$ivyProcess.configPath } else { [string]$ivyPlan.configPath }
    $ivyOwnConfig = @(); if ($ivyProcess.componentId -eq 'host-executor' -and $ivyProcess.configPath) { $ivyOwnConfig = @('--instance-config', [string]$ivyProcess.configPath) }
    $ivyArguments = ((@('--parent', '0') + $ivyBreakaway + @($ivyExecutable) + $ivyArgs + @('--config', $ivyConfig) + $ivyOwnConfig) | ForEach-Object { Quote-IvyArgument $_ }) -join ' '
    $ivyAction.Type -eq 0 -and $ivyAction.Path -ceq (Join-Path $ivyPlan.artifactRoot 'dist/native/ivy-host-job.exe') -and
      $ivyAction.Arguments -ceq $ivyArguments -and $ivyAction.WorkingDirectory -ceq $ivyArtifact -and
      $ivyDefinition.RegistrationInfo.Description -ceq ('IvyNext immutable bootstrap ' + $ivyPlan.candidateId + ' host ' + $ivyPlan.hostId)
  })
  $ivyCandidates = @($ivyMatches | ForEach-Object {
    $ivyMatchedPlan = $_
    $ivyMatchedProcess = @($ivyMatchedPlan.processes | Where-Object { $_.instanceId -ceq $Instance.instanceId })[0]
    if ($ivyMatchedProcess.candidateId) { [string]$ivyMatchedProcess.candidateId } else { [string]$ivyMatchedPlan.candidateId }
  } | Sort-Object -Unique)
  if ($ivyMatches.Count -lt 1 -or $ivyCandidates.Count -ne 1 -or (Resolve-IvyPrincipalSid $ivyDefinition.Principal.UserId) -ne $ivySid -or $ivyDefinition.Principal.LogonType -ne 3 -or $ivyDefinition.Principal.RunLevel -ne 0) { throw 'Task does not have one retained bootstrap candidate and its exact owner/arguments.' }
  $ivyContent = $ivyTask.Xml
  $ivyHash = Get-IvyHash $ivyContent
  $ivyProcess = @($ivyMatches[0].processes | Where-Object { $_.instanceId -ceq $Instance.instanceId })[0]
  $ivyCandidate = $ivyCandidates[0]
  @{instanceId=$Instance.instanceId; componentId=$ivyProcess.componentId; name=$Instance.name; candidateId=$ivyCandidate; definitionHash=$ivyHash;
    enabled=[bool]$ivyTask.Enabled; running=($ivyTask.State -notin @(1,3)); definition=@{content=$ivyContent;hash=$ivyHash}}
}
try {
  $ivyStage = 'read-request'
  $ivyInput = [Console]::In.ReadToEnd() | ConvertFrom-Json
  $ivyData = $ivyInput.data
  if ($ivyInput.action -eq 'check') {
    Assert-IvyDefinition $ivyData.definition
    $ivyResult = $true
  } elseif ($ivyInput.action -eq 'render') {
    Assert-IvyDefinition $ivyData.owner.definition
    $ivyXml = Read-IvyXml $ivyData.owner.definition.content
    $ivyAction = $ivyXml.SelectSingleNode("/*[local-name()='Task']/*[local-name()='Actions']/*[local-name()='Exec']")
    if ($null -eq $ivyAction) { throw 'Missing task action.' }
    $ivyNext = $ivyData.next
    $ivyProcess = @($ivyNext.processes | Where-Object { $_.instanceId -ceq $ivyData.owner.instanceId })
    if ($ivyProcess.Count -ne 1) { throw 'Missing next process definition.' }
    $ivyProcess = $ivyProcess[0]
    $ivyBreakaway = @(); if ($ivyProcess.allowWindowsBreakaway -eq $true) { $ivyBreakaway = @('--allow-breakaway') }
    $ivyArtifact = if ($ivyProcess.artifactRoot) { [string]$ivyProcess.artifactRoot } else { [string]$ivyNext.artifactRoot }
    $ivyExecutable = if ($ivyProcess.entrypoint -and $ivyProcess.entrypoint.executable -ne 'node') { [string]$ivyProcess.entrypoint.executable } else { [string]$ivyNext.nodeExecutable }
    $ivyArgs = if ($ivyProcess.entrypoint) { @($ivyProcess.entrypoint.args) } elseif ($ivyProcess.componentId -eq 'host-executor') { @('dist\packages\host-runtime\src\executor.js') } else { @('dist\services\' + $ivyProcess.componentId + '\src\main.js') }
    $ivyConfig = if ($ivyProcess.componentId -eq 'host-executor') { [string]$ivyNext.configPath } elseif ($ivyProcess.configPath) { [string]$ivyProcess.configPath } else { [string]$ivyNext.configPath }
    $ivyOwnConfig = @(); if ($ivyProcess.componentId -eq 'host-executor' -and $ivyProcess.configPath) { $ivyOwnConfig = @('--instance-config', [string]$ivyProcess.configPath) }
    $ivyChanges = @{
      Command=(Join-Path $ivyNext.artifactRoot 'dist/native/ivy-host-job.exe');
      Arguments=((@('--parent','0') + $ivyBreakaway + @($ivyExecutable) + $ivyArgs + @('--config',$ivyConfig) + $ivyOwnConfig) | ForEach-Object { Quote-IvyArgument $_ }) -join ' ';
      WorkingDirectory=$ivyArtifact
    }
    foreach ($ivyKey in $ivyChanges.Keys) {
      $ivyNode = $ivyAction.SelectSingleNode("*[local-name()='$ivyKey']")
      if ($null -eq $ivyNode) { throw 'Incomplete task action.' }
      $ivyNode.InnerText = $ivyChanges[$ivyKey]
    }
    $ivyDescription = $ivyXml.SelectSingleNode("/*[local-name()='Task']/*[local-name()='RegistrationInfo']/*[local-name()='Description']")
    if ($null -eq $ivyDescription) { throw 'Missing bootstrap description.' }
    $ivyDescription.InnerText = 'IvyNext immutable bootstrap ' + $ivyNext.candidateId + ' host ' + $ivyNext.hostId
    $ivyEnabled = $ivyXml.SelectSingleNode("/*[local-name()='Task']/*[local-name()='Settings']/*[local-name()='Enabled']")
    if ($null -eq $ivyEnabled -or $ivyEnabled.InnerText -cne 'false') { throw 'Maintenance requires disabled scheduler entries.' }
    $ivyResult = @{content=$ivyXml.OuterXml;hash=(Get-IvyHash $ivyXml.OuterXml)}
  } else {
    $ivyScheduler = New-Object -ComObject 'Schedule.Service'
    $ivyScheduler.Connect()
    $ivyFolder = $ivyScheduler.GetFolder('\')
    $ivySid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($ivyInput.action -eq 'inspect') {
      $ivyResult = @($ivyData.plans[0].processes | ForEach-Object { Get-IvyOwner $_ $ivyData.plans })
    } elseif ($ivyInput.action -eq 'pause') {
      $ivyPlans = @($ivyData.plans); $ivySnapshot = $ivyData.snapshot; $ivyOwners = @($ivySnapshot.owners)
      if ($ivyPlans.Count -lt 1 -or $ivySnapshot.hostId -cne $ivyPlans[0].hostId -or
          $ivySnapshot.installationId -cne $ivyPlans[0].installationId -or $ivyOwners.Count -ne @($ivyPlans[0].processes).Count) {
        throw 'Bootstrap snapshot belongs to another installation.'
      }
      foreach ($ivyOwner in $ivyOwners) {
        $ivyCurrent = Get-IvyOwner $ivyOwner $ivyPlans
        if ($ivyCurrent.candidateId -cne $ivyOwner.candidateId -or $ivyCurrent.definitionHash -cne $ivyOwner.definitionHash -or
            $ivyCurrent.enabled -ne [bool]$ivyOwner.enabled -or $ivyCurrent.running -ne [bool]$ivyOwner.running) {
          throw 'Bootstrap owner changed after its exact snapshot.'
        }
      }
      foreach ($ivyOwner in $ivyOwners) { ($ivyFolder.GetTask([string]$ivyOwner.name)).Enabled = $false }
      foreach ($ivyOwner in @($ivyOwners | Sort-Object { if ($_.componentId -ceq 'host-executor') { 1 } else { 0 } })) {
        $ivyTask = $ivyFolder.GetTask([string]$ivyOwner.name); if ($ivyTask.State -in @(2,4)) { $null = $ivyTask.Stop(0) }
      }
      $ivyDeadline = [DateTime]::UtcNow.AddSeconds(60)
      do {
        $ivyActive = @($ivyOwners | Where-Object { ($ivyFolder.GetTask([string]$_.name)).State -notin @(1,3) })
        if ($ivyActive.Count -eq 0) { break }; Start-Sleep -Milliseconds 200
      } while ([DateTime]::UtcNow -lt $ivyDeadline)
      if ($ivyActive.Count -ne 0) { throw 'Bootstrap owners did not stop for maintenance.' }
      $ivyResult = $true
    } elseif ($ivyInput.action -eq 'resume') {
      $ivyPlans = @($ivyData.plans); $ivySnapshot = $ivyData.snapshot; $ivyOwners = @($ivySnapshot.owners)
      if ($ivyPlans.Count -lt 1 -or $ivySnapshot.hostId -cne $ivyPlans[0].hostId -or
          $ivySnapshot.installationId -cne $ivyPlans[0].installationId -or $ivyOwners.Count -ne @($ivyPlans[0].processes).Count) {
        throw 'Bootstrap resume belongs to another installation.'
      }
      foreach ($ivyOwner in $ivyOwners) { $null = Get-IvyOwner $ivyOwner $ivyPlans }
      foreach ($ivyOwner in @($ivyOwners | Sort-Object { if ($_.componentId -ceq 'host-executor') { 0 } else { 1 } })) {
        $ivyTask = $ivyFolder.GetTask([string]$ivyOwner.name); $ivyTask.Enabled = [bool]$ivyOwner.enabled
        if ($ivyOwner.running -and $ivyTask.State -ne 4) { $null = $ivyTask.Run($null) }
      }
      $ivyResult = $true
    } elseif ($ivyInput.action -eq 'launch') {
      $ivyStage = 'validate-launch-plan'
      $ivyPlan = $ivyData.plan; $ivyOperation = [string]$ivyData.operationId
      if ($ivyPlan.os -cne 'win32' -or $ivyPlan.installationId -cnotmatch '^ivy-next-[0-9a-f]{12}$' -or
          $ivyOperation.Length -lt 1 -or $ivyOperation.Length -gt 128) { throw 'Invalid automatic bootstrap request.' }
      $ivyStage = 'inspect-bootstrap-owners'
      foreach ($ivyProcess in @($ivyPlan.processes)) { $null = Get-IvyOwner $ivyProcess @($ivyPlan) }
      $ivyStage = 'validate-launch-path'
      $ivyRequest = [IO.Path]::GetFullPath([string]$ivyData.requestPath)
      $ivyRuntime = [IO.Path]::GetFullPath([string]$ivyPlan.runtimeRoot).TrimEnd('\') + '\'
      if (-not $ivyRequest.StartsWith($ivyRuntime + 'bootstrap-automatic\', [StringComparison]::OrdinalIgnoreCase) -or
          [IO.Path]::GetFileName($ivyRequest) -cne 'request.json') { throw 'Automatic request leaves the host runtime.' }
      $ivyName = [string]$ivyPlan.installationId + '-maintenance'
      $ivyDescription = 'IvyNext automatic bootstrap maintenance ' + $ivyOperation + ' host ' + [string]$ivyPlan.hostId
      $ivyLauncher = Join-Path ([string]$ivyPlan.artifactRoot) 'dist\native\ivy-host-job.exe'
      $ivyWorker = Join-Path ([string]$ivyPlan.artifactRoot) 'dist\packages\host-runtime\src\bootstrap-auto-worker.js'
      if (-not (Test-Path -LiteralPath $ivyLauncher -PathType Leaf) -or -not (Test-Path -LiteralPath $ivyWorker -PathType Leaf)) { throw 'Automatic bootstrap worker is incomplete.' }
      $ivyStage = 'inspect-maintenance-task'
      $ivyArgs = @('--parent','0',[string]$ivyPlan.nodeExecutable,$ivyWorker,'--config',[string]$ivyPlan.configPath,
        '--request',$ivyRequest,'--distribution',[string]$ivyPlan.artifactRoot)
      $ivyArguments = ($ivyArgs | ForEach-Object { Quote-IvyArgument $_ }) -join ' '
      $ivyExisting = $null; try { $ivyExisting = $ivyFolder.GetTask($ivyName) } catch { if ($_.Exception.HResult -ne -2147024894) { throw } }
      if ($null -ne $ivyExisting -and $ivyExisting.State -in @(2,4)) {
        if ($ivyExisting.Definition.RegistrationInfo.Description -cne $ivyDescription) { throw 'Another maintenance task owns this name.' }
      } else {
        $ivyStage = 'register-maintenance-task'
        $ivyDefinition = $ivyScheduler.NewTask(0); $ivyDefinition.RegistrationInfo.Description = $ivyDescription
        $ivyDefinition.Principal.UserId = $ivySid; $ivyDefinition.Principal.LogonType = 3; $ivyDefinition.Principal.RunLevel = 0
        $ivyDefinition.Settings.Enabled = $true; $ivyDefinition.Settings.Hidden = $true; $ivyDefinition.Settings.AllowDemandStart = $true
        $ivyDefinition.Settings.DisallowStartIfOnBatteries = $false; $ivyDefinition.Settings.StopIfGoingOnBatteries = $false
        $ivyDefinition.Settings.MultipleInstances = 2; $ivyDefinition.Settings.ExecutionTimeLimit = 'PT15M'
        $ivyDefinition.Settings.RestartCount = 3; $ivyDefinition.Settings.RestartInterval = 'PT1M'
        # The task is started explicitly below. A boot trigger requires elevation
        # and prevents a standard-user bootstrap owner from registering it.
        $ivyAction = $ivyDefinition.Actions.Create(0); $ivyAction.Path = $ivyLauncher; $ivyAction.Arguments = $ivyArguments
        $ivyAction.WorkingDirectory = [string]$ivyPlan.artifactRoot
        $ivyExisting = $ivyFolder.RegisterTaskDefinition($ivyName, $ivyDefinition, 6, $ivySid, $null, 3, $null)
        $ivyStage = 'start-maintenance-task'
        $null = $ivyExisting.Run($null)
      }
      $ivyResult = @{name=$ivyName}
    } elseif ($ivyInput.action -eq 'apply') {
      Assert-IvyDefinition $ivyData.desired
      $ivyCurrent = Get-IvyOwner $ivyData.owner $ivyData.plans
      if ($ivyCurrent.enabled -or $ivyCurrent.running -or $ivyCurrent.definitionHash -cnotin @($ivyData.owner.definitionHash, $ivyData.desired.hash)) { throw 'Task changed or started during maintenance.' }
      if ($ivyCurrent.definitionHash -cne $ivyData.desired.hash) {
        $ivyDefinition = $ivyScheduler.NewTask(0)
        $ivyDefinition.XmlText = $ivyData.desired.content
        if ($ivyDefinition.Settings.Enabled) { throw 'Replacement must remain disabled.' }
        $ivyRechecked = Get-IvyOwner $ivyData.owner $ivyData.plans
        if ($ivyRechecked.enabled -or $ivyRechecked.running -or $ivyRechecked.definitionHash -cne $ivyCurrent.definitionHash) { throw 'Task changed before publication.' }
        $null = $ivyFolder.RegisterTaskDefinition($ivyData.owner.name, $ivyDefinition, 4, $ivySid, $null, 3, $null)
      }
      $ivyAfter = Get-IvyOwner $ivyData.owner $ivyData.plans
      if ($ivyAfter.enabled -or $ivyAfter.running -or $ivyAfter.definitionHash -cne $ivyData.desired.hash) { throw 'Scheduler replacement did not retain the exact desired definition.' }
      $ivyResult = $true
    } else { throw 'Unknown maintenance operation.' }
  }
  @{ok=$true;value=$ivyResult} | ConvertTo-Json -Depth 16 -Compress
} catch {
  # Never return task XML, configuration contents or arbitrary OS exception text as an error.
  $ivyHresult = ('0x{0:X8}' -f ($_.Exception.HResult -band 0xffffffff))
  @{ok=$false;code='target_conflict';message=('Scheduler definition, principal or scheduling state does not match this maintenance operation. Stage: ' + $ivyStage + '; HRESULT: ' + $ivyHresult)} | ConvertTo-Json -Compress
}
