using Ivy.PhoneBridge;

static partial class Program {
    sealed class CommandCaptureEndpoint(string id, bool active) : ICaptureEndpoint {
        public string Id => id;
        public bool IsActiveCapture => active;
        public CaptureSession[] ReadSessions() => new[] { new CaptureSession("capture-owned", 1, 123, 0),
            new CaptureSession("capture-shared", 1, 321, 0x0889000d) };
        public void Dispose() { }
    }
    sealed class CommandProcessSnapshot(DesktopIdentity root) : IAudioProcessSnapshot {
        public AudioProcessRecord Read(int pid) => pid == root.pid ? new(pid, 0, 1, 1, root.imagePath) :
            new(pid, 0, 1, 1, Path.GetFullPath("foreign.exe"));
        public void Dispose() { }
    }
    static async Task CaptureCommands() {
        bool available = true; int captures = 0, processes = 0;
        var desktop = new PhoneDesktopRuntime(capture: id => { captures++; return CaptureObservation.Read(id, endpoint => new CommandCaptureEndpoint(endpoint, available)); },
            process: (root, pid) => { processes++; return AudioProcessOwnership.Read(root, pid, () => available, () => new CommandProcessSnapshot(root)); });
        await using var operations = new NativeSipOperations(_ => throw new Exception("No SIP/audio allocation permitted by reads."), desktop);
        string epoch = Guid.NewGuid().ToString(); long sequence = 0;
        var rpc = new NativeRpc(epoch, operations.InvokeAsync, 1);
        Task<NativeResponse> Send(string method, object args) => rpc.ExecuteAsync(Request(epoch, ++sequence, method, args));
        var capture = await Send("desktop.capture", new { endpointId = "fixture-capture" });
        Check(capture.Ok && capture.Result!.Value.GetProperty("endpointId").GetString() == "fixture-capture" &&
            capture.Result.Value.GetProperty("sessions")[0].GetProperty("singleProcess").GetBoolean() &&
            !capture.Result.Value.GetProperty("sessions")[1].GetProperty("singleProcess").GetBoolean(),
            "capture IPC retains exact endpoint, session state and shared-process uncertainty");
        var identity = new NativeDesktopIdentity(123, "1", Path.GetFullPath("fixture-desktop.exe"), "Fixture.Package!UI");
        var owned = await Send("desktop.process", new { desktop = identity, pid = 123 });
        Check(owned.Ok && owned.Result!.Value.GetProperty("pid").GetInt32() == 123 && owned.Result.Value.GetProperty("state").GetString() == "owned", "process IPC retains actual ancestry result");
        var foreign = await Send("desktop.process", new { desktop = identity, pid = 321 });
        Check(foreign.Ok && foreign.Result!.Value.GetProperty("state").GetString() == "foreign", "process IPC cannot upgrade a foreign executable");
        available = false;
        capture = await Send("desktop.capture", new { endpointId = "fixture-capture" });
        Check(capture.Ok && capture.Result!.Value.GetProperty("state").GetString() == "unavailable" && capture.Result.Value.GetProperty("sessions").GetArrayLength() == 0,
            "unavailable capture does not replay old active sessions");
        owned = await Send("desktop.process", new { desktop = identity, pid = 123 });
        Check(owned.Ok && owned.Result!.Value.GetProperty("state").GetString() == "unavailable", "unavailable process observation does not replay old ownership");
        Check((await Send("desktop.capture", new { endpointId = "" })).Error == "invalid_request" && captures == 2, "invalid endpoint rejected before observation");
        Check((await Send("desktop.process", new { desktop = identity, pid = 0 })).Error == "invalid_request" && processes == 3, "invalid process rejected before observation");
        var status = await Send("status", new { });
        Check(status.Ok && !status.Result!.Value.GetProperty("configured").GetBoolean(), "reads need no SIP configuration or effect receipt capacity");
    }
    sealed class CommandApplication : IDesktopApplication {
        public int Observations, Activations;
        public Action? BeforeObserve;
        public bool Absent = true;
        public DesktopObservation Observe() {
            Observations++; BeforeObserve?.Invoke();
            return DesktopObservation.Select(!Absent, Absent ? Array.Empty<DesktopIdentity>() : new[] { CommandIdentity() });
        }
        public int Activate() { Activations++; return 123; }
    }
    sealed class CommandOwner : IDesktopOwner {
        public bool Current = true, Focused, FocusSucceeds = true;
        public int FocusAttempts;
        public Action? BeforeFocus = null, BeforeCheck = null;
        public bool IsCurrent() { BeforeCheck?.Invoke(); return Current; }
        public bool TryFocus() { FocusAttempts++; BeforeFocus?.Invoke(); if (FocusSucceeds) Focused = true; return FocusSucceeds; }
        public bool IsFocused() => Focused;
    }
    sealed class CommandKeyboard(CommandOwner owner) : IKeyboardInput {
        public int Sends;
        public bool RequireFocus, Partial;
        public bool IsIdle() => true;
        public int Send(KeyStroke[] keys) {
            Check(!RequireFocus || owner.Focused, "configured focus precedes any input");
            Sends++; return Partial && Sends == 1 ? 1 : keys.Length;
        }
    }
    static DesktopIdentity CommandIdentity() => new(123, 1, Path.GetFullPath("fixture-desktop.exe"), "Fixture.Package!UI");
    static object CommandHotkey(string callId, bool focus = true) => new { callId,
        desktop = new NativeDesktopIdentity(123, "1", Path.GetFullPath("fixture-desktop.exe"), "Fixture.Package!UI"),
        voiceInput = FallbackVoiceInput(focus) };
    static async Task DesktopCommands() {
        var supervised = new CommandApplication();
        await using (var operations = new NativeSipOperations(_ => throw new Exception("Desktop supervision cannot allocate SIP/audio."), new PhoneDesktopRuntime(_ => supervised))) {
            string epoch = Guid.NewGuid().ToString(); long sequence = 0; var rpc = new NativeRpc(epoch, operations.InvokeAsync);
            Task<NativeResponse> Send(string method, object args, string? id = null) => rpc.ExecuteAsync(Request(epoch, ++sequence, method, args, id));
            string operationId = Guid.NewGuid().ToString(); var application = new NativeDesktopApplication("Fixture.Package!UI", true);
            var launch = await Send("desktop.launch", application, operationId);
            Check(launch.Ok && launch.Result!.Value.GetProperty("phase").GetString() == "submitted" && supervised.Activations == 1,
                "idle Desktop supervision activates the configured packaged application");
            Check((await Send("desktop.launch", application, operationId)).Ok && supervised.Activations == 1,
                "retrying one retained idle activation does not activate twice");
            supervised.Absent = false;
            Check((await Send("desktop.launch", application, Guid.NewGuid().ToString())).Result!.Value.GetProperty("phase").GetString() == "ready" && supervised.Activations == 1,
                "idle supervision observes an existing Desktop without activating it");
        }
        var ui = new CommandApplication(); var owner = new CommandOwner(); var keyboard = new CommandKeyboard(owner) { RequireFocus = true };
        var lifetime = new VoiceLifetimeFixture();
        var desktop = lifetime.Runtime(ui);
        await using (var operations = new NativeSipOperations(id => new CallAudioPort(id, new AudioPermit(), (_, _, allowed, _) => {
            var route = new RouteFixture { Permitted = allowed }; return Task.FromResult<ICallAudioRoute>(route);
        }), desktop)) {
            string epoch = Guid.NewGuid().ToString(), callId = Guid.NewGuid().ToString(); long sequence = 0;
            var rpc = new NativeRpc(epoch, operations.InvokeAsync);
            Task<NativeResponse> Send(string method, object args, string? id = null) => rpc.ExecuteAsync(Request(epoch, ++sequence, method, args, id));
            var observed = await Send("desktop.observe", new { appUserModelId = "Fixture.Package!UI" });
            Check(observed.Ok && observed.Result!.Value.GetProperty("state").GetString() == "absent" && ui.Activations == 0 && keyboard.Sends == 0,
                "native read-only Desktop observation cannot activate or send input");
            await Send("configure", NativeFixtureConfig(), Guid.NewGuid().ToString());
            await Send("call.prepare", new { callId }, Guid.NewGuid().ToString());
            Check((await Send("call.audio.prepare", new { callId, settings = AudioFixtureSettings,
                desktop = new NativeDesktopIdentity(123, "1", Path.GetFullPath("fixture-desktop.exe"), "Fixture.Package!UI") }, Guid.NewGuid().ToString())).Ok,
                "guarded Voice uses the original prepared audio target");
            string launchId = Guid.NewGuid().ToString(); var launchArgs = new { callId, application = new NativeDesktopApplication("Fixture.Package!UI", true) };
            var launch = await Send("call.desktop.launch", launchArgs, launchId);
            Check(launch.Ok && launch.Result!.Value.GetProperty("callId").GetString() == callId &&
                launch.Result.Value.GetProperty("result").GetProperty("phase").GetString() == "submitted" && ui.Activations == 1,
                "ordinary activation result retains the original call and submission only");
            Check((await Send("call.desktop.launch", launchArgs, launchId)).Ok && ui.Activations == 1, "original launch receipt does not activate twice");
            Check((await Send("call.desktop.launch", launchArgs, Guid.NewGuid().ToString())).Error == "operation_conflict", "new launch operation cannot repeat same call activation");
            var started = await Send("call.desktop.startVoice", CommandHotkey(callId), Guid.NewGuid().ToString());
            Check(started.Ok && started.Result!.Value.GetProperty("result").GetProperty("state").GetString() == "active" && lifetime.Owner.FocusAttempts == 1 && lifetime.Sends == 1,
                "start chord requires configured focus and positively correlated own Voice before active receipt");
            await Send("call.hangup", new { callId }, Guid.NewGuid().ToString());
            var stopped = await Send("call.desktop.stopVoice", CommandHotkey(callId), Guid.NewGuid().ToString());
            Check(stopped.Ok && stopped.Result!.Value.GetProperty("result").GetProperty("state").GetString() == "stopped" && lifetime.Sends == 2,
                "original retained call verifies its own stop and idle after SIP hangup");
            Check((await Send("call.desktop.stopVoice", CommandHotkey(callId), Guid.NewGuid().ToString())).Error == "operation_conflict" && lifetime.Sends == 2,
                "another operation cannot toggle stopped Voice back on");
            await Send("call.release", new { callId }, Guid.NewGuid().ToString());
            Check((await Send("call.desktop.startVoice", CommandHotkey(callId), Guid.NewGuid().ToString())).Error == "runtime_not_ready",
                "released call cannot regain Desktop input authority");
        }
        using var entered = new ManualResetEventSlim(); using var finish = new ManualResetEventSlim();
        ui = new CommandApplication { BeforeObserve = () => { entered.Set(); if (!finish.Wait(TimeSpan.FromSeconds(5))) throw new TimeoutException(); } };
        desktop = new PhoneDesktopRuntime(_ => ui, _ => owner, () => keyboard);
        await using (var operations = new NativeSipOperations(_ => new AudioPort(), desktop)) {
            string epoch = Guid.NewGuid().ToString(), callId = Guid.NewGuid().ToString(); long sequence = 0;
            var rpc = new NativeRpc(epoch, operations.InvokeAsync);
            Task<NativeResponse> Send(string method, object args, string? id = null) => rpc.ExecuteAsync(Request(epoch, ++sequence, method, args, id));
            await Send("configure", NativeFixtureConfig(), Guid.NewGuid().ToString()); await Send("call.prepare", new { callId }, Guid.NewGuid().ToString());
            var launch = Send("call.desktop.launch", new { callId, application = new NativeDesktopApplication("Fixture.Package!UI", true) }, Guid.NewGuid().ToString());
            try {
                Check(await Task.Run(() => entered.Wait(TimeSpan.FromSeconds(2))), "original Desktop observation entered");
                Check((await Send("heartbeat", new { }).WaitAsync(TimeSpan.FromSeconds(2))).Ok, "slow Desktop work leaves heartbeat available");
                Check((await Send("call.desktop.startVoice", CommandHotkey(callId), Guid.NewGuid().ToString())).Error == "runtime_not_ready", "overlapping Desktop actions refused");
                Check((await Send("call.hangup", new { callId }, Guid.NewGuid().ToString())).Ok, "slow Desktop work leaves SIP hangup available");
                var release = Send("call.release", new { callId }, Guid.NewGuid().ToString());
                await Task.Delay(20); Check(!release.IsCompleted, "release waits for original possible Desktop work");
                Check((await Send("call.prepare", new { callId = Guid.NewGuid().ToString() }, Guid.NewGuid().ToString())).Error == "operation_conflict", "replacement call cannot inherit late Desktop work");
                finish.Set(); var result = await launch;
                Check(result.Ok && result.Result!.Value.GetProperty("result").GetProperty("phase").GetString() == "not_submitted" && ui.Activations == 0,
                    "call cancellation during observation prevents subsequent activation");
                Check((await release).Ok, "release completes after original Desktop work ends");
            } finally { finish.Set(); }
        }
        foreach (bool partial in new[] { false, true }) {
            owner = new CommandOwner { FocusSucceeds = partial }; keyboard = new CommandKeyboard(owner) { Partial = partial };
            desktop = new PhoneDesktopRuntime(_ => new CommandApplication(), _ => owner, () => keyboard);
            // Use an explicit original identity to exercise both refusal and uncertain OS insertion.
            var result = desktop.VoiceToggle(CommandIdentity(), new(["control", "shift"], 32, true), () => true);
            Check(result.phase == (partial ? "outcome_unknown" : "not_submitted") && keyboard.Sends == (partial ? 2 : 0),
                "focus refusal and partial insertion preserve exact result and key-up-only cleanup");
        }
    }
}
