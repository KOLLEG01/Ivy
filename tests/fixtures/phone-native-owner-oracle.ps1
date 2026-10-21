$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot '../../services/phone-bridge/native/phone-runtime/NativeOwnerLease.cs'
$imports = "using System;`nusing System.IO;`nusing System.Collections.Generic;`nusing System.Reflection;`n"
$harness = @'
public static class NativeOwnerOracle {
    public static string Run(string input) {
        using var document = JsonDocument.Parse(input);
        var read = typeof(NativeOwnerLease).GetMethod("Read", BindingFlags.NonPublic | BindingFlags.Static)!;
        var exited = typeof(NativeOwnerLease).GetMethod("OriginalExited", BindingFlags.NonPublic | BindingFlags.Static)!;
        var results = new List<object>();
        foreach (var item in document.RootElement.EnumerateArray()) {
            var bytes = new List<byte>();
            foreach (var value in item.EnumerateArray()) bytes.Add(value.GetByte());
            try {
                if (bytes.Count is < 2 or > 4096) throw new InvalidOperationException();
                using var stream = new MemoryStream(bytes.ToArray());
                using var record = JsonDocument.Parse(stream, new JsonDocumentOptions { MaxDepth = 4 });
                var owner = (NativeOwnerLease.Owner)read.Invoke(null, new object[] { record.RootElement })!;
                var encoded = JsonSerializer.Serialize(owner, new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase });
                results.Add(new { ok = true, epoch = owner.Epoch, pid = owner.Pid, creationFileTime = owner.CreationFileTime, originalExited = (bool)exited.Invoke(null, new object[] { owner })!, encoded });
            } catch { results.Add(new { ok = false }); }
        }
        return JsonSerializer.Serialize(results);
    }
}
'@
Add-Type -TypeDefinition ($imports + [IO.File]::ReadAllText($source) + $harness)
[Console]::Out.Write([Ivy.PhoneBridge.NativeOwnerOracle]::Run([Console]::In.ReadToEnd()))
