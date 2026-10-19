$ErrorActionPreference='Stop'
$packages=@(Get-AppxPackage -Name 'OpenAI.Codex')
if($packages.Count -ne 1 -or $packages[0].Status -ne 'Ok'){throw 'Desktop package unavailable or servicing'}
$package=$packages[0]
$session=(Get-Process -Id $PID).SessionId
$root=[IO.Path]::GetFullPath($package.InstallLocation).TrimEnd('\')+'\'
$targets=@(Get-Process -Name Codex -ErrorAction SilentlyContinue | Where-Object {$_.SessionId -eq $session -and $_.Path -and $_.Path.StartsWith($root,[StringComparison]::OrdinalIgnoreCase)})
{{CLOSE}}
if({{START}}){
 $manifest=Get-AppxPackageManifest -Package $package.PackageFullName
 $ui=@($manifest.Package.Applications.Application)[0].Id
 if(-not $ui){throw 'Desktop application missing'}
 Start-Process -FilePath explorer.exe -ArgumentList ('shell:AppsFolder\'+$package.PackageFamilyName+'!'+$ui) -WindowStyle Hidden
}
Write-Output 'ok'
