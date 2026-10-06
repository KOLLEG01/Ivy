using Ivy.PhoneBridge;

static partial class Program {
    static void ObserveInstalledCaptureEndpoint(string applicationId, string endpointId) {
        var desktop = new WindowsDesktopApplication(applicationId).Observe();
        Check(desktop.state == "ready" && desktop.identity != null, "one current installed Desktop owner required");
        var identity = desktop.identity!;
        var capture = DesktopCapture.Observe(identity, endpointId);
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { schemaVersion = 1, readOnly = true,
            desktop = new { identity.pid, identity.startTimeUtcTicks, identity.appUserModelId }, capture }, NativeRpc.Json));
    }
    static async Task RecoverInstalledVoiceHotkey(string applicationId, string endpointId) {
        var desktop = new WindowsDesktopApplication(applicationId).Observe();
        Check(desktop.state == "ready" && desktop.identity != null, "one current installed Desktop owner required");
        var identity = desktop.identity!;
        var before = DesktopCapture.Observe(identity, endpointId);
        Check(before.State == "matched" && before.Identity != null, "exact Desktop capture must be active before recovery");
        await using var runtime = new PhoneDesktopRuntime();
        var hotkey = new NativeHotkey(["control", "alt", "shift"], 123, false);
        var result = runtime.VoiceToggle(identity, hotkey, identity.Owner().IsCurrent);
        Check(result.phase == "submitted", "configured recovery hotkey must be submitted to the current Desktop");
        var began = System.Diagnostics.Stopwatch.StartNew();
        DesktopCaptureCorrelation after;
        do {
            Thread.Sleep(100); after = DesktopCapture.Observe(identity, endpointId);
        } while (identity.Owner().IsCurrent() && after.State != "none" && began.Elapsed < TimeSpan.FromSeconds(10));
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { schemaVersion = 1, action = "recover-hotkey",
            desktop = new { identity.pid, identity.startTimeUtcTicks, identity.appUserModelId }, before = before.State,
            result.phase, after = after.State }, NativeRpc.Json));
        Check(after.State == "none" && after.Identity == null, "recovery must prove capture absent");
    }
    static void InvokeInstalledVoiceControl(string applicationId, bool stop) {
        var desktop = new WindowsDesktopApplication(applicationId).Observe();
        Check(desktop.state == "ready" && desktop.identity != null, "one current installed Desktop owner required");
        var identity = desktop.identity!;
        var before = DesktopVoiceControls.Classify(WindowsDesktopControls.Observe(identity, DesktopVoiceControls.Labels));
        Check(stop ? before.State is "active" or "muted" : before.State == "idle", "exact requested Voice control must be visible");
        var result = WindowsDesktopControls.InvokeVoice(identity, stop, identity.Owner().IsCurrent);
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { schemaVersion = 1, action = stop ? "stop" : "start",
            before = before.State, result.Phase }, NativeRpc.Json));
        Check(result.Phase == "submitted", "exact Voice control invocation must be submitted");
    }
    static void ObserveInstalledControls(string applicationId) {
        object Loaded(System.Reflection.Assembly assembly) => new { name = assembly.GetName().Name, path = assembly.Location,
            sha256 = Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(File.ReadAllBytes(assembly.Location))) };
        var loadedAssemblies = new[] { Loaded(typeof(Program).Assembly), Loaded(typeof(WindowsDesktopControls).Assembly) };
        var desktop = new WindowsDesktopApplication(applicationId).Observe();
        Check(desktop.state == "ready" && desktop.identity != null, "one current installed Desktop owner required for read-only acceptance");
        var identity = desktop.identity!;
        string[] labels = ["Neuen Sprachchat starten", "Sprachchat starten", "Sprachchat stoppen", "Mikrofon stummschalten",
            "Mikrofonstummschaltung aufheben", "Start new voice chat", "Start voice chat", "Stop voice chat", "Mute microphone", "Unmute microphone",
            "End voice chat", "Sprachchat beenden"];
        var diagnostics = new List<object>();
        var stages = new List<DesktopReadStage>();
        var observationTime = System.Diagnostics.Stopwatch.StartNew();
        var timed = WindowsDesktopControls.ObserveTimed(identity, labels, null, error => diagnostics.Add(new {
            errorType = error.GetType().FullName, hResult = error.HResult,
            propertyId = error.Data["propertyId"] is int property ? (int?)property : null,
            frames = new System.Diagnostics.StackTrace(error, true).GetFrames().Take(8).Select(frame => new {
                method = frame.GetMethod()?.DeclaringType?.FullName + "." + frame.GetMethod()?.Name, line = frame.GetFileLineNumber()
            }).ToArray()
        }), onReadStage: value => stages.Add(value));
        observationTime.Stop();
        var result = timed.Observation;
        double freshObservationMs = TimeProvider.System.GetElapsedTime(timed.Timestamp).TotalMilliseconds;
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { schemaVersion = 1, readOnly = true, loadedAssemblies,
            framework = System.Runtime.InteropServices.RuntimeInformation.FrameworkDescription,
            desktop = new { identity.pid, identity.startTimeUtcTicks, identity.imagePath, identity.appUserModelId },
            observation = result, observationMs = observationTime.Elapsed.TotalMilliseconds, freshObservationMs, diagnostics, stages }, NativeRpc.Json));
        Check(result.State == "observed" && result.Windows.Any(window => window.Controls.Length > 0), "actual Desktop must expose requested controls through production UI Automation");
    }
    sealed class ControlSource(DesktopControlWindow[] windows, Action? closed = null) : IDesktopControlSource {
        public DesktopControlWindow[] Read(string[] labels) => windows;
        public void Dispose() => closed?.Invoke();
    }
    static async Task ControlObservations() {
        CachedControlSelection();
        var control = new DesktopControl([42, 1], [42, 0], "Mute microphone", true, false, 0);
        var window = new DesktopControlWindow("123", false, [control]); string[] labels = [control.Name, "Stop voice chat"];
        bool current = true;
        var observed = DesktopControls.Read(labels, () => current, () => new ControlSource([window], () => control.Id[1] = 9));
        Check(observed.State == "observed" && observed.Windows[0].Controls[0].Id[1] == 1, "snapshot owns values before provider release");
        control.Id[1] = 1;
        foreach (var invalid in new DesktopControlWindow[][] {
            [], [window, window], [window with { Controls = [control, control] }],
            [window with { Controls = [control with { Name = "unrequested document content" }] }],
            [window with { Controls = [control with { ParentId = control.Id }] }],
            [window with { Controls = [control with { Toggle = 3 }] }],
            [window with { Controls = [control with { Id = [] }] }],
            [window with { Handle = "0123" }], Enumerable.Repeat(window, 33).ToArray()
        }) Check(DesktopControls.Read(labels, () => true, () => new ControlSource(invalid)).State == "unavailable", "partial, ambiguous or unrequested control data cannot become authority");
        Check(DesktopControls.Read(labels, () => current, () => new ControlSource([window], () => current = false)).State == "unavailable",
            "Desktop change at provider release invalidates entire observation");
        bool opened = false;
        Check(DesktopControls.Read(labels, () => false, () => { opened = true; return new ControlSource([window]); }).State == "unavailable" && !opened,
            "stale Desktop never opens an accessibility provider");
        Reject(() => DesktopControls.Read(["same", "same"], () => true, () => new ControlSource([window])), "duplicate labels refused before provider work");
        Check(DesktopControls.Read(labels, () => true, () => new ControlSource([window], () => throw new IOException())).Windows.Length == 0,
            "failed provider cleanup returns no partial values");
        Exception? diagnostic = null;
        Check(DesktopControls.Read(labels, () => true, () => throw new IOException("provider failed"), error => diagnostic = error).State == "unavailable" &&
            diagnostic is IOException, "local diagnostics preserve original failure without changing unavailable result");
        Check(DesktopControls.Read(labels, () => false, () => throw new Exception(), _ => throw new IOException()).Windows.Length == 0,
            "failed diagnostic sink cannot expose partial or positive control data");
        var clock = new Clock(); long stamp = -1;
        var timedSource = new TimingControlSource([window], () => clock.Ticks += 40, () => clock.Ticks += 700);
        var timed = DesktopControls.Read(labels, () => true, () => { clock.Ticks += 1000; return timedSource; },
            onReadStarted: () => stamp = clock.Ticks);
        Check(timed.State == "observed" && stamp == 1700 && clock.Ticks - stamp == 40 && timedSource.Disposed,
            "provider and fixed query initialization precede timestamp; the entire actual read and cleanup follow it");
        bool valid = true, read = false; var abandoned = new TimingControlSource([window], () => read = true);
        var changedOwner = DesktopControls.Read(labels, () => valid, () => { valid = false; return abandoned; }, onReadStarted: () => stamp = clock.Ticks);
        Check(changedOwner.State == "unavailable" && !read && abandoned.Disposed, "owner is rechecked after slow setup before any UI evidence read");
        valid = true; read = false;
        var changedDuringPrepare = new TimingControlSource([window], () => read = true, () => valid = false);
        Check(DesktopControls.Read(labels, () => valid, () => changedDuringPrepare, onReadStarted: () => stamp = clock.Ticks).State == "unavailable" &&
            !read && changedDuringPrepare.Disposed, "owner loss during fixed query preparation cannot authorize a subsequent read");
        var hidden = window with { Minimized = true, Controls = [control with { Enabled = false, Offscreen = true, Toggle = null }] };
        observed = DesktopControls.Read(labels, () => true, () => new ControlSource([hidden]));
        Check(observed.Windows[0].Minimized && !observed.Windows[0].Controls[0].Enabled && observed.Windows[0].Controls[0].Offscreen &&
            observed.Windows[0].Controls[0].Toggle == null, "unavailable toggle, disabled, minimized and offscreen are not upgraded");

        int queries = 0; var runtime = new PhoneDesktopRuntime(controls: (identity, names) => {
            Check(identity.pid == 123 && names.SequenceEqual(labels), "IPC retains original Desktop and exact label set"); queries++; return observed;
        });
        await using var operations = new NativeSipOperations(_ => throw new Exception("No audio allocation"), runtime);
        string epoch = Guid.NewGuid().ToString(); var rpc = new NativeRpc(epoch, operations.InvokeAsync, 1);
        var desktop = new NativeDesktopIdentity(123, "1", Path.GetFullPath("fixture.exe"), "Fixture.Package!UI");
        for (int id = 1; id <= 2; id++) {
            var reply = await rpc.ExecuteAsync(Request(epoch, id, "desktop.controls", new { desktop, labels }));
            Check(reply.Ok && reply.Result!.Value.GetProperty("windows")[0].GetProperty("controls")[0].GetProperty("toggle").ValueKind == System.Text.Json.JsonValueKind.Null,
                "read-only controls serialize complete properties and consume no effect receipt capacity");
        }
        var bad = await rpc.ExecuteAsync(Request(epoch, 3, "desktop.controls", new { desktop, labels = new[] { "" } }));
        Check(bad.Error == "invalid_request" && queries == 2, "invalid labels refused before native observation");
    }
    sealed class TimingControlSource(DesktopControlWindow[] windows, Action beforeRead, Action? prepare = null) : IDesktopControlSource {
        public bool Disposed;
        public void Prepare(string[] labels) => prepare?.Invoke();
        public DesktopControlWindow[] Read(string[] labels) { beforeRead(); return windows; }
        public void Dispose() => Disposed = true;
    }
    static void CachedControlSelection() {
        var idle = new DesktopControl([42, 1], [42, 0], "Start voice chat", true, false, null);
        var hidden = idle with { Id = [42, 2], Name = "Stop voice chat", Offscreen = true };
        var unrelated = idle with { Id = [42, 3], Name = "STOP VOICE CHAT", ParentId = [] };
        var candidates = new[] { unrelated, idle, hidden, hidden };
        var selected = DesktopControls.SelectNamed(candidates.Length, index => candidates[index], item => item.Name, DesktopVoiceControls.Labels);
        Check(selected.SequenceEqual(new[] { idle, hidden, hidden }),
            "cached-name selection is exact, complete and ordered; hidden/duplicate Voice controls are never filtered out");
        Check(DesktopControls.Read(DesktopVoiceControls.Labels, () => true,
            () => new ControlSource([new("123", false, selected)])).State == "unavailable",
            "duplicate matched runtime IDs still invalidate the complete observation");
        Check(DesktopVoiceControls.Classify(new("observed", [new("123", false, [idle, hidden])])).State == "unavailable",
            "hidden Voice stop still prevents a false idle claim after cached-name selection");
        int reads = 0;
        var bounded = DesktopControls.SelectNamed(4096, index => { reads++; return index == 4095 ? idle : unrelated; }, item => item.Name, DesktopVoiceControls.Labels);
        Check(reads == 4096 && bounded.Length == 1 && bounded[0] == idle,
            "the full bounded button inventory includes a matching last element, without reading unrelated parent metadata");
        foreach (int size in new[] { -1, 4097 }) Reject(() => DesktopControls.SelectNamed<DesktopControl>(size,
            _ => throw new Exception("oversized inventory was accessed"), item => item.Name, DesktopVoiceControls.Labels),
            "invalid candidate count refuses before element access");
        Reject(() => DesktopControls.SelectNamed(65, _ => idle, item => item.Name, DesktopVoiceControls.Labels),
            "more than64 matches remains unavailable instead of returning a truncated positive");
        var failed = DesktopControls.Read(DesktopVoiceControls.Labels, () => true, () => {
            DesktopControls.SelectNamed(2, index => index == 0 ? idle : throw new IOException(), item => item.Name, DesktopVoiceControls.Labels);
            return new ControlSource([new("123", false, [idle])]);
        });
        Check(failed.State == "unavailable" && failed.Windows.Length == 0,
            "provider failure after an early match cannot publish a partial idle inventory");
    }
}
