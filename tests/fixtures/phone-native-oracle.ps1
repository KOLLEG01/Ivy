$ErrorActionPreference = 'Stop'
$sourcePath = Join-Path $PSScriptRoot '../../services/phone-bridge/native/phone-runtime/NativeRpc.cs'
$imports = "using System;`nusing System.Collections.Generic;`nusing System.Linq;`nusing System.Threading.Tasks;`n"
Add-Type -TypeDefinition ($imports + [IO.File]::ReadAllText($sourcePath))
$frames = [System.Text.Json.JsonSerializer]::Deserialize([Console]::In.ReadToEnd(), [string[]])
$results = foreach ($frame in $frames) {
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($frame)
        $request = [Ivy.PhoneBridge.NativeRpc]::Parse([ReadOnlyMemory[byte]]::new($bytes))
        [ordered]@{ ok = $true; epoch = $request.Epoch; requestId = $request.RequestId; operationId = $request.OperationId; method = $request.Method; params = $request.Params.GetRawText() }
    } catch {
        [ordered]@{ ok = $false }
    }
}
[Console]::Out.Write((ConvertTo-Json -InputObject @($results) -Depth 4 -Compress))
