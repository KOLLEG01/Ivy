$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false,$true)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false,$true)
$task93Directory=[IO.Path]::GetFullPath($env:IVY_PHONE_CONFIG_ORACLE_DIR)
$task93References=@(Get-ChildItem -LiteralPath "$PSHOME/ref" -Filter '*.dll' | ForEach-Object FullName)
$task93Managed=@(Get-ChildItem -LiteralPath $task93Directory -Filter '*.dll' | ForEach-Object FullName)
foreach($path in $task93Managed){$null=[Runtime.Loader.AssemblyLoadContext]::Default.LoadFromAssemblyPath($path)}
Add-Type -Path @((Join-Path $PSScriptRoot 'phone-native-dial-oracle.cs'),(Join-Path $PSScriptRoot 'phone-native-screening-oracle.cs')) -ReferencedAssemblies ($task93References+$task93Managed) -CompilerOptions '/nowarn:1701'
[PhoneNativeDialOracle]::Run()
