$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false,$true)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false,$true)
$task93Directory=[IO.Path]::GetFullPath($env:IVY_PHONE_CONFIG_ORACLE_DIR)
$task93Resolver=[Func[System.Runtime.Loader.AssemblyLoadContext,System.Reflection.AssemblyName,System.Reflection.Assembly]]{
 param($context,$name)
 $path=[IO.Path]::Combine($task93Directory,$name.Name+'.dll')
 if ([IO.File]::Exists($path)) { return $context.LoadFromAssemblyPath($path) }
 return $null
}
[System.Runtime.Loader.AssemblyLoadContext]::Default.add_Resolving($task93Resolver)
try {
 $task93Assembly=[System.Runtime.Loader.AssemblyLoadContext]::Default.LoadFromAssemblyPath((Join-Path $task93Directory 'Ivy.PhoneRuntime.dll'))
 $evs=$task93Assembly.GetType('Ivy.PhoneBridge.EvsFormat')
 $flags=[Reflection.BindingFlags]'NonPublic,Static'
 $cases=ConvertFrom-Json -InputObject ([Console]::In.ReadToEnd())
 $results=foreach($case in $cases) {
  try {
   switch($case.operation) {
    'channels' { [ordered]@{error=$null;value=$evs.GetMethod('Channels',$flags).Invoke($null,[object[]]@($case.text))} }
    'header' { [ordered]@{error=$null;value=$evs.GetMethod('HeaderFull',$flags).Invoke($null,[object[]]@($case.text))} }
    'sender' {
     $value=$evs.GetMethod('Sender',$flags).Invoke($null,[object[]]@($case.text))
     [ordered]@{error=$null;bitrate=$value.Item1;bandwidth=$value.Item2}
    }
    'select' {
     $parameters=[object[]]@($case.text,[bool]$case.answering,$null,[int]$case.channels)
     $value=$evs.GetMethod('TrySelect',$flags).Invoke($null,$parameters)
     [ordered]@{error=$null;accepted=$value;selected=$parameters[2]}
    }
    'formats' {
     $settings=[Ivy.PhoneBridge.CodecSettings]::new([string[]]$case.preferences,20,20)
     $formats=@(foreach($format in $settings.Formats()) {
      [ordered]@{payload=$format.FormatID;name=$format.FormatName;pcmRate=$format.ClockRate;rtpRate=$format.RtpClockRate;channels=$format.ChannelCount;parameters=$format.Parameters;duration=[uint32]($format.RtpClockRate/50)}
     })
     [ordered]@{error=$null;formats=$formats}
    }
    'validate' {
     $format=[SIPSorceryMedia.Abstractions.AudioFormat]::new([int]$case.payload,[string]$case.name,[int]$case.pcmRate,[int]$case.rtpRate,[int]$case.channels,$case.parameters)
     $null=$task93Assembly.GetType('Ivy.PhoneBridge.MediaCodec').GetMethod('ValidateFormat',$flags).Invoke($null,[object[]]@($format))
     [ordered]@{error=$null;duration=[uint32]($format.RtpClockRate/50)}
    }
    default { throw 'Unknown oracle operation' }
   }
  } catch {
   $failure=$_.Exception
   while(($failure -is [Management.Automation.MethodInvocationException] -or $failure -is [Reflection.TargetInvocationException]) -and $null -ne $failure.InnerException){$failure=$failure.InnerException}
   [ordered]@{error=if($failure -is [ArgumentException]){'invalid_format'}else{$failure.GetType().FullName}}
  }
 }
 [Console]::Out.Write((ConvertTo-Json -InputObject @($results) -Compress -Depth 7))
} finally {[System.Runtime.Loader.AssemblyLoadContext]::Default.remove_Resolving($task93Resolver)}
