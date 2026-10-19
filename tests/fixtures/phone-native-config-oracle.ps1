$ErrorActionPreference='Stop'
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
 $task93Operations=$task93Assembly.GetType('Ivy.PhoneBridge.NativeSipOperations')
 $task93Configuration=$task93Assembly.GetType('Ivy.PhoneBridge.NativeSipConfiguration')
 $task93Read=$task93Operations.GetMethod('Read',[Reflection.BindingFlags]'NonPublic,Static').MakeGenericMethod($task93Configuration)
 $task93Frames=[System.Text.Json.JsonSerializer]::Deserialize([Console]::In.ReadToEnd(),[string[]])
 $task93Results=foreach($frame in $task93Frames) {
  try {
   $request=[Ivy.PhoneBridge.NativeRpc]::Parse([ReadOnlyMemory[byte]]::new([Text.Encoding]::UTF8.GetBytes($frame)))
   $task93Parameters=$request.Params
   $fields=[Collections.Generic.List[string]]::new([string[]]@('binding','codecs','incomingPeers','registration','password'))
   $unused=[Text.Json.JsonElement]::new()
   if($task93Parameters.TryGetProperty('micro',[ref]$unused)){$fields.Add('micro')}
   $value=$task93Read.Invoke($null,[object[]]@($task93Parameters,[string[]]$fields.ToArray()))
   if($null -eq $value.Binding -or $null -eq $value.Codecs -or $null -eq $value.IncomingPeers -or $value.IncomingPeers.Length -gt 32 -or [Linq.Enumerable]::Distinct([string[]]$value.IncomingPeers,[StringComparer]::Ordinal).Count -ne $value.IncomingPeers.Length){throw [ArgumentException]::new('Invalid configuration')}
   $peers=@($value.IncomingPeers | Where-Object {$_ -cne 'registered'} | ForEach-Object {[Net.IPAddress]::Parse($_)})
   $endpoint=$value.Binding.Endpoint()
   $value.Codecs.Validate()
   if($null -ne $value.Micro){$value.Micro.Validate()}
   if($null -ne $value.Registration){
    $protocol=[Enum]::Parse([SIPSorcery.SIP.SIPProtocolsEnum],$value.Binding.Transport)
    $value.Registration.Validate($protocol)
    if($null -eq $value.Password -or $value.Password.Length -gt 4096 -or @($value.Password.ToCharArray()|Where-Object{[char]::IsControl($_)}).Count -gt 0){throw [ArgumentException]::new('Invalid password')}
   } elseif($null -ne $value.Password){throw [ArgumentException]::new('Password without registration')}
   $proxy=if($null -ne $value.Binding.OutboundProxy){[SIPSorcery.SIP.SIPEndPoint]::ParseSIPEndPoint($value.Binding.OutboundProxy).ToString()}else{$null}
   [ordered]@{error=$null;endpoint=$endpoint.ToString();proxy=$proxy;incoming=@($peers|ForEach-Object{$_.ToString()}|Sort-Object -Unique);registered=$value.IncomingPeers -ccontains 'registered';packetMs=$value.Codecs.PacketMs;receiveReorderMs=$value.Codecs.ReceiveReorderMs;preferences=@($value.Codecs.Preferences);ringSeconds=$value.Binding.IncomingRingSeconds;offerMode=$value.Binding.OutgoingOfferMode;delayed=($value.Binding.OutgoingOfferMode -ceq 'delayed' -or ($value.Binding.OutgoingOfferMode -ceq 'auto' -and $value.Codecs.Preferences[0] -ceq 'EVS'));rtpStart=$value.Binding.RtpPortRangeStart;rtpEnd=$value.Binding.RtpPortRangeEnd;maxCalls=$value.Binding.MaxConcurrentCalls;keepalive=if($null -ne $value.Registration){$value.Registration.KeepAliveIntervalSeconds}else{$null};passwordUnits=if($null -ne $value.Password){$value.Password.Length}else{$null};micro=if($null -ne $value.Micro){$value.Micro.UsbipExecutable}else{$null}}
  } catch {
   $failure=$_.Exception.GetBaseException()
   [ordered]@{error=if($failure -is [ArgumentException] -or $failure -is [Text.Json.JsonException]){'invalid_request'}else{'operation_outcome_unknown'}}
  }
 }
 [Console]::Out.Write((ConvertTo-Json -InputObject @($task93Results) -Compress -Depth 7))
} finally {[System.Runtime.Loader.AssemblyLoadContext]::Default.remove_Resolving($task93Resolver)}
