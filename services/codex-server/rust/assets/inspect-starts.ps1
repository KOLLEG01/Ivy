function Get-IvySharedServerTasks([object[]]$ExpectedTasks) {
  $ivyScheduler = New-Object -ComObject 'Schedule.Service'
  $ivyScheduler.Connect()
  $ivyFound = @{}
  function Read-IvyStartFolder($Folder) {
    foreach ($ivyTask in @($Folder.GetTasks(1))) { $ivyFound[[string]$ivyTask.Path] = $ivyTask }
    foreach ($ivyChild in @($Folder.GetFolders(0))) { Read-IvyStartFolder $ivyChild }
  }
  Read-IvyStartFolder ($ivyScheduler.GetFolder('\'))
  # Enumeration can omit another principal's definition. Read each identified
  # legacy definition by its exact path as well; never mutate it here.
  foreach ($ivyPath in @($ExpectedTasks)) {
    if ([string]$ivyPath -notmatch '^\\[^*?]+$') { throw 'Use complete Scheduler paths for legacy definitions.' }
    try {
      $ivyTask = $ivyScheduler.GetFolder('\').GetTask([string]$ivyPath)
      $ivyFound[[string]$ivyTask.Path] = $ivyTask
    } catch { if ($_.Exception.HResult -ne -2147024894) { throw } }
  }
  foreach ($ivyTask in $ivyFound.Values) {
    $ivyFullPath = [string]$ivyTask.Path
    [pscustomobject]@{
      TaskPath = $ivyFullPath.Substring(0, $ivyFullPath.LastIndexOf('\') + 1)
      TaskName = [string]$ivyTask.Name
      State = if ($ivyTask.Enabled) { 'Enabled' } else { 'Disabled' }
      Actions = @(foreach ($ivyAction in @($ivyTask.Definition.Actions)) {
        [pscustomobject]@{ Execute=[string]$ivyAction.Path; Arguments=[string]$ivyAction.Arguments; WorkingDirectory=[string]$ivyAction.WorkingDirectory }
      })
    }
  }
}
function Assert-IvySharedServerStarts([string]$InputJson) {
  $ErrorActionPreference = 'Stop'
  $ivyInput = $InputJson | ConvertFrom-Json
  $ivyHome = [IO.Path]::GetFullPath([string]$ivyInput.home).TrimEnd('\','/')
  $ivyHome = $ivyHome.Replace('\\?\UNC\','\\').Replace('\\?\','')
  foreach ($ivyTask in @(Get-IvySharedServerTasks @($ivyInput.legacyTasks))) {
    if ($ivyTask.State -eq 'Disabled') { continue }
    $ivyIdentity = [string]$ivyTask.TaskPath + [string]$ivyTask.TaskName
    $ivyKnown = @($ivyInput.legacyTasks) -contains $ivyIdentity
    foreach ($ivyAction in @($ivyTask.Actions)) {
      $ivyText = [string]$ivyAction.Execute + ' ' + [string]$ivyAction.Arguments + ' ' + [string]$ivyAction.WorkingDirectory
      foreach ($ivyMatch in [regex]::Matches([string]$ivyAction.Arguments, '(?i)"([^"]+\.ps1)"|''([^'']+\.ps1)''|(?:^|\s)([A-Za-z]:\\[^\s]+\.ps1)')) {
        $ivyScript = @($ivyMatch.Groups | Select-Object -Skip 1 | Where-Object Success | Select-Object -First 1).Value
        if ([IO.Path]::IsPathRooted($ivyScript) -and (Test-Path -LiteralPath $ivyScript -PathType Leaf)) {
          $ivyFile = Get-Item -LiteralPath $ivyScript
          if ($ivyFile.Length -le 1048576) { $ivyText += ' ' + [IO.File]::ReadAllText($ivyScript) }
        }
      }
      # Exact configured home distinguishes other legitimate server installations.
      $ivyNormalized = $ivyText.Replace('/','\').Replace('\\?\UNC\','\\').Replace('\\?\','')
      $ivySelected = $ivyNormalized -match '(?i)codex|app-server|ui-server|start-server' -and
        $ivyNormalized -match ('(?i)' + [regex]::Escape($ivyHome) + '(?=$|[\\\s''";])')
      if ($ivyKnown -or $ivySelected) { throw ('Enabled startup definition for this server: ' + $ivyIdentity) }
    }
  }
}
