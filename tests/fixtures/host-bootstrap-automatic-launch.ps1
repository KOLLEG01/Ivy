$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$ivySource = $args[0]
$ivyTokens = $null; $ivyErrors = $null
$ivyAst = [System.Management.Automation.Language.Parser]::ParseFile($ivySource, [ref]$ivyTokens, [ref]$ivyErrors)
if ($ivyErrors.Count -ne 0) { throw 'Production scheduler script failed to parse.' }
$ivyFunctions = @($ivyAst.FindAll({param($ivyNode) $ivyNode -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $ivyNode.Name -ceq 'Get-IvyAutomaticLaunch'}, $true))
if ($ivyFunctions.Count -ne 1) { throw 'Expected one actual production recipe function.' }
# Evaluate only the pure recipe function, never the scheduler script body.
Invoke-Expression $ivyFunctions[0].Extent.Text
$ivyCases = [Console]::In.ReadToEnd() | ConvertFrom-Json
$ivyResults = @($ivyCases | ForEach-Object {
  try { @{ok=$true;value=(Get-IvyAutomaticLaunch $_.plan $_.data)} }
  catch { @{ok=$false} }
})
ConvertTo-Json -InputObject $ivyResults -Depth 16 -Compress
