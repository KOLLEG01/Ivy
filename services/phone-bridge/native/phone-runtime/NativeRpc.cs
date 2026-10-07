using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Ivy.PhoneBridge;

public sealed record NativeRequest(int Version, string Epoch, long RequestId, string OperationId, string Method, JsonElement Params);
public sealed record NativeResponse(int Version, string Epoch, long RequestId, bool Ok, JsonElement? Result, string Error);
public sealed class NativeRpcException(string code) : Exception(code) { public string Code { get; } = code; }

// Inherited stdio only, one parent-owned epoch. The service persists original intent before IPC.
// This bounded in-process receipt map prevents duplicate dispatch after a lost pipe response; it
// is not recovery authority after native process loss. That loss remains unknown in the service.
public sealed class NativeRpc {
    public const int FrameBytes = 65536;
    public static readonly JsonSerializerOptions Json = new() {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase, PropertyNameCaseInsensitive = false,
        UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow, MaxDepth = 24
    };
    private sealed record Receipt(string Hash, Task<(JsonElement? Result, string Error)> Result);
    private readonly Dictionary<string, Receipt> receipts = new(StringComparer.Ordinal);
    private readonly Func<string, JsonElement, Task<object>> invoke;
    private readonly object sync = new();
    private readonly string epoch;
    private readonly int capacity;
    private readonly Func<int> cleanupSlots;
    private bool stopping;
    public NativeRpc(string epoch, Func<string, JsonElement, Task<object>> invoke, int capacity = 4096, Func<int> cleanupSlots = null) {
        if (!Guid.TryParseExact(epoch, "D", out _) || capacity is < 1 or > 4096) throw new ArgumentException("Bounded native epoch required.");
        this.epoch = epoch; this.invoke = invoke; this.capacity = capacity; this.cleanupSlots = cleanupSlots;
    }
    public void Stop() { lock (sync) stopping = true; }
    private static bool ReadOnly(string method) => method is "audio.probe" or "codec.test" or "inventory" or "heartbeat" or "status" or "desktop.observe" or "desktop.capture" or "desktop.captureOwner" or "desktop.process" or "desktop.controls";
    private static void Unique(JsonElement value) {
        if (value.ValueKind == JsonValueKind.Object) {
            var names = new HashSet<string>(StringComparer.Ordinal);
            foreach (var property in value.EnumerateObject()) {
                if (!names.Add(property.Name)) throw new JsonException("Duplicate property.");
                Unique(property.Value);
            }
        } else if (value.ValueKind == JsonValueKind.Array) foreach (var item in value.EnumerateArray()) Unique(item);
    }
    public static NativeRequest Parse(ReadOnlyMemory<byte> frame) {
        if (frame.Length is < 1 or > FrameBytes) throw new JsonException("Frame outside bounds.");
        using var document = JsonDocument.Parse(frame, new JsonDocumentOptions { MaxDepth = 24 });
        Unique(document.RootElement);
        var properties = document.RootElement.EnumerateObject().Select(p => p.Name).ToHashSet(StringComparer.Ordinal);
        if (!properties.SetEquals(new[] { "version", "epoch", "requestId", "operationId", "method", "params" })) throw new JsonException("Exact envelope required.");
        var request = document.RootElement.Deserialize<NativeRequest>(Json);
        if (request == null || request.Version != 1 || !Guid.TryParseExact(request.Epoch, "D", out _) || request.RequestId is < 1 or > 9007199254740991 ||
            request.Params.ValueKind != JsonValueKind.Object || request.Method is not ("audio.probe" or "codec.test" or "inventory" or "registration.reconnect" or "configure" or "heartbeat" or "status" or "desktop.observe" or "desktop.launch" or "desktop.capture" or "desktop.captureOwner" or "desktop.process" or "desktop.controls" or "call.desktop.launch" or "call.desktop.startVoice" or "call.desktop.pauseVoice" or "call.desktop.resumeVoice" or "call.desktop.stopVoice" or "call.screening.prepare" or "call.screening.bridge" or "call.features" or "call.windows.connect" or "call.codec.upgrade" or "call.realtime.prepare" or "call.realtime.answer" or "call.realtime.stop" or "call.prepare" or "call.audio.prepare" or "call.audio.rebind" or "call.dial" or "call.claim" or "call.answer" or "call.waiting.end" or "call.command.feedback" or "call.hangup" or "call.release" or "shutdown") ||
            (ReadOnly(request.Method) ? request.OperationId != null : !Guid.TryParseExact(request.OperationId, "D", out _))) throw new JsonException("Invalid native envelope.");
        return request;
    }
    public async Task<NativeResponse> ExecuteAsync(NativeRequest request) {
        Task<(JsonElement? Result, string Error)> pending;
        lock (sync) {
            if (request.Epoch != epoch) return Error(request, "epoch_mismatch");
            if (stopping) return Error(request, "runtime_stopping");
            if (ReadOnly(request.Method)) pending = Dispatch(request);
            else {
                // Exact serialized params are the retry contract. Requests use fresh requestIds;
                // method/params/operationId remain unchanged. No secret payload is retained here.
                string hash = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(request.Method + "\n" + request.Params.GetRawText())));
                if (receipts.TryGetValue(request.OperationId, out var receipt)) {
                    if (receipt.Hash != hash) return Error(request, "operation_conflict");
                    pending = receipt.Result;
                } else {
                    bool cleanup = request.Method is "call.hangup" or "call.release" or "call.desktop.stopVoice" or "call.realtime.stop" or "shutdown";
                    int required = Math.Clamp(cleanupSlots?.Invoke() ?? 4, 0, 128);
                    int reserve = Math.Min(Math.Max(0, required - (cleanup ? 4 : 0)), Math.Max(0, capacity - 1));
                    if (receipts.Count >= capacity - reserve) return Error(request, "capacity_exceeded");
                    pending = Dispatch(request); receipts.Add(request.OperationId, new Receipt(hash, pending));
                }
            }
        }
        var result = await pending;
        return new(1, epoch, request.RequestId, result.Error == null, result.Result, result.Error);
    }
    private NativeResponse Error(NativeRequest request, string error) => new(1, epoch, request.RequestId, false, null, error);
    private Task<(JsonElement? Result, string Error)> Dispatch(NativeRequest request) {
        // The saved promise owns only the safe result, not the async state containing credentials.
        var completion = new TaskCompletionSource<(JsonElement? Result, string Error)>(TaskCreationOptions.RunContinuationsAsynchronously);
        _ = CompleteAsync(request, completion); return completion.Task;
    }
    private async Task CompleteAsync(NativeRequest request, TaskCompletionSource<(JsonElement? Result, string Error)> completion) {
        completion.TrySetResult(await InvokeAsync(request));
    }
    private async Task<(JsonElement? Result, string Error)> InvokeAsync(NativeRequest request) {
        await Task.Yield();
        try {
            Task<object> action;
            lock (sync) { if (stopping) return (null, "runtime_stopping"); action = invoke(request.Method, request.Params); }
            var result = JsonSerializer.SerializeToElement(await action, Json);
            if (Encoding.UTF8.GetByteCount(result.GetRawText()) > FrameBytes - 512) return (null, "operation_outcome_unknown");
            return (result, null);
        } catch (NativeRpcException error) { return (null, error.Code); }
        catch (ArgumentException) { return (null, "invalid_request"); }
        catch (JsonException) { return (null, "invalid_request"); }
        catch { return (null, "operation_outcome_unknown"); }
    }
}
