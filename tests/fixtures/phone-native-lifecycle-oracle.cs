using System;
using System.Collections.Generic;
using System.Text.Json;
using Ivy.PhoneBridge;

// Compare the existing handler's lifecycle DTO ordering and only id/state
// fields of its call projection. No status/feature/platform parity is implied.
// Ports stay closed, no registration/account or non-loopback traffic is used.
public static class PhoneNativeLifecycleOracle {
    sealed class Port : IPcmAudioPort {
        public bool IsOpen => false;
        public long Generation => 0;
        public int ReadCaptured(Span<float> output) { output.Clear(); return 0; }
        public void WriteReceived(ReadOnlySpan<float> input) {}
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());
        var owner=new NativeSipOperations(_=>new Port());
        var rows=new List<object>();
        try {
            foreach(var step in input.RootElement.EnumerateArray()) {
                try {
                    string method=step.GetProperty("method").GetString();
                    var result=owner.InvokeAsync(method,step.GetProperty("params")).GetAwaiter().GetResult();
                    using var json=JsonDocument.Parse(JsonSerializer.Serialize(result,NativeRpc.Json));
                    var value=json.RootElement;
                    rows.Add(method=="configure" ? new { configured=value.GetProperty("configured").GetBoolean() } :
                        method=="call.release" ? (object)new { callId=value.GetProperty("callId").GetString(),released=value.GetProperty("released").GetBoolean() } :
                        new { id=value.GetProperty("id").GetString(),state=value.GetProperty("state").GetString() });
                } catch(Exception failure) {
                    rows.Add(new { error=failure is NativeRpcException rpc ? rpc.Code :
                        failure is ArgumentException || failure is JsonException ? "invalid_request" : "operation_outcome_unknown" });
                }
            }
        } finally { owner.DisposeAsync().AsTask().GetAwaiter().GetResult(); }
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}
