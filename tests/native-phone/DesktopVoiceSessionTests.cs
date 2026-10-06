using Ivy.PhoneBridge;

static partial class Program {
    sealed class VoiceSource : IDesktopVoiceSource {
        public DesktopControlWindow[] Windows = [new("123", false, [
            new([42, 1], [42, 0], "Mute microphone", true, false, 0),
            new([42, 2], [42, 0], "Stop voice chat", true, false, null)])];
        public Action? OnRead, OnRefresh, OnDispose;
        public int Discoveries, Refreshes, Disposals;
        public VoiceControlIdentity? Received;
        public DesktopControlWindow[] Read(string[] labels) { Discoveries++; OnRead?.Invoke(); return Windows; }
        public DesktopControlWindow[] Refresh(VoiceControlIdentity identity) {
            Refreshes++; Received = identity; OnRefresh?.Invoke(); return Windows;
        }
        public void Dispose() { Disposals++; OnDispose?.Invoke(); }
    }
    static void BoundVoiceControls() {
        var clock = new Clock(); var source = new VoiceSource(); bool current = true;
        source.OnRead = () => clock.Ticks = 618;
        using (var session = DesktopVoiceSession.Open(() => source, () => current, clock)) {
            Check(source.Discoveries == 1 && source.Refreshes == 0, "slow discovery never supplies a timed positive");
            source.OnRefresh = () => clock.Ticks += 40;
            var first = session.Read();
            Check(first.Timestamp == 618 && first.Observation.State == "active", "refresh measures from before current checks, independently of slow discovery");
            first.Observation.Identity.MicrophoneId[1] = 90; source.Received!.MicrophoneId[1] = 91;
            Check(session.Read().Observation.Identity.MicrophoneId[1] == 1, "consumer/provider mutation cannot rebind retained controls");
            var microphone = source.Windows[0].Controls[0];
            source.Windows[0].Controls[0] = microphone with { Name = "Unmute microphone", Toggle = 1 };
            Check(session.Read().Observation.State == "muted", "same original muted group stays distinct from usable audio");
            source.Windows[0].Controls[0] = microphone;
            source.OnRefresh = () => clock.Ticks += 500;
            Check(session.Read().Observation.State == "unavailable", "exact500ms observation cannot receive a fresh timestamp at completion");
            source.OnRefresh = () => clock.Ticks--;
            Check(session.Read().Observation.State == "unavailable", "backwards clock never creates a positive");
            source.OnRefresh = null;
            Check(session.Read().Observation.State == "active", "later fresh read can observe only original retained controls");
            foreach (var changed in new[] {
                microphone with { Id = [42, 99] }, microphone with { ParentId = [42, 99] },
                microphone with { Enabled = false }, microphone with { Offscreen = true },
                microphone with { Toggle = null }, microphone with { Name = "Cancel voice chat" }
            }) {
                source.Windows[0].Controls[0] = changed;
                Check(session.Read().Observation.Identity == null, "changed/hidden/disabled/replaced controls never regain prior positive");
            }
            source.Windows[0].Controls[0] = microphone;
            source.Windows[0] = source.Windows[0] with { Handle = "124" };
            Check(session.Read().Observation.Identity == null, "another window cannot replace original presentation");
            source.Windows[0] = source.Windows[0] with { Handle = "123" };
            source.OnRefresh = () => current = false;
            Check(session.Read().Observation.Identity == null, "owner loss during refresh discards all controls");
            int reads = source.Refreshes;
            Check(session.Read().Observation.Identity == null && source.Refreshes == reads, "lost Desktop never queries retained provider");
            current = true; source.OnRefresh = () => throw new IOException();
            Check(session.Read().Observation.Identity == null, "failed provider never returns last positive");
            Check(source.Discoveries == 1, "refresh never searches for replacement controls");
            bool wrongThread = false;
            var foreign = new Thread(() => { try { session.Read(); } catch (InvalidOperationException) { wrongThread = true; } });
            foreign.Start(); foreign.Join(); Check(wrongThread, "provider reads stay on original COM thread");
            session.Dispose();
            Check(session.Read().Observation.Identity == null, "disposed session never reads or returns a positive");
        }
        Check(source.Disposals == 1, "owned provider closes once");
        source = new VoiceSource { Windows = [] };
        Reject(() => DesktopVoiceSession.Open(() => source, () => true), "unavailable discovery cannot create session");
        Check(source.Disposals == 1, "failed discovery still closes owned provider");
        source = new VoiceSource(); bool opened = false;
        Reject(() => DesktopVoiceSession.Open(() => { opened = true; return source; }, () => false), "old Desktop cannot discover");
        Check(!opened, "current owner checked before provider allocation");
        source.OnDispose = () => throw new IOException();
        var failing = DesktopVoiceSession.Open(() => source, () => true);
        bool failed = false; try { failing.Dispose(); } catch (IOException) { failed = true; }
        Check(failed && failing.Read().Observation.Identity == null, "failed cleanup remains closed and reports failure");
    }
    static void ObserveInstalledVoiceRefresh(string applicationId) {
        var desktop = new WindowsDesktopApplication(applicationId).Observe();
        Check(desktop.state == "ready" && desktop.identity != null, "one installed Desktop owner required");
        var discovery = System.Diagnostics.Stopwatch.StartNew();
        using var session = WindowsDesktopControls.OpenVoice(desktop.identity!);
        discovery.Stop();
        var values = new List<object>(); bool allFresh = true;
        for (int index = 0; index < 3; index++) {
            var observation = session.Read();
            double elapsedMs = TimeProvider.System.GetElapsedTime(observation.Timestamp).TotalMilliseconds;
            values.Add(new { observation, elapsedMs });
            allFresh &= observation.Observation.State is "active" or "muted";
        }
        object Loaded(System.Reflection.Assembly assembly) => new { name = assembly.GetName().Name, path = assembly.Location,
            sha256 = Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(File.ReadAllBytes(assembly.Location))) };
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { schemaVersion = 1, readOnly = true,
            loadedAssemblies = new[] { Loaded(typeof(Program).Assembly), Loaded(typeof(WindowsDesktopControls).Assembly) },
            framework = System.Runtime.InteropServices.RuntimeInformation.FrameworkDescription,
            discoveryMs = discovery.Elapsed.TotalMilliseconds, refreshes = values }, NativeRpc.Json));
        // Emit diagnostic timings before assertions, including a failed refresh.
        Check(allFresh, "all three original control refreshes must be positive within500ms");
    }
}
