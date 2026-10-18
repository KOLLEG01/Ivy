param([Parameter(Mandatory=$true)][string]$JobName, [string]$ObservedIds='')
$ErrorActionPreference='Stop'
if($JobName -notmatch '^Global\\Ivy\.dashboards-test-[0-9a-f-]{36}$'){throw 'Unexpected fixture job name'}
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class DashboardJob {
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr OpenJobObject(uint access, bool inherit, string name);
  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool QueryInformationJobObject(IntPtr job, int kind, IntPtr buffer, uint length, out uint returned);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
}
'@
$job=[DashboardJob]::OpenJobObject(4,$false,$JobName)
$exists=$job -ne [IntPtr]::Zero
$ids=@()
if($exists){
  $buffer=[Runtime.InteropServices.Marshal]::AllocHGlobal(65536)
  try {
    [uint32]$returned=0
    if(-not [DashboardJob]::QueryInformationJobObject($job,3,$buffer,65536,[ref]$returned)){throw 'Cannot read owned fixture job'}
    $count=[Runtime.InteropServices.Marshal]::ReadInt32($buffer,4)
    if($count -lt 0 -or $count -gt 4096){throw 'Unexpected fixture process count'}
    for($index=0;$index -lt $count;$index++){$ids+=([Runtime.InteropServices.Marshal]::ReadIntPtr($buffer,8+$index*[IntPtr]::Size)).ToInt64()}
  } finally {
    [Runtime.InteropServices.Marshal]::FreeHGlobal($buffer)
    [void][DashboardJob]::CloseHandle($job)
  }
} elseif([Runtime.InteropServices.Marshal]::GetLastWin32Error() -ne 2){throw 'Cannot observe owned fixture job'}
$allIds=@($ids)
if($ObservedIds){foreach($number in $ObservedIds.Split(',')){if($number -notmatch '^\d+$'){throw 'Invalid observed PID'};$allIds+=[long]$number}}
$rows=@(foreach($number in ($allIds | Select-Object -Unique)){
  $process=Get-CimInstance Win32_Process -Filter "ProcessId=$number"
  if($process){[pscustomobject]@{id=[long]$process.ProcessId;parent=[long]$process.ParentProcessId;executable=$process.ExecutablePath;created=$process.CreationDate.ToUniversalTime().ToString('o')}}
})
[pscustomobject]@{exists=$exists;members=@($ids);processes=$rows} | ConvertTo-Json -Depth 5 -Compress
