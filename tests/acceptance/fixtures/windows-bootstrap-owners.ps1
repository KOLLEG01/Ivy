param([Parameter(Mandatory=$true)][string]$PlanPath,[Parameter(Mandatory=$true)][ValidateSet('stop','start')][string]$Action,
  [Parameter(Mandatory=$true)][string]$ExpectedHostId,
  [Parameter(Mandatory=$true)][ValidatePattern('^ivy-next-[0-9a-f]{12}$')][string]$ExpectedInstallationId,
  [Parameter(Mandatory=$true)][string]$ExecutorInstanceId,
  [Parameter(Mandatory=$true)][ValidateRange(1,32)][int]$ExpectedOwnerCount)
$ErrorActionPreference = 'Stop'
$ivyPlan = Get-Content -LiteralPath $PlanPath -Raw | ConvertFrom-Json
if ($ivyPlan.hostId -cne $ExpectedHostId -or $ivyPlan.installationId -cne $ExpectedInstallationId) { throw 'Wrong isolated installation.' }
if ($ivyPlan.owners.Count -ne $ExpectedOwnerCount -or @($ivyPlan.owners.instanceId | Sort-Object -Unique).Count -ne $ExpectedOwnerCount) { throw 'Expected distinct reviewed owners.' }
if (@($ivyPlan.owners | Where-Object { $_.instanceId -ceq $ExecutorInstanceId }).Count -ne 1) { throw 'Expected one reviewed executor.' }
$ivyScheduler = New-Object -ComObject Schedule.Service
$ivyScheduler.Connect()
$ivyFolder = $ivyScheduler.GetFolder('\')
function Get-IvyHash($Task) {
  $ivyXml = [System.Xml.XmlDocument]::new()
  $ivyXml.XmlResolver = $null
  $ivyXml.LoadXml($Task.Xml)
  if ($null -ne $ivyXml.DocumentType) { throw 'DTD is forbidden.' }
  $ivyEnabled = $ivyXml.SelectSingleNode("/*[local-name()='Task']/*[local-name()='Settings']/*[local-name()='Enabled']")
  if ($null -ne $ivyEnabled) { $null = $ivyEnabled.ParentNode.RemoveChild($ivyEnabled) }
  $ivySha = [Security.Cryptography.SHA256]::Create()
  try { 'sha256:' + [BitConverter]::ToString($ivySha.ComputeHash([Text.Encoding]::UTF8.GetBytes($ivyXml.OuterXml))).Replace('-','').ToLowerInvariant() } finally { $ivySha.Dispose() }
}
function Get-IvyTask($Owner) {
  if ($Owner.name -cnotmatch ('^' + [regex]::Escape($ExpectedInstallationId) + '-[0-9a-f]{12}$')) { throw 'Task leaves the reviewed installation.' }
  $ivyTask = $ivyFolder.GetTask($Owner.name)
  if ((Get-IvyHash $ivyTask) -cne $Owner.definitionHash) { throw 'Task definition changed after verified inspection.' }
  return $ivyTask
}
# Verify the whole exact snapshot before any side effect, then recheck each affected owner.
foreach ($ivyOwner in $ivyPlan.owners) { $null = Get-IvyTask $ivyOwner }
if ($Action -eq 'stop') {
  foreach ($ivyOwner in $ivyPlan.owners) { (Get-IvyTask $ivyOwner).Enabled = $false }
  foreach ($ivyOwner in ($ivyPlan.owners | Sort-Object { if ($_.instanceId -ceq $ExecutorInstanceId) { 1 } else { 0 } })) {
    (Get-IvyTask $ivyOwner).Stop(0)
  }
  $ivyDeadline = [DateTime]::UtcNow.AddSeconds(60)
  do {
    $ivyActive = @($ivyPlan.owners | Where-Object { (Get-IvyTask $_).State -notin @(1,3) })
    if ($ivyActive.Count -eq 0) { break }
    Start-Sleep -Milliseconds 200
  } while ([DateTime]::UtcNow -lt $ivyDeadline)
  if ($ivyActive.Count -ne 0) { throw 'Owned OS stop deadline exceeded.' }
} else {
  foreach ($ivyOwner in ($ivyPlan.owners | Sort-Object { if ($_.instanceId -ceq $ExecutorInstanceId) { 0 } else { 1 } })) {
    $ivyTask = Get-IvyTask $ivyOwner
    $ivyTask.Enabled = [bool]$ivyOwner.enabled
    if ($ivyOwner.running -and $ivyTask.State -ne 4) { $null = $ivyTask.Run($null) }
  }
}
@{ok=$true;action=$Action;owners=@($ivyPlan.owners | ForEach-Object {
  $ivyTask = Get-IvyTask $_
  @{instanceId=$_.instanceId;name=$_.name;definitionHash=$_.definitionHash;enabled=$ivyTask.Enabled;state=$ivyTask.State}
})} | ConvertTo-Json -Depth 5 -Compress
