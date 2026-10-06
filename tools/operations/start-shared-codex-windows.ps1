param(
  [Parameter(Mandatory=$true)][string]$ConfigPath,
  [Parameter(Mandatory=$true)][string]$OwnerPath,
  [Parameter(Mandatory=$true)][ValidatePattern('^[a-fA-F0-9]{64}$')][string]$OwnerHash
)
# The scheduled task owns this launcher and its guarded server tree; Ivy only owns proxy connections.
$ErrorActionPreference='Stop'
$ConfigPath=[IO.Path]::GetFullPath($ConfigPath)
$root=Split-Path -Parent $ConfigPath
$logRoot=Join-Path $root 'state\codex-ui-server'
[IO.Directory]::CreateDirectory($logRoot) | Out-Null
function Get-Sha256Hex([string]$Path) {
  $stream=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite)
  $hash=[Security.Cryptography.IncrementalHash]::CreateHash([Security.Cryptography.HashAlgorithmName]::SHA256)
  $buffer=New-Object byte[] (4MB)
  try {
    while(($read=$stream.Read($buffer,0,$buffer.Length)) -gt 0){$hash.AppendData($buffer,0,$read)}
    return [BitConverter]::ToString($hash.GetHashAndReset()).Replace('-','').ToLowerInvariant()
  } finally {$hash.Dispose();$stream.Dispose()}
}
try {
  $config=Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
  $instances=@($config.instances | Where-Object componentId -eq 'agent-manager')
  if($instances.Count -ne 1){throw 'Exactly one configured AgentManager is required'}
  $settings=$instances[0].settings
  if($settings.appServer.mode -ne 'external-proxy'){throw 'External proxy configuration required'}
  $exe=$settings.nativeExecutable
  if(('sha256:'+(Get-Sha256Hex $exe)) -ne $settings.nativeExecutableHash){throw 'Codex executable identity mismatch'}
  $shell=$settings.windowsShell.executable
  if(('sha256:'+(Get-Sha256Hex $shell)) -ne $settings.windowsShell.executableHash){throw 'PowerShell executable identity mismatch'}
  $env:CODEX_HOME=$settings.codexHome
  if(-not [IO.Path]::IsPathRooted($env:CODEX_HOME)){throw 'An absolute Codex home is required'}
  $env:PATH=(Split-Path -Parent $shell)+';'+$env:PATH
  $log=Join-Path $logRoot 'server.log'
  if(Test-Path -LiteralPath $log){Move-Item -LiteralPath $log -Destination (Join-Path $logRoot 'server.previous.log') -Force}
  @{startedAt=[DateTime]::UtcNow.ToString('o');launcherPid=$PID;nativeVersion=$settings.nativeVersion;nativeExecutableHash=$settings.nativeExecutableHash;codexHome=$env:CODEX_HOME} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $logRoot 'start.json')
$owner=[IO.Path]::GetFullPath($OwnerPath)
if((Get-Sha256Hex $owner) -ne $OwnerHash.ToLowerInvariant()){throw 'Process owner identity mismatch'}
  # Windows PowerShell represents native stderr as ErrorRecord; connector errors must not stop the host.
  $ErrorActionPreference='Continue'
  try {
    & $owner --parent $PID $exe ui-server --listen 'unix://' *> $log
    $serverExit=$LASTEXITCODE
  } finally { $ErrorActionPreference='Stop' }
  @{finishedAt=[DateTime]::UtcNow.ToString('o');exitCode=$serverExit} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $logRoot 'exit.json')
  exit $serverExit
} catch {
  @{failedAt=[DateTime]::UtcNow.ToString('o');error=$_.Exception.Message} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $logRoot 'failure.json')
  exit 1
}
