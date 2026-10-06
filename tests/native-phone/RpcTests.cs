using System.Diagnostics;
using System.Reflection;
using System.Text;
using System.Text.Json;
using Ivy.PhoneBridge;

static partial class Program {
    static JsonElement Args(object value) => JsonSerializer.SerializeToElement(value, NativeRpc.Json);
    static NativeRequest Request(string epoch, long id, string method, object args, string? operation = null) =>
        NativeRpc.Parse(JsonSerializer.SerializeToUtf8Bytes(new { version = 1, epoch, requestId = id, operationId = operation, method, @params = args }, NativeRpc.Json));
    static async Task RpcUnits() {
        string epoch = Guid.NewGuid().ToString(), operation = Guid.NewGuid().ToString(); int calls = 0;
        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var finish = new TaskCompletionSource<object>(TaskCreationOptions.RunContinuationsAsynchronously);
        var rpc = new NativeRpc(epoch, (method, args) => { Interlocked.Increment(ref calls); started.TrySetResult(); return finish.Task; }, 1);
        var original = rpc.ExecuteAsync(Request(epoch, 1, "call.dial", new { callId = operation }, operation));
        await started.Task.WaitAsync(TimeSpan.FromSeconds(2));
        var repeated = rpc.ExecuteAsync(Request(epoch, 2, "call.dial", new { callId = operation }, operation));
        Check((await rpc.ExecuteAsync(Request(epoch, 3, "call.hangup", new { callId = operation }, operation))).Error == "operation_conflict", "different method cannot reuse original intent");
        Check((await rpc.ExecuteAsync(Request(epoch, 4, "call.dial", new { callId = "changed" }, operation))).Error == "operation_conflict", "changed payload cannot reuse original intent");
        Check((await rpc.ExecuteAsync(Request(Guid.NewGuid().ToString(), 5, "call.dial", new { }, operation))).Error == "epoch_mismatch", "different native epoch cannot dispatch");
        Check((await rpc.ExecuteAsync(Request(epoch, 6, "call.dial", new { }, Guid.NewGuid().ToString()))).Error == "capacity_exceeded", "receipt capacity never evicts an unknown operation");
        finish.SetResult(new { confirmed = true });
        var results = await Task.WhenAll(original, repeated);
        Check(calls == 1 && results.All(value => value.Ok) && results[0].RequestId == 1 && results[1].RequestId == 2, "concurrent retry shares one effect and distinct response correlation");
        Check((await rpc.ExecuteAsync(Request(epoch, 7, "call.dial", new { callId = operation }, operation))).Ok && calls == 1, "completed receipt reused without dispatch");
        rpc.Stop(); Check((await rpc.ExecuteAsync(Request(epoch, 8, "status", new { }))).Error == "runtime_stopping", "stopped native owner cannot dispatch");
        var failed = new NativeRpc(epoch, (_, _) => throw new Exception("private-credential-fixture"));
        var reserved = new NativeRpc(epoch, (_, _) => Task.FromResult<object>(new { done = true }), 6);
        for (int index = 0; index < 2; index++) Check((await reserved.ExecuteAsync(Request(epoch, 20 + index, "call.claim", new { }, Guid.NewGuid().ToString()))).Ok, "ordinary effects fit before cleanup reserve");
        Check((await reserved.ExecuteAsync(Request(epoch, 22, "call.dial", new { }, Guid.NewGuid().ToString()))).Error == "capacity_exceeded", "ordinary work cannot consume cleanup reserve");
        foreach (var method in new[] { "call.hangup", "call.desktop.stopVoice", "call.release", "shutdown" })
            Check((await reserved.ExecuteAsync(Request(epoch, 23, method, new { }, Guid.NewGuid().ToString()))).Ok, "cleanup retains reserved native receipt capacity");
        var unknown = await failed.ExecuteAsync(Request(epoch, 9, "call.dial", new { }, operation));
        Check(unknown.Error == "operation_outcome_unknown" && !JsonSerializer.Serialize(unknown).Contains("private-credential-fixture"), "post-dispatch errors stay unknown and redact exception text");
        foreach (byte[] invalid in new[] { new byte[NativeRpc.FrameBytes + 1], Encoding.UTF8.GetBytes("{\"version\":1,\"version\":1}"),
            Encoding.UTF8.GetBytes("{\"version\":1}"), new byte[] { 0xff } }) {
            bool rejected = false; try { NativeRpc.Parse(invalid); } catch (JsonException) { rejected = true; }
            Check(rejected, "strict framing rejects oversized, duplicate, missing and invalid UTF8");
        }
    }
    sealed class PipePeer : IAsyncDisposable {
        public Process Process { get; }
        public string Epoch { get; } = Guid.NewGuid().ToString();
        private long sequence;
        private readonly SemaphoreSlim write = new(1, 1);
        private readonly Dictionary<long, TaskCompletionSource<NativeResponse>> pending = new();
        private readonly CancellationTokenSource stop = new();
        private readonly Task reader, heartbeats;
        public bool SendHeartbeats = true;
        private readonly string? temporaryOwner;
        public PipePeer(string? nativeExecutable = null, string? ownerRoot = null, string fixtureMode = "--ipc-fixture") {
            var info = new ProcessStartInfo(Environment.ProcessPath!) { UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true };
            info.ArgumentList.Add(nativeExecutable ?? Assembly.GetExecutingAssembly().Location);
            info.ArgumentList.Add(nativeExecutable == null ? fixtureMode : "--epoch"); info.ArgumentList.Add(Epoch);
            if (nativeExecutable != null) {
                if (ownerRoot == null) { temporaryOwner = Path.Combine(Path.GetTempPath(), "ivy-native-owner-" + Guid.NewGuid()); Directory.CreateDirectory(temporaryOwner); }
                info.ArgumentList.Add("--owner-root"); info.ArgumentList.Add(ownerRoot ?? temporaryOwner!);
            }
            Process = System.Diagnostics.Process.Start(info) ?? throw new Exception("Fixture child did not start.");
            reader = ReadAsync(); heartbeats = HeartbeatsAsync();
        }
        public async Task<NativeResponse> Send(string method, object args, string? operation = null) {
            long id = Interlocked.Increment(ref sequence);
            var completion = new TaskCompletionSource<NativeResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
            lock (pending) pending.Add(id, completion);
            await write.WaitAsync(stop.Token);
            try { await Process.StandardInput.WriteLineAsync(JsonSerializer.Serialize(new { version = 1, epoch = Epoch, requestId = id,
                operationId = operation, method, @params = args }, NativeRpc.Json)); await Process.StandardInput.FlushAsync(); }
            finally { write.Release(); }
            return await completion.Task.WaitAsync(TimeSpan.FromSeconds(8));
        }
        private async Task ReadAsync() {
            try {
                while (await Process.StandardOutput.ReadLineAsync(stop.Token) is { } line) {
                    var response = JsonSerializer.Deserialize<NativeResponse>(line, NativeRpc.Json)!;
                    lock (pending) if (pending.Remove(response.RequestId, out var completion)) completion.TrySetResult(response);
                }
            } catch (OperationCanceledException) { }
            finally { lock (pending) foreach (var completion in pending.Values) completion.TrySetException(new IOException("Owned fixture pipe ended.")); }
        }
        private async Task HeartbeatsAsync() {
            try { while (!stop.IsCancellationRequested) { if (SendHeartbeats) await Send("heartbeat", new { }); await Task.Delay(200, stop.Token); } }
            catch (Exception) when (stop.IsCancellationRequested || Process.HasExited) { }
        }
        public async ValueTask DisposeAsync() {
            stop.Cancel();
            if (!Process.HasExited) { Process.StandardInput.Close(); try { await Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5)); } catch (TimeoutException) { Process.Kill(true); await Process.WaitForExitAsync(); } }
            try { await Task.WhenAll(reader, heartbeats); } catch { }
            Process.Dispose(); stop.Dispose(); write.Dispose();
            if (temporaryOwner != null) Directory.Delete(temporaryOwner, true);
        }
    }
    static async Task PipeCapacityFixture(string epoch) {
        int observations = 0;
        var finish = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await new NativePipeServer(epoch, Console.OpenStandardInput(), Console.OpenStandardOutput(), async (method, args) => {
            if (method == "desktop.observe") { Interlocked.Increment(ref observations); await finish.Task; }
            if (method == "call.release") finish.TrySetResult();
            return new { method, observations = Volatile.Read(ref observations) };
        }, () => { finish.TrySetResult(); return Task.CompletedTask; }).RunAsync();
    }
    static async Task PipeCapacity() {
        await using var peer = new PipePeer(fixtureMode: "--pipe-capacity-fixture");
        var pending = Enumerable.Range(0, 32).Select(_ => peer.Send("desktop.observe", new { appUserModelId = "Fixture.Package!UI" })).ToList();
        // Observe the requests actually admitted by the child, rather than assuming pipe writes ran.
        var deadline = Stopwatch.StartNew();
        while (true) {
            var status = await peer.Send("status", new { });
            Check(status.Ok, "reserved status reaches saturated native pipe");
            int admitted = status.Result!.Value.GetProperty("observations").GetInt32();
            if (admitted == 32) break;
            Check(admitted < 32 && deadline.Elapsed < TimeSpan.FromSeconds(5) && pending.Count < 64,
                "native ordinary requests reached their bound; admitted=" + admitted + ", attempted=" + pending.Count);
            // A concurrent heartbeat/status can temporarily consume an ordinary admission slot.
            // Only an explicit capacity refusal of a read-only observation permits another attempt.
            int refused = pending.Count(task => task.IsCompletedSuccessfully && task.Result.Error == "capacity_exceeded");
            int waiting = pending.Count(task => !task.IsCompleted);
            Check(pending.All(task => !task.IsCompleted || task.IsCompletedSuccessfully && task.Result.Error == "capacity_exceeded"),
                "setup observations are either held or explicitly refused for capacity");
            if (refused > 0 && waiting < 32)
                pending.Add(peer.Send("desktop.observe", new { appUserModelId = "Fixture.Package!UI" }));
            await Task.Delay(10);
        }
        string callId = Guid.NewGuid().ToString();
        Check((await peer.Send("call.prepare", new { callId }, Guid.NewGuid().ToString())).Error == "capacity_exceeded", "ordinary native request cannot consume cleanup capacity");
        foreach (string method in new[] { "call.hangup", "call.desktop.stopVoice", "call.release" }) {
            var result = await peer.Send(method, new { callId }, Guid.NewGuid().ToString());
            Check(result.Ok && result.Result!.Value.GetProperty("method").GetString() == method, "complete original cleanup reaches saturated native pipe: " + method);
        }
        var outcomes = await Task.WhenAll(pending);
        Check(outcomes.Count(result => result.Ok) == 32 && outcomes.All(result => result.Ok || result.Error == "capacity_exceeded"),
            "cleanup release drains exactly32 admitted observations; only explicit setup-capacity refusals are excluded");
    }
    static async Task PipeLoopback() {
        await PipeCapacity();
        await using var caller = new PipePeer(); await using var callee = new PipePeer();
        var config = new { binding = new SipBinding("127.0.0.1", 0, "udp"), codecs = new CodecSettings(new[] { "PCMA" }),
            incomingPeers = new[] { "127.0.0.1" }, registration = (object?)null, password = (string?)null };
        Check((await caller.Send("configure", config, Guid.NewGuid().ToString())).Ok, "child caller configures actual native SIP");
        var configured = await callee.Send("configure", config, Guid.NewGuid().ToString()); Check(configured.Ok, "child callee configures actual native SIP");
        string endpoint = configured.Result!.Value.GetProperty("endpoint").GetString()!, callId = Guid.NewGuid().ToString();
        Check((await caller.Send("call.prepare", new { callId }, Guid.NewGuid().ToString())).Ok, "child prepares original call");
        string dialOperation = Guid.NewGuid().ToString();
        var dialArgs = new { callId, destination = "sip:fixture@" + endpoint, username = (string?)null, password = (string?)null, ringSeconds = 5 };
        var dial = caller.Send("call.dial", dialArgs, dialOperation); var retry = caller.Send("call.dial", dialArgs, dialOperation);
        var wait = Stopwatch.StartNew(); bool ringing = false;
        while (wait.Elapsed < TimeSpan.FromSeconds(3)) {
            var incoming = await callee.Send("status", new { });
            if (incoming.Result!.Value.GetProperty("call").ValueKind == JsonValueKind.Object) { ringing = true; break; }
            await Task.Delay(10);
        }
        Check(ringing, "peer received the original pipe-dispatched INVITE");
        NativeResponse status = await caller.Send("status", new { });
        Check(status.Ok && status.Result!.Value.GetProperty("call").GetProperty("id").GetString() == callId, "status responds while original dial and repeated receipt remain pending");
        Check((await caller.Send("call.hangup", new { callId }, Guid.NewGuid().ToString())).Ok, "hangup remains reachable during pending dial");
        var outcomes = await Task.WhenAll(dial, retry);
        Check(outcomes.All(value => value.Ok && value.Result!.Value.GetProperty("state").GetString() == "local_ended"), "both pipe responses retain the same cancelled outcome");
        Console.WriteLine("phone_pipe_phase: child configure, INVITE, concurrent receipt/status and hangup passed; testing heartbeat loss");
        caller.SendHeartbeats = false;
        await caller.Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(18));
        Check(caller.Process.ExitCode == 0, "heartbeat loss closes only owned fixture runtime");
        Console.WriteLine("phone_pipe_phase: heartbeat loss passed; testing EOF");
        callee.SendHeartbeats = false; callee.Process.StandardInput.Close();
        await callee.Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
        Check(callee.Process.ExitCode == 0, "pipe EOF closes owned fixture runtime");
    }
}
