using Ivy.PhoneBridge;
using NAudio.CoreAudioApi;
using System.Text.Json;
using System.Text.Json.Serialization;

static partial class Program {
    sealed record InstalledVoiceSettings(string ApplicationId, string CaptureEndpointId, NativeVoiceInput VoiceInput);
    sealed record InstalledOwnerRead(string Method, bool Result, double ElapsedMs);
    sealed class ObservedInstalledOwner(IDesktopOwner owner, List<InstalledOwnerRead> reads) : IDesktopOwner {
        private bool Read(string method, Func<bool> read) {
            var elapsed = System.Diagnostics.Stopwatch.StartNew();
            bool result = read();
            // Diagnostic only, after the unchanged production primitive has returned. No new
            // query, cached authority or input; expose which actual boundary refused dispatch.
            if (reads.Count < 128) reads.Add(new(method, result, elapsed.Elapsed.TotalMilliseconds));
            return result;
        }
        public bool IsCurrent() => Read("current", owner.IsCurrent);
        public bool TryFocus() => Read("focus", owner.TryFocus);
        public bool IsFocused() => Read("foreground", owner.IsFocused);
    }
    // Explicit opt-in only, never part of the automatic regression. Real ordinary Desktop input
    // and read-only capture correlation; the Phone audio route is synthetic, with no SIP peer.
    static async Task InstalledVoiceLifetime(string settingsPath) {
        Check(Path.IsPathFullyQualified(settingsPath), "explicit absolute installed Voice settings path required");
        var bytes = File.ReadAllBytes(settingsPath);
        Check(bytes.Length is > 0 and <= 4096, "bounded non-secret installed Voice settings required");
        var options = new JsonSerializerOptions(NativeRpc.Json) { UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow };
        var settings = JsonSerializer.Deserialize<InstalledVoiceSettings>(bytes, options);
        Check(settings?.VoiceInput != null, "explicit configured Voice input required");
        settings!.VoiceInput.Snapshot();
        var observed = new WindowsDesktopApplication(settings.ApplicationId).Observe();
        Check(observed.state == "ready" && observed.identity != null, "one already-running Desktop owner required; no activation/restart");
        var desktop = observed.identity!;
        using (var devices = new MMDeviceEnumerator()) {
            using var endpoint = devices.GetDevice(settings.CaptureEndpointId);
            Check(endpoint.DataFlow == DataFlow.Capture && endpoint.State == DeviceState.Active, "exact configured active capture endpoint required");
        }
        var route = new RouteFixture();
        await using var port = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, permitted, _) => {
            route.Permitted = permitted; return Task.FromResult<ICallAudioRoute>(route);
        });
        await port.PrepareAsync(new(settings.CaptureEndpointId, "unused-synthetic-render", "desktop_process", 20, 10, 60), desktop);
        var baselines = new List<VoiceBaselineObservation>();
        var ownerReads = new List<InstalledOwnerRead>();
        MicroAttachment? attachment = null;
        var clientOperations = new List<object>();
        var usbFailures = new List<string>();
        var captureStates = new List<string>();
        await using var runtime = new PhoneDesktopRuntime(owner: identity => new ObservedInstalledOwner(identity.Owner(), ownerReads),
            microFactory: config => attachment = new MicroAttachment(config, async (arguments, token) => {
                var result = await new MicroUsbClient(config).Run(arguments, token);
                lock (clientOperations) clientOperations.Add(new { command = arguments.Contains("attach") ? "attach" : "port",
                    result.Started, result.Completed, result.ExitCode, result.ErrorCode });
                return result;
            }, diagnostic: value => { lock (usbFailures) if (usbFailures.Count < 16) usbFailures.Add(value); }));
        var session = new CallVoiceSession(port, desktop, settings.VoiceInput, runtime, _ => true,
            observeBaseline: value => { if (baselines.Count < 16) baselines.Add(value); });
        VoiceLifetimeResult? start = null, stop = null; bool heldAuthority = false;
        string? failure = null, cleanupFailure = null;
        var elapsed = System.Diagnostics.Stopwatch.StartNew();
        try {
            start = await session.StartAsync().WaitAsync(TimeSpan.FromSeconds(55));
            if (start.State == "active") {
                heldAuthority = port.IsOpen;
                for (int i = 0; i < 60; i++) {
                    await Task.Delay(250);
                    captureStates.Add(runtime.VoiceCapture(desktop, settings.CaptureEndpointId).State);
                    heldAuthority &= port.IsOpen;
                }
            }
        } catch (Exception error) { failure = error.GetType().Name; }
        finally {
            try { stop = await session.StopAsync().WaitAsync(TimeSpan.FromSeconds(10)); await session.DisposeAsync(); }
            catch (Exception error) { cleanupFailure = error.GetType().Name; }
        }
        object Loaded(System.Reflection.Assembly assembly) => new { path = assembly.Location,
            sha256 = Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(File.ReadAllBytes(assembly.Location))) };
        Console.WriteLine(JsonSerializer.Serialize(new { schemaVersion = 1, actualDesktopInput = true, syntheticPhoneAudio = true,
            sipPeer = false, settings, desktop = new { desktop.pid, desktop.startTimeUtcTicks, desktop.appUserModelId },
            loadedAssemblies = new[] { Loaded(typeof(Program).Assembly), Loaded(typeof(CallVoiceSession).Assembly) },
            start, heldAuthority, stop, micro = attachment?.Status, clientOperations, usbFailures, captureStates, baselines, ownerReads, failure, cleanupFailure, elapsedMs = elapsed.Elapsed.TotalMilliseconds,
            audioRevoked = !port.IsOpen }, NativeRpc.Json));
        Check(failure == null && cleanupFailure == null && start?.State == "active" && heldAuthority && captureStates.Contains("matched") &&
            stop?.State == "stopped" && !port.IsOpen, "actual original Voice start/capture/monitor/stop must all be confirmed; no input retry");
    }
}
