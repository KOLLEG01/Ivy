using Ivy.PhoneBridge;

static partial class Program {
    static MicroSettings FixtureMicroSettings() => new(Path.GetFullPath("usbip.exe"), "sha256:" + new string('0', 64), 3241);
    static NativeVoiceInput FallbackVoiceInput(bool focus) => new(FixtureMicroSettings(), new(["control", "shift"], 32, focus));
    sealed class UnavailableMicroInput : IMicroVoiceInput {
        public MicroAttachmentStatus Status => new("unavailable", null, "micro_client_unavailable");
        public MicroInputReceipt SendMicrophone(bool pressed, long generation, Func<bool> current) => new("not_submitted", pressed, generation);
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
    sealed class ReadyMicroInput : IMicroVoiceInput {
        public MicroAttachmentStatus Status => new("ready", 1, null);
        public MicroInputReceipt SendMicrophone(bool pressed, long generation, Func<bool> current) => new("submitted", pressed, generation);
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
    sealed class LifetimeReader(VoiceLifetimeFixture fixture) : IDesktopVoiceReader {
        public TimedVoiceControlObservation Read() {
            fixture.Threads.Add(Environment.CurrentManagedThreadId); fixture.Reads++;
            return new(fixture.Time.GetTimestamp(), new(fixture.Active ? "active" : "unavailable",
                fixture.Active ? fixture.Identity : null));
        }
        public void Dispose() { fixture.Threads.Add(Environment.CurrentManagedThreadId); fixture.Disposals++; }
    }
    sealed class VoiceLifetimeFixture : IKeyboardInput {
        public readonly VoiceControlIdentity Identity = new("123", [42, 0], [42, 1], [42, 2]);
        public readonly DesktopCaptureIdentity Capture = new("owned-voice", 123, "1");
        public readonly HashSet<int> Threads = [];
        public bool Active, CaptureAbsent, ForeignCapture, PartialStart, RefuseStart, ControlsUnavailable;
        public int Sends, Reads, Disposals, Discoveries, ControlReads, CaptureReads;
        public TimeProvider Time = TimeProvider.System;
        public Action? BeforeCapture;
        public readonly CommandOwner Owner = new();
        public bool IsIdle() { return !RefuseStart; }
        public int Send(KeyStroke[] keys) {
            Threads.Add(Environment.CurrentManagedThreadId); Sends++;
            if (PartialStart) return Sends == 1 ? 1 : keys.Length;
            Active = !Active; return keys.Length;
        }
        public PhoneDesktopRuntime Runtime(CommandApplication? application = null, IMicroVoiceInput? micro = null) => new(application: _ => application ?? new CommandApplication(), owner: _ => Owner, keyboard: () => this,
            controls: (_, labels) => {
                Threads.Add(Environment.CurrentManagedThreadId); ControlReads++;
                if (ControlsUnavailable) return new("unavailable", []);
                return new("observed", [new("123", false, Active ? [
                    new([42, 1], [42, 0], "Mute microphone", true, false, 0),
                    new([42, 2], [42, 0], "Stop voice chat", true, false, null)] :
                    [new([42, 3], [42, 0], "Start voice chat", true, false, null)])]);
            }, captureOwner: (_, endpoint) => {
                CaptureReads++; BeforeCapture?.Invoke();
                bool capturing = Active && !CaptureAbsent;
                return new(endpoint, capturing ? "matched" : "none", capturing ? (ForeignCapture ? Capture with { InstanceId = "foreign" } : Capture) : null);
            }, voiceReader: _ => { Discoveries++; return new LifetimeReader(this); }, microFactory: _ => micro ?? new UnavailableMicroInput(), exited: _ => !Owner.Current);
    }
    static async Task VoiceControlUnits() {
        var microFixture = new VoiceLifetimeFixture();
        var microClock = new Clock(); microFixture.Time = microClock;
        microFixture.BeforeCapture = () => microClock.Ticks += 100;
        var micro = new LifetimeMicro(microFixture, "submitted");
        await using var microPort = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(microClock), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        await microPort.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using (var microRuntime = microFixture.Runtime(micro: micro)) {
            await using var microSession = new CallVoiceSession(microPort, CommandIdentity(), new(FixtureMicroSettings()), microRuntime, _ => true, microClock);
            var microStarted = await microSession.StartAsync().WaitAsync(TimeSpan.FromSeconds(3));
            Check(microStarted.State == "active" && (microStarted.Dispatch as VoiceDispatchResult)?.Method == "micro",
                "Codex Micro controls Realtime Voice without Accessibility");
            microFixture.CaptureAbsent = true;
            microFixture.BeforeCapture = () => microClock.Ticks += 10000;
            await Task.Delay(1100);
            Check(microPort.IsOpen, "a temporary missing Windows capture does not hang up the owning SIP call");
            var microStopped = await microSession.StopAsync().WaitAsync(TimeSpan.FromSeconds(3));
            Check(microStopped.State == "stopped" && !microFixture.Active &&
                micro.Events.SequenceEqual(new[] { true, false, true, false, true, false }),
                "Micro starts once and ends Voice even after its audio capture disappeared");
        }

        var fallbackCleanupFixture = new VoiceLifetimeFixture();
        var fallbackCleanupClock = new Clock(); fallbackCleanupFixture.Time = fallbackCleanupClock;
        fallbackCleanupFixture.BeforeCapture = () => fallbackCleanupClock.Ticks += 1000;
        var stuckMicro = new LifetimeMicro(fallbackCleanupFixture, "stuck-stop");
        await using var fallbackCleanupPort = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(fallbackCleanupClock), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        await fallbackCleanupPort.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using (var fallbackCleanupRuntime = fallbackCleanupFixture.Runtime(micro: stuckMicro)) {
            await using var fallbackCleanupSession = new CallVoiceSession(fallbackCleanupPort, CommandIdentity(), FallbackVoiceInput(false),
                fallbackCleanupRuntime, _ => true, fallbackCleanupClock);
            Check((await fallbackCleanupSession.StartAsync().WaitAsync(TimeSpan.FromSeconds(3))).State == "active",
                "Micro start establishes the original capture before cleanup fallback");
            var ended = await fallbackCleanupSession.StopAsync().WaitAsync(TimeSpan.FromSeconds(4));
            Check(ended.State == "stopped" && !fallbackCleanupFixture.Active && fallbackCleanupFixture.Sends == 1 &&
                stuckMicro.Events.SequenceEqual(new[] { true, false, true, false, true, false }),
                "an ineffective submitted Micro stop falls back once to the configured hotkey for the unchanged owned capture");
        }

        var dictationFixture = new VoiceLifetimeFixture();
        var dictation = new LifetimeMicro(dictationFixture, "dictation");
        await using var dictationPort = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        await dictationPort.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using (var dictationRuntime = dictationFixture.Runtime(micro: dictation)) {
            await using var dictationSession = new CallVoiceSession(dictationPort, CommandIdentity(), FallbackVoiceInput(false),
                dictationRuntime, _ => true);
            var dictationStarted = await dictationSession.StartAsync().WaitAsync(TimeSpan.FromSeconds(3));
            Check(dictationStarted.State == "active" && (dictationStarted.Dispatch as VoiceDispatchResult)?.Method == "hotkey" &&
                (dictationStarted.Dispatch as VoiceDispatchResult)?.Micro?.Phase == "submitted" && dictationFixture.Sends == 1,
                "exact Micro lighting rejects Dictation and permits one configured Voice fallback");
            var dictationStopped = await dictationSession.StopAsync().WaitAsync(TimeSpan.FromSeconds(3));
            Check(dictationStopped.State == "stopped" && dictationFixture.Sends == 2 &&
                dictation.Events.SequenceEqual(new[] { true, false, true, false }),
                "a Dictation-routed Micro gesture is not reused to end fallback Voice");
        }

        var ignoredMicroFixture = new VoiceLifetimeFixture();
        var ignoredMicroClock = new Clock(); ignoredMicroFixture.Time = ignoredMicroClock;
        ignoredMicroFixture.BeforeCapture = () => ignoredMicroClock.Ticks += 1000;
        var ignoredMicro = new LifetimeMicro(ignoredMicroFixture, "submitted-no-effect");
        await using var ignoredMicroPort = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(ignoredMicroClock), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        await ignoredMicroPort.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using (var ignoredMicroRuntime = ignoredMicroFixture.Runtime(micro: ignoredMicro)) {
            await using var ignoredMicroSession = new CallVoiceSession(ignoredMicroPort, CommandIdentity(), new(FixtureMicroSettings()),
                ignoredMicroRuntime, _ => true, ignoredMicroClock);
            var ignoredMicroStarted = await ignoredMicroSession.StartAsync().WaitAsync(TimeSpan.FromSeconds(6));
            Check(ignoredMicroStarted.State == "outcome_unknown" &&
                (ignoredMicroStarted.Dispatch as VoiceDispatchResult)?.Method == "micro" &&
                ignoredMicro.Events.SequenceEqual(new[] { true, false, true, false, true, false }),
                "a Micro gesture with no captured Voice effect fails and cleans up through Micro only");
        }

        var fixture = new VoiceLifetimeFixture();
        await using var port = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        await port.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using var runtime = fixture.Runtime();
        await using var session = new CallVoiceSession(port, CommandIdentity(), FallbackVoiceInput(false), runtime, _ => true);
        var started = await session.StartAsync().WaitAsync(TimeSpan.FromSeconds(3));
        Check(started.State == "active" && (started.Dispatch as VoiceDispatchResult)?.Method == "hotkey" && port.IsOpen,
            "already matched capture starts without using Accessibility");
        var stopped = await session.StopAsync().WaitAsync(TimeSpan.FromSeconds(3));
        Check(stopped.State == "stopped" && fixture.Sends == 2 && !fixture.Active && !port.IsOpen,
            "the matching explicit fallback ends Voice without Accessibility");

        var missingCapture = new VoiceLifetimeFixture();
        await using var missingPort = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        await missingPort.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using var missingRuntime = missingCapture.Runtime();
        var missingSession = new CallVoiceSession(missingPort, CommandIdentity(), FallbackVoiceInput(false), missingRuntime, _ => true);
        var pending = missingSession.StartAsync();
        await Task.Delay(100);
        missingCapture.CaptureAbsent = true;
        var missingStopped = await missingSession.StopAsync().WaitAsync(TimeSpan.FromSeconds(3));
        Check(missingStopped.State == "stopped" && missingCapture.Sends == 2 && !missingCapture.Active,
            "cleanup uses the configured input even when capture correlation disappeared");
        await missingSession.DisposeAsync();
    }
    static async Task VoiceLifetimeUnits() {
        int microAttempts = 0;
        await using (var rolloutRuntime = new PhoneDesktopRuntime(microFactory: _ => {
            if (++microAttempts == 1) throw new System.Net.Sockets.SocketException((int)System.Net.Sockets.SocketError.AddressAlreadyInUse);
            return new ReadyMicroInput();
        })) {
            rolloutRuntime.PrepareMicro(FixtureMicroSettings());
            Check(rolloutRuntime.MicroStatus.ErrorCode == "micro_attachment_conflict", "overlapping rollout reports the temporary Micro owner");
            await Task.Delay(1100);
            Check(rolloutRuntime.MicroStatus.State == "ready" && microAttempts == 2, "Micro attaches after the previous rollout owner exits");
        }
        await VoiceGenerationTransitionUnits();
        foreach (string scenario in new[] { "normal", "existing", "adopt", "foreign", "owner-exited", "refused", "partial" }) {
            var fixture = new VoiceLifetimeFixture { ControlsUnavailable = true, Active = scenario is "existing" or "adopt",
                RefuseStart = scenario == "refused", PartialStart = scenario == "partial" };
            var clock = new Clock(); fixture.Time = clock; fixture.BeforeCapture = () => clock.Ticks += 650;
            await using var port = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(clock), (_, _, allowed, _) =>
                Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
            await port.PrepareAsync(AudioFixtureSettings, CommandIdentity());
            var session = new CallVoiceSession(port, CommandIdentity(), FallbackVoiceInput(false) with { AdoptExisting = scenario == "adopt" }, fixture.Runtime(), _ => true, clock);
            var result = await session.StartAsync().WaitAsync(TimeSpan.FromSeconds(3));
            if (scenario is "normal" or "adopt" or "foreign" or "owner-exited") {
                Check(result.State == "active" && port.IsOpen, "capture correlation starts Voice without UI controls or a 500ms query deadline");
                clock.Ticks += 30000;
                Check(port.IsOpen, "an owned active call does not expire between observations");
            } else Check(result.State is "not_started" or "outcome_unknown", "refused or uncertain input never claims connected audio");
            if (scenario == "existing") Check(result.Reason == "capture_not_absent" && fixture.Sends == 0, "unowned existing capture is untouched");
            if (scenario == "foreign") fixture.ForeignCapture = true;
            if (scenario == "owner-exited") fixture.Owner.Current = false;
            var ended = await session.StopAsync().WaitAsync(TimeSpan.FromSeconds(3));
            Check(!port.IsOpen && fixture.ControlReads == 0 && fixture.Discoveries == 0, "hangup closes audio immediately; accessibility is never queried");
            if (scenario == "foreign") {
                Check(ended.State == "outcome_unknown" && fixture.Sends == 1, "replacement capture never receives a stop gesture");
                await ThrowsAsync<InvalidOperationException>(session.DisposeAsync().AsTask(), "unknown Voice cleanup is retained");
            } else {
                Check(ended.State is "stopped" or "not_started", "owned Voice or exited process releases cleanup");
                await session.DisposeAsync();
                if (scenario == "normal") Check(fixture.Sends == 2 && !fixture.Active, "one start and one stop");
                if (scenario == "adopt") Check(fixture.Sends == 1 && !fixture.Active, "explicit adoption sends only stop");
                if (scenario == "owner-exited") Check(fixture.Sends == 1, "process exit does not send input to a replacement");
            }
        }
        var blocked = new VoiceLifetimeFixture(); using var entered = new ManualResetEventSlim(); using var release = new ManualResetEventSlim();
        blocked.BeforeCapture = () => { entered.Set(); release.Wait(TimeSpan.FromSeconds(3)); };
        await using var blockedPort = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        await blockedPort.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using var blockedVoice = new CallVoiceSession(blockedPort, CommandIdentity(), FallbackVoiceInput(false), blocked.Runtime(), _ => true);
        var pending = blockedVoice.StartAsync();
        Check(await Task.Run(() => entered.Wait(TimeSpan.FromSeconds(2))), "capture query entered");
        var ending = blockedVoice.StopAsync();
        Check(!blockedPort.IsOpen && !ending.IsCompleted, "hangup revokes immediately and joins in-flight capture");
        release.Set();
        Check((await pending).State == "not_started" && (await ending).State == "not_started" && blocked.Sends == 0,
            "cancellation cannot dispatch late input");
    }
    static async Task VoiceGenerationTransitionUnits() {
        var fixture = new VoiceLifetimeFixture(); var route = new RouteFixture(); int preparations = 0;
        await using var port = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) => {
            preparations++; route.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(route);
        });
        await port.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using var runtime = fixture.Runtime();
        var first = new CallVoiceSession(port, CommandIdentity(), FallbackVoiceInput(false), runtime, _ => true);
        Check((await first.StartAsync()).State == "active" && port.IsOpen, "first Voice generation opens its own fresh binding");
        Check((await first.PauseForTransitionAsync()).State == "stopped" && !port.IsOpen && !fixture.Active,
            "transition positively ends Voice while leaving call devices suspended");
        await first.RetireForTransitionAsync();
        await using var second = new CallVoiceSession(port, CommandIdentity(), FallbackVoiceInput(false), runtime, _ => true);
        Check((await second.StartAsync()).State == "active" && port.IsOpen && preparations == 1,
            "new Voice generation obtains fresh authority on the same prepared SIP audio route");
        await first.StopAsync(); await first.DisposeAsync();
        Check(port.IsOpen, "retired Voice cleanup cannot revoke a replacement generation");
        Check((await second.StopAsync()).State == "stopped" && port.Status.State == "revoked" && fixture.Sends == 4,
            "final call cleanup permanently revokes audio after exactly two starts and stops");
        var uncertain = new VoiceLifetimeFixture();
        await using var uncertainPort = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed }));
        await uncertainPort.PrepareAsync(AudioFixtureSettings, CommandIdentity());
        await using var uncertainRuntime = uncertain.Runtime();
        var uncertainVoice = new CallVoiceSession(uncertainPort, CommandIdentity(), FallbackVoiceInput(false), uncertainRuntime, _ => true);
        Check((await uncertainVoice.StartAsync()).State == "active", "uncertain transition begins from an owned active Voice");
        uncertain.ForeignCapture = true;
        Check((await uncertainVoice.PauseForTransitionAsync()).State == "outcome_unknown" && !uncertainPort.IsOpen,
            "changed capture ownership leaves transition unknown and audio closed");
        await ThrowsAsync<InvalidOperationException>(uncertainVoice.RetireForTransitionAsync(), "unknown transition cannot release authority for another generation");
        await ThrowsAsync<InvalidOperationException>(uncertainVoice.DisposeAsync().AsTask(), "unknown transition still fences original cleanup");
    }
    static async Task VoiceLifetimeNativeUnits() {
        var fixture = new VoiceLifetimeFixture();
        var operations = new NativeSipOperations(id => new CallAudioPort(id, new AudioPermit(), (_, _, allowed, _) =>
            Task.FromResult<ICallAudioRoute>(new RouteFixture { Permitted = allowed })), fixture.Runtime());
        string epoch = Guid.NewGuid().ToString(), callId = Guid.NewGuid().ToString(); long sequence = 0;
        var rpc = new NativeRpc(epoch, operations.InvokeAsync);
        Task<NativeResponse> Send(string method, object args) => rpc.ExecuteAsync(Request(epoch, ++sequence, method, args, Guid.NewGuid().ToString()));
        try {
            Check((await Send("configure", NativeFixtureConfig())).Ok, "isolated native SIP host configured");
            Check((await Send("call.prepare", new { callId })).Ok, "original call prepared");
            Check((await Send("call.audio.prepare", new { callId, settings = AudioFixtureSettings,
                desktop = new NativeDesktopIdentity(123, "1", Path.GetFullPath("fixture-desktop.exe"), "Fixture.Package!UI") })).Ok,
                "original synthetic audio prepared");
            Check((await Send("call.desktop.startVoice", CommandHotkey(callId))).Result!.Value.GetProperty("result").GetProperty("state").GetString() == "active",
                "native start owns the live lifetime");
            fixture.ForeignCapture = true;
            var ended = await Send("call.desktop.stopVoice", CommandHotkey(callId));
            Check(ended.Ok && ended.Result!.Value.GetProperty("result").GetProperty("state").GetString() == "outcome_unknown" && fixture.Sends == 1,
                "unknown foreign Voice cleanup is retained without another input");
            Check((await Send("call.release", new { callId })).Error == "operation_outcome_unknown", "unknown Voice cleanup cannot release native call owner");
            Check((await Send("call.prepare", new { callId = Guid.NewGuid().ToString() })).Error == "operation_conflict", "replacement call remains fenced after failed cleanup");
        } finally {
            await ThrowsAsync<InvalidOperationException>(operations.DisposeAsync().AsTask(), "shutdown retains original failed Voice cleanup");
        }
    }
}
