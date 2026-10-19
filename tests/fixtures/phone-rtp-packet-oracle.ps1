$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false,$true)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false,$true)
$task93Directory=[IO.Path]::GetFullPath($env:IVY_PHONE_CONFIG_ORACLE_DIR)
$task93Assembly=[Runtime.Loader.AssemblyLoadContext]::Default.LoadFromAssemblyPath((Join-Path $task93Directory 'SIPSorcery.dll'))
$cases=ConvertFrom-Json -InputObject ([Console]::In.ReadToEnd())
$results=foreach($case in $cases){
 $packet=[SIPSorcery.Net.RTPPacket]::new([Convert]::FromHexString($case))
 [ordered]@{sequence=$packet.Header.SequenceNumber;timestamp=$packet.Header.Timestamp;source=$packet.Header.SyncSource;payloadType=$packet.Header.PayloadType;marker=$packet.Header.MarkerBit;payload=[Convert]::ToHexString($packet.GetPayloadBytes()).ToLowerInvariant()}
}
[Console]::Out.Write((ConvertTo-Json -InputObject @($results) -Compress -Depth 5))
