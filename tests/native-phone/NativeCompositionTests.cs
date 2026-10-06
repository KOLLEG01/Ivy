using System.Diagnostics;
using System.Text.Json;
using System.Net;
using Ivy.PhoneBridge;

static partial class Program {
    static async Task NativeExpiredOffer() {
        await using var operations = new NativeSipOperations(_ => new AudioPort());
        var configured = Args(await operations.InvokeAsync("configure", Args(new {
            binding = new SipBinding("127.0.0.1", 0, "udp", 1), codecs = new CodecSettings(new[] { "PCMA" }),
            incomingPeers = new[] { "127.0.0.1" }, registration = (object?)null, password = (string?)null
        })));
        await using var caller = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp"), new CodecSettings(new[] { "PCMA" }), _ => new AudioPort(), _ => false);
        var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString());
        var dialing = outgoing.DialAsync("sip:fixture@" + configured.GetProperty("endpoint").GetString(), null, null, 5);
        string? original = null; var timer = Stopwatch.StartNew();
        while (timer.Elapsed < TimeSpan.FromSeconds(2)) {
            var call = Args(await operations.InvokeAsync("status", Args(new { }))).GetProperty("call");
            if (call.ValueKind == JsonValueKind.Object) { original = call.GetProperty("id").GetString(); break; }
            await Task.Delay(10);
        }
        Check(original != null, "unclaimed offer has one original identity");
        await dialing.WaitAsync(TimeSpan.FromSeconds(6)); timer.Restart();
        while (Args(await operations.InvokeAsync("status", Args(new { }))).GetProperty("call").ValueKind != JsonValueKind.Null && timer.Elapsed < TimeSpan.FromSeconds(2)) await Task.Delay(10);
        Check(Args(await operations.InvokeAsync("status", Args(new { }))).GetProperty("call").ValueKind == JsonValueKind.Null, "expired unclaimed offer releases native resources");
        bool expired = false;
        try { await operations.InvokeAsync("call.claim", Args(new { callId = original })); } catch (NativeRpcException error) { expired = error.Code == "phone_offer_expired"; }
        Check(expired, "late handoff cannot claim an expired offer");
        string replacement = Guid.NewGuid().ToString();
        await operations.InvokeAsync("call.prepare", Args(new { callId = replacement }));
        await operations.InvokeAsync("call.release", Args(new { callId = replacement })); await caller.ReleaseAsync(outgoing);
    }
    static async Task NativeCallHandoff() {
        await using var operations = new NativeSipOperations(_ => new AudioPort());
        var config = new { binding = new SipBinding("127.0.0.1", 0, "udp"), codecs = new CodecSettings(new[] { "PCMA" }),
            incomingPeers = new[] { "127.0.0.1" }, registration = (object?)null, password = (string?)null };
        var configured = Args(await operations.InvokeAsync("configure", Args(config)));
        string endpoint = configured.GetProperty("endpoint").GetString()!, previous = Guid.NewGuid().ToString();
        await operations.InvokeAsync("call.prepare", Args(new { callId = previous }));
        await using var caller = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp"), new CodecSettings(new[] { "PCMA" }), _ => new AudioPort(), _ => false);
        var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString());
        var fields = System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic;
        var commandGate = typeof(NativeSipOperations).GetField("sync", fields)!.GetValue(operations)!;
        var nativeHost = (SipTransportHost)typeof(NativeSipOperations).GetField("host", fields)!.GetValue(operations)!;
        Task<object>? releasing = null; Task<SipCallObservation>? dialing = null; string? incomingId = null;
        // Hold only the RPC command lock to control the previously unsafe gap between releasing
        // the SIP host's old call and clearing a second current-call pointer in the command owner.
        await Task.Run(() => {
            lock (commandGate) {
                releasing = operations.InvokeAsync("call.release", Args(new { callId = previous }));
                var deadline = Stopwatch.StartNew();
                while (nativeHost.CurrentCall != null && deadline.Elapsed < TimeSpan.FromSeconds(2)) Thread.Sleep(5);
                Check(nativeHost.CurrentCall == null, "original SIP owner releases while command observation is paused");
                dialing = outgoing.DialAsync("sip:fixture@" + endpoint, null, null, 5);
                deadline.Restart();
                while (nativeHost.CurrentCall == null && deadline.Elapsed < TimeSpan.FromSeconds(2)) Thread.Sleep(5);
                incomingId = nativeHost.CurrentCall?.Observation.Id;
                Check(incomingId != null && incomingId != previous, "actual incoming INVITE gains one current host owner");
                var observed = Args(operations.InvokeAsync("status", Args(new { })).GetAwaiter().GetResult());
                Check(observed.GetProperty("call").GetProperty("id").GetString() == incomingId &&
                    observed.GetProperty("call").GetProperty("state").GetString() == "ringing",
                    "command status reads the admitted host call, not a stale parallel pointer");
            }
        });
        Check(Args(await releasing!).GetProperty("callId").GetString() == previous, "old release receipt retains only the original call");
        var current = Args(await operations.InvokeAsync("status", Args(new { })));
        Check(current.GetProperty("call").GetProperty("id").GetString() == incomingId, "late old release cannot erase a new incoming call");
        await operations.InvokeAsync("call.claim", Args(new { callId = incomingId }));
        await operations.InvokeAsync("call.hangup", Args(new { callId = incomingId }));
        await dialing!.WaitAsync(TimeSpan.FromSeconds(5));
        await operations.InvokeAsync("call.release", Args(new { callId = incomingId })); await caller.ReleaseAsync(outgoing);
        Check(nativeHost.CurrentCall == null, "new incoming call remains accessible for original cleanup");
    }
    static async Task NativeIncomingIdentity() {
        int allocations = 0;
        await using var operations = new NativeSipOperations(id => { allocations++; return new CallAudioPort(id, new AudioPermit()); });
        var config = new { binding = new SipBinding("127.0.0.1", 0, "udp"), codecs = new CodecSettings(new[] { "PCMA" }),
            incomingPeers = new[] { "127.0.0.1" }, registration = (object?)null, password = (string?)null };
        var configured = Args(await operations.InvokeAsync("configure", Args(config)));
        string endpoint = configured.GetProperty("endpoint").GetString()!;
        await using var caller = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp"), new CodecSettings(new[] { "PCMA" }), _ => new AudioPort(), _ => false);
        var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString());
        Check(outgoing.Observation.Incoming == null, "outgoing call cannot invent inbound caller evidence");
        var dialing = outgoing.DialAsync("sip:fixture@" + endpoint, "known-caller", null, 5);
        JsonElement call = default;
        var waiting = Stopwatch.StartNew();
        while (waiting.Elapsed < TimeSpan.FromSeconds(3)) {
            var status = Args(await operations.InvokeAsync("status", Args(new { })));
            call = status.GetProperty("call");
            if (call.ValueKind == JsonValueKind.Object) break;
            await Task.Delay(10);
        }
        Check(call.ValueKind == JsonValueKind.Object && allocations == 1, "original incoming identity reaches native service status before answer");
        string id = call.GetProperty("id").GetString()!;
        var identity = call.GetProperty("incoming").Deserialize<SipIncoming>(NativeRpc.Json)!;
        Check(identity.SipCallId == outgoing.Observation.SipCallId && identity.FromUri == "sip:known-caller@" + endpoint &&
            identity.PeerAddress == IPAddress.Loopback.ToString() && identity.PeerPort == caller.LocalEndpoint.Port && identity.Transport == "udp",
            "status retains exact wire call, asserted From and actual transport endpoint");
        await operations.InvokeAsync("call.claim", Args(new { callId = id }));
        var ended = Args(await operations.InvokeAsync("call.hangup", Args(new { callId = id })));
        Check(ended.GetProperty("incoming").GetRawText() == call.GetProperty("incoming").GetRawText(), "hangup cannot replace the original caller context");
        await dialing.WaitAsync(TimeSpan.FromSeconds(5));
        await operations.InvokeAsync("call.release", Args(new { callId = id })); await caller.ReleaseAsync(outgoing);
        var released = Args(await operations.InvokeAsync("status", Args(new { })));
        Check(released.GetProperty("call").ValueKind == JsonValueKind.Null, "released caller context is not assigned to another call");
    }
    static object NativeFixtureConfig() => new { binding = new SipBinding("127.0.0.1", 0, "udp"),
        codecs = new CodecSettings(new[] { "PCMA" }), incomingPeers = Array.Empty<string>(), registration = (object?)null, password = (string?)null };
    static object NativeFixtureAudio(string callId) => new { callId, settings = AudioFixtureSettings,
        desktop = new NativeDesktopIdentity(123, "1", Path.GetFullPath("fixture-desktop.exe"), "Fixture.Package!UI") };
    static async Task NativeAudioCommands() {
        foreach (bool cancelled in new[] { false, true }) {
            int openings = 0; var entered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            var finish = new TaskCompletionSource<ICallAudioRoute>(TaskCreationOptions.RunContinuationsAsynchronously);
            var route = new RouteFixture();
            await using var operations = new NativeSipOperations(id => new CallAudioPort(id, new AudioPermit(), (_, _, permit, _) => {
                openings++; route.Permitted = permit; entered.TrySetResult(true); return finish.Task;
            }));
            string epoch = Guid.NewGuid().ToString(), callId = Guid.NewGuid().ToString(), preparationId = Guid.NewGuid().ToString();
            var rpc = new NativeRpc(epoch, operations.InvokeAsync); long requestId = 0;
            Task<NativeResponse> Send(string method, object args, string? operation = null) => rpc.ExecuteAsync(Request(epoch, ++requestId, method, args, operation));
            Check((await Send("configure", NativeFixtureConfig(), Guid.NewGuid().ToString())).Ok, "composition configures only isolated SIP");
            Check((await Send("call.prepare", new { callId }, Guid.NewGuid().ToString())).Ok, "composition admits original audio owner");
            var initial = await Send("status", new { });
            Check(initial.Result!.Value.GetProperty("media").GetProperty("audio").GetProperty("state").GetString() == "unprepared", "status reports actual unopened call port");
            Check((await Send("call.audio.prepare", NativeFixtureAudio(Guid.NewGuid().ToString()), Guid.NewGuid().ToString())).Error == "runtime_not_ready" && openings == 0,
                "foreign call cannot open the original route");
            var invalid = new { callId, settings = AudioFixtureSettings, desktop = new { pid = 123, startTimeUtcTicks = "9223372036854775808", imagePath = "C:\\fixture.exe", appUserModelId = "Fixture.Package!UI" } };
            Check((await Send("call.audio.prepare", invalid, Guid.NewGuid().ToString())).Error == "invalid_request" && openings == 0, "overflow Desktop identity is rejected before devices");
            var preparing = Send("call.audio.prepare", NativeFixtureAudio(callId), preparationId);
            await entered.Task.WaitAsync(TimeSpan.FromSeconds(2));
            var retry = Send("call.audio.prepare", NativeFixtureAudio(callId), preparationId);
            var status = await Send("heartbeat", new { }).WaitAsync(TimeSpan.FromSeconds(2));
            Check(status.Ok && status.Result!.Value.GetProperty("media").GetProperty("audio").GetProperty("state").GetString() == "preparing",
                "pending device open does not block heartbeat/status");
            if (cancelled) {
                Check((await Send("call.hangup", new { callId }, Guid.NewGuid().ToString())).Ok, "hangup responds during pending original preparation");
                var release = Send("call.release", new { callId }, Guid.NewGuid().ToString());
                await Task.Delay(20); Check(!release.IsCompleted, "release retains pending original cleanup");
                finish.SetResult(route);
                var replies = await Task.WhenAll(preparing, retry);
                Check(replies.All(value => value.Error == "operation_outcome_unknown"), "cancelled possible device effect retains one unknown receipt");
                Check((await release).Ok, "release completes only after late device cleanup");
                Check((await Send("call.audio.prepare", NativeFixtureAudio(callId), preparationId)).Error == "operation_outcome_unknown" && openings == 1,
                    "repeated saved preparation cannot reopen a released call");
            } else {
                finish.SetResult(route);
                var replies = await Task.WhenAll(preparing, retry);
                Check(replies.All(value => value.Ok && value.Result!.Value.GetProperty("state").GetString() == "suspended") && openings == 1 && route.Resumes == 0,
                    "duplicate original receipt returns one suspended preparation, never grants audio");
                Check(!(await Send("call.audio.prepare", NativeFixtureAudio(callId), Guid.NewGuid().ToString())).Ok && openings == 1,
                    "another operation cannot replace prepared devices");
                Check((await Send("call.release", new { callId }, Guid.NewGuid().ToString())).Ok, "release disposes the prepared route");
            }
            Check(route.Disposals == 1 && route.Resumes == 0, "original route disposed exactly once without Voice authority");
            var ended = await Send("status", new { });
            Check(ended.Result!.Value.GetProperty("call").ValueKind == JsonValueKind.Null && ended.Result.Value.GetProperty("media").ValueKind == JsonValueKind.Null,
                "released audio cannot appear on a later status");
        }
    }
    static async Task NativeProductionProcess(string nativeExecutable) {
        Check(Path.IsPathFullyQualified(nativeExecutable) && File.Exists(nativeExecutable), "exact built production DLL required");
        // Real production entrypoint and dependencies, with no test factory/device/hotkey/registration.
        await using (var peer = new PipePeer(nativeExecutable)) {
            var status = await peer.Send("status", new { });
            Check(status.Ok && !status.Result!.Value.GetProperty("configured").GetBoolean(), "production startup has no SIP/audio effects");
            Check((await peer.Send("configure", NativeFixtureConfig(), Guid.NewGuid().ToString())).Ok, "production accepts original local configuration");
            string callId = Guid.NewGuid().ToString();
            Check((await peer.Send("call.prepare", new { callId }, Guid.NewGuid().ToString())).Ok, "production creates actual CallAudioPort");
            status = await peer.Send("status", new { });
            var audio = status.Result!.Value.GetProperty("media").GetProperty("audio");
            Check(audio.GetProperty("callId").GetString() == callId && audio.GetProperty("state").GetString() == "unprepared" && audio.GetProperty("route").ValueKind == JsonValueKind.Null,
                "production media exposes its exact unopened owner, not synthetic PCM");
            Check((await peer.Send("call.hangup", new { callId }, Guid.NewGuid().ToString())).Ok, "production hangup closes original call");
            status = await peer.Send("status", new { });
            Check(status.Result!.Value.GetProperty("media").GetProperty("audio").GetProperty("state").GetString() == "revoked", "production media revokes actual port");
            peer.SendHeartbeats = false;
            await peer.Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(18));
            Check(peer.Process.ExitCode == 0 && (await peer.Process.StandardError.ReadToEndAsync()).Length == 0, "production heartbeat loss completes owned cleanup");
        }
        await using (var peer = new PipePeer(nativeExecutable)) {
            Check((await peer.Send("status", new { })).Ok, "second separate production epoch starts");
            peer.SendHeartbeats = false; peer.Process.StandardInput.Close();
            await peer.Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
            Check(peer.Process.ExitCode == 0, "production EOF closes unconfigured owner");
        }
        var info = new ProcessStartInfo(Environment.ProcessPath!) { UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true };
        info.ArgumentList.Add(nativeExecutable); info.ArgumentList.Add("--ipc-fixture"); info.ArgumentList.Add(Guid.NewGuid().ToString());
        using var invalid = Process.Start(info)!;
        await invalid.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
        Check(invalid.ExitCode == 64 && (await invalid.StandardOutput.ReadToEndAsync()).Length == 0 && (await invalid.StandardError.ReadToEndAsync()).Length == 0,
            "production rejects fixture/unknown CLI mode without effects or argument disclosure");
    }
}
