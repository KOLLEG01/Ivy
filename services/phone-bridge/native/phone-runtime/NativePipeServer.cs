using System.Text.Json;

namespace Ivy.PhoneBridge;

// The caller launches this process with inherited pipes inside its existing OS job. EOF, invalid
// framing, blocked output, or loss of the15s monotonic heartbeat ends only the owned Phone runtime.
public sealed class NativePipeServer {
    private readonly Stream input, output;
    private readonly Func<string, JsonElement, Task<object>> handler;
    private readonly Func<Task> shutdown;
    private readonly string epoch;
    private readonly TimeProvider time;
    private readonly Func<int> cleanupSlots;
    private long heartbeat;
    private readonly SemaphoreSlim writer = new(1, 1);
    public NativePipeServer(string epoch, Stream input, Stream output, Func<string, JsonElement, Task<object>> handler, Func<Task> shutdown, TimeProvider time = null, Func<int> cleanupSlots = null) {
        this.epoch = epoch; this.input = input; this.output = output; this.handler = handler; this.shutdown = shutdown;
        this.time = time ?? TimeProvider.System; heartbeat = this.time.GetTimestamp();
        this.cleanupSlots = cleanupSlots;
    }
    public async Task RunAsync(CancellationToken cancellationToken = default) {
        using var stop = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        var rpc = new NativeRpc(epoch, async (method, args) => {
            if (method is "heartbeat" or "shutdown") {
                if (args.EnumerateObject().Any()) throw new ArgumentException("No control arguments.");
                if (method == "heartbeat") Interlocked.Exchange(ref heartbeat, time.GetTimestamp());
                else { stop.Cancel(); return new { stopping = true }; }
            }
            return await handler(method, args);
        }, cleanupSlots: cleanupSlots);
        var active = new HashSet<Task>();
        var watchdog = WatchdogAsync(stop);
        var frame = new byte[NativeRpc.FrameBytes]; var buffer = new byte[4096]; int length = 0;
        try {
            while (!stop.IsCancellationRequested) {
                // Windows Console's inherited stdin may perform a synchronous pipe read which
                // ignores cancellation. Keep it on a background worker and bound our wait, so
                // lease loss still closes SIP and lets the owning process exit without another byte.
                int count = await Task.Run(async () => await input.ReadAsync(buffer, stop.Token), stop.Token).WaitAsync(stop.Token);
                if (count == 0) break;
                for (int index = 0; index < count; index++) {
                    byte value = buffer[index];
                    if (value != 10) {
                        if (length == frame.Length) throw new JsonException("Frame too large.");
                        frame[length++] = value; continue;
                    }
                    var request = NativeRpc.Parse(frame.AsMemory(0, length)); Array.Clear(frame, 0, length); length = 0;
                    active.RemoveWhere(task => task.IsCompleted);
                    bool control = request.Method is "heartbeat" or "status" or "call.hangup" or "call.desktop.stopVoice" or "call.release" or "shutdown";
                    if (active.Count >= (control ? 48 : 32)) {
                        await WriteAsync(new(1, epoch, request.RequestId, false, null, "capacity_exceeded"), stop.Token);
                        continue;
                    }
                    active.Add(RespondAsync(rpc, request, stop));
                }
            }
        } catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
        finally {
            rpc.Stop(); stop.Cancel(); Array.Clear(frame); Array.Clear(buffer);
            try { await shutdown().WaitAsync(TimeSpan.FromSeconds(4)); }
            finally { await watchdog; }
        }
    }
    private async Task RespondAsync(NativeRpc rpc, NativeRequest request, CancellationTokenSource stop) {
        try { await WriteAsync(await rpc.ExecuteAsync(request), stop.Token); }
        catch { stop.Cancel(); }
    }
    private async Task WriteAsync(NativeResponse response, CancellationToken cancellationToken) {
        byte[] bytes = JsonSerializer.SerializeToUtf8Bytes(response, NativeRpc.Json);
        if (bytes.Length > NativeRpc.FrameBytes) throw new InvalidOperationException("Response too large.");
        await writer.WaitAsync(cancellationToken);
        try { await output.WriteAsync(bytes, cancellationToken); await output.WriteAsync(new byte[] { 10 }, cancellationToken); await output.FlushAsync(cancellationToken); }
        finally { writer.Release(); }
    }
    private async Task WatchdogAsync(CancellationTokenSource stop) {
        using var timer = new PeriodicTimer(TimeSpan.FromSeconds(1), time);
        try {
            while (await timer.WaitForNextTickAsync(stop.Token)) {
                var elapsed = time.GetElapsedTime(Interlocked.Read(ref heartbeat));
                if (elapsed < TimeSpan.Zero || elapsed >= TimeSpan.FromSeconds(15)) { stop.Cancel(); return; }
            }
        } catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
    }
}
