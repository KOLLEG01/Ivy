using Ivy.PhoneBridge;

static partial class Program {
    sealed class LifetimeMicro(VoiceLifetimeFixture fixture, string phase) : IMicroVoiceInput {
        public readonly List<bool> Events = [];
        public long Generation = 1;
        public long DictationSequence;
        public MicroAttachmentStatus Status => new("ready", Generation, null);
        public MicroInputReceipt SendMicrophone(bool pressed, long generation, Func<bool> original) {
            if (generation != Generation || !original()) return new("not_submitted", pressed, generation);
            Events.Add(pressed);
            if (phase is "submitted" or "stuck-stop") {
                if (pressed && Events.Count % 6 == 3) fixture.Active = true;
                if (pressed && Events.Count % 6 == 5 && phase != "stuck-stop") fixture.Active = false;
            } else if (phase == "dictation" && !pressed) { DictationSequence++; fixture.Active = false; }
            return new(phase is "submitted-no-effect" or "stuck-stop" or "dictation" ? "submitted" : phase, pressed, generation);
        }
        public MicroVoiceObservation ObserveVoice(long generation) => new(
            generation == Generation ? (DictationSequence == 0 ? "other" : "dictation_recording") : "unavailable",
            generation, DictationSequence);
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
    static async Task MicroVoiceLifetimeUnits() {
        var serializedHotkey = System.Text.Json.JsonSerializer.SerializeToElement(
            new VoiceDispatchResult("submitted", "hotkey", new HotkeyResult("submitted", 8, 8, true, null),
                new MicroGestureResult("submitted", new("submitted", true, 4294967296), new("submitted", false, 4294967296))),
            NativeRpc.Json);
        var serializedResult = serializedHotkey.GetProperty("hotkey");
        Check(serializedResult.GetProperty("phase").GetString() == "submitted" &&
            serializedResult.GetProperty("requested").GetInt32() == 8 && serializedResult.GetProperty("submitted").GetInt32() == 8 &&
            serializedResult.GetProperty("keyUpSubmitted").GetBoolean() && serializedResult.GetProperty("errorCode").ValueKind == System.Text.Json.JsonValueKind.Null,
            "fallback receipt serializes the complete submitted hotkey result");
        await MicroVoiceTransitionUnits();
        foreach (var scenario in new[] { "primary", "disabled-fallback", "refused-fallback", "unknown-no-fallback" }) {
            var fixture = new VoiceLifetimeFixture();
            var micro = new LifetimeMicro(fixture, scenario switch {
                "refused-fallback" => "not_submitted", "unknown-no-fallback" => "outcome_unknown", _ => "submitted"
            });
            IMicroVoiceInput selected = scenario == "disabled-fallback" ? new UnavailableMicroInput() : micro;
            var input = new NativeVoiceInput(FixtureMicroSettings(),
                scenario is "refused-fallback" or "unknown-no-fallback" ? FallbackVoiceInput(false).HotkeyFallback : null);
            await using var runtime = fixture.Runtime(micro: selected);
            await using var port = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) =>
                Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
            await port.PrepareAsync(AudioFixtureSettings, CommandIdentity());
            var session = new CallVoiceSession(port, CommandIdentity(), input, runtime, _ => true);
            var started = await session.StartAsync().WaitAsync(TimeSpan.FromSeconds(3));
            if (scenario is "primary" or "refused-fallback") {
                Check(started.State == "active" && port.IsOpen, "preferred input still requires actual original Voice/audio correlation");
                if (scenario == "primary") micro.Generation++;
                var ended = await session.StopAsync().WaitAsync(TimeSpan.FromSeconds(3));
                Check(ended.State == "stopped" && !fixture.Active && !port.IsOpen, "original capture permits a fresh stop gesture after HID reconnect; explicit fallback is retained");
                Check(fixture.Sends == (scenario == "primary" ? 0 : 2), "keyboard is used only for explicit known-no-effect fallback");
                Check(micro.Events.SequenceEqual(scenario == "primary" ? new[] { true, false, true, false, true, false } : new[] { true }),
                    "Micro latches Voice and cleans up once; refused Micro does not receive a release");
            } else {
                Check(started.State == (scenario == "disabled-fallback" ? "not_started" : "outcome_unknown") && !port.IsOpen,
                    "unavailable or unknown input cannot grant audio authority");
                Check(fixture.Sends == 0, "disabled fallback and unknown Micro effect never insert a hotkey");
                Check(micro.Events.Count == (scenario == "disabled-fallback" ? 0 : 2), "unknown Micro start gets one explicit end-call cleanup attempt");
            }
            await session.DisposeAsync();
        }
        {
            var fixture = new VoiceLifetimeFixture(); var micro = new LifetimeMicro(fixture, "stuck-stop");
            await using var runtime = fixture.Runtime(micro: micro);
            await using var port = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) =>
                Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
            await port.PrepareAsync(AudioFixtureSettings, CommandIdentity());
            var session = new CallVoiceSession(port, CommandIdentity(), new NativeVoiceInput(FixtureMicroSettings()), runtime, _ => true);
            Check((await session.StartAsync().WaitAsync(TimeSpan.FromSeconds(3))).State == "active", "original Voice capture starts");
            Check((await session.StopAsync().WaitAsync(TimeSpan.FromSeconds(8))).State == "outcome_unknown", "slow Voice stop retains its original receipt");
            var release = session.DisposeAsync().AsTask();
            await Task.Delay(150); fixture.Active = false;
            await release;
            Check(micro.Events.Count == 6 && !port.IsOpen, "later absent capture releases the owner without a second Micro gesture");
        }
        await MicroDispatcherUnits();
    }
    static async Task MicroVoiceTransitionUnits() {
        var fixture = new VoiceLifetimeFixture(); var micro = new LifetimeMicro(fixture, "submitted");
        await using var runtime = fixture.Runtime(micro: micro);
        string callId = Guid.NewGuid().ToString();
        await using var audio = new CallAudioPort(callId, new AudioPermit(), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        var identity = CommandIdentity(); await audio.PrepareAsync(AudioFixtureSettings, identity);
        var desktop = new NativeDesktopIdentity(identity.pid, identity.startTimeUtcTicks.ToString(), identity.imagePath, identity.appUserModelId);
        await using var session = new CallDesktopSession(callId, runtime, _ => true, audio);
        var voiceInput = new NativeVoiceInput(FixtureMicroSettings(), null);
        System.Text.Json.JsonElement Args(int? generation = null) => System.Text.Json.JsonSerializer.SerializeToElement(
            generation == null ? (object)new { callId, desktop, voiceInput } : new { callId, desktop, voiceInput, generation }, NativeRpc.Json);
        await session.Invoke("call.desktop.startVoice", Args());
        for (int generation = 0; generation < 2; generation++) {
            await session.Invoke("call.desktop.pauseVoice", Args(generation));
            Check(!audio.IsOpen && !fixture.Active, "Micro pause leaves SIP route suspended before archival");
            await session.Invoke("call.desktop.resumeVoice", Args(generation + 1));
            Check(audio.IsOpen && fixture.Active, "Micro restart binds fresh Voice authority without replacing call devices");
            bool refused = false;
            try { await session.Invoke("call.desktop.resumeVoice", Args(generation + 1)); }
            catch (NativeRpcException error) { refused = error.Code == "operation_conflict"; }
            Check(refused, "replayed Voice generation cannot dispatch a second gesture");
        }
        await session.Invoke("call.desktop.stopVoice", Args());
        Check(fixture.Sends == 0 && micro.Events.Count == 18 && !audio.IsOpen,
            "three starts and stops use only Micro with final audio revocation");
    }
    static async Task MicroDispatcherUnits() {
        foreach (bool unknown in new[] { false, true }) {
            var fixture = new VoiceLifetimeFixture();
            var micro = new LifetimeMicro(fixture, unknown ? "outcome_unknown" : "submitted");
            var operations = new NativeSipOperations(id => new CallAudioPort(id, new AudioPermit(), (_, _, allowed, _) =>
                Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed })), fixture.Runtime(micro: micro));
            string epoch = Guid.NewGuid().ToString(), callId = Guid.NewGuid().ToString(); long sequence = 0;
            var rpc = new NativeRpc(epoch, operations.InvokeAsync);
            Task<NativeResponse> Send(string method, object args) => rpc.ExecuteAsync(Request(epoch, ++sequence, method, args, Guid.NewGuid().ToString()));
            var desktop = new NativeDesktopIdentity(123, "1", Path.GetFullPath("fixture-desktop.exe"), "Fixture.Package!UI");
            var voiceInput = new NativeVoiceInput(FixtureMicroSettings(), unknown ? FallbackVoiceInput(false).HotkeyFallback : null);
            var input = new { callId, desktop, voiceInput };
            var configuration = new NativeSipConfiguration(new("127.0.0.1", 0, "udp"), new(["PCMA"]), [], null, null, FixtureMicroSettings());
            Check((await Send("configure", configuration)).Ok, "Micro dispatcher configured");
            var readiness = await operations.InvokeAsync("status", System.Text.Json.JsonSerializer.SerializeToElement(new { }, NativeRpc.Json));
            Check(System.Text.Json.JsonSerializer.SerializeToElement(readiness, NativeRpc.Json).GetProperty("micro").GetProperty("state").GetString() == "ready" && micro.Events.Count == 0,
                "configuration prepares Micro and status exposes readiness before any call or input");
            Check((await Send("call.prepare", new { callId })).Ok, "Micro dispatcher retains original call");
            Check((await Send("call.audio.prepare", new { callId, settings = AudioFixtureSettings, desktop })).Ok, "Micro dispatcher audio prepared");
            var started = await Send("call.desktop.startVoice", input);
            Check(started.Ok && started.Result!.Value.GetProperty("result").GetProperty("state").GetString() == (unknown ? "outcome_unknown" : "active"), "production dispatcher selects Micro");
            Check(fixture.Sends == 0 && micro.Events.Count == (unknown ? 2 : 4),
                "production dispatcher uses Micro and never falls back after an uncertain effect");
            Check((await Send("call.desktop.startVoice", input)).Error == "operation_conflict", "a second operation cannot start the same call again");
            var stopped = await Send("call.desktop.stopVoice", input);
            Check(stopped.Ok && fixture.Sends == 0, "production cleanup retains Micro selection");
            if (!unknown) Check(micro.Events.SequenceEqual(new[] { true, false, true, false, true, false }),
                "production starts and stops through Micro exactly once each");
            await operations.DisposeAsync();
        }
    }
}
