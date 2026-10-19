$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false,$true)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false,$true)
$task93Directory=[IO.Path]::GetFullPath($env:IVY_PHONE_CONFIG_ORACLE_DIR)
$task93References=@(Get-ChildItem -LiteralPath "$PSHOME/ref" -Filter '*.dll' | ForEach-Object FullName)
# Load the sealed managed oracle dependencies before any background callback.
$task93Managed=@(Get-ChildItem -LiteralPath $task93Directory -Filter '*.dll' | ForEach-Object FullName)
foreach($path in $task93Managed){$null=[Runtime.Loader.AssemblyLoadContext]::Default.LoadFromAssemblyPath($path)}
$task93References+=$task93Managed
Add-Type -Path (Join-Path $PSScriptRoot 'phone-rtp-peer.cs') -ReferencedAssemblies $task93References
[PhoneRtpOracle]::Run()
