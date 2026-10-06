using System.Diagnostics;
using Ivy.PhoneBridge;

static partial class Program {
    const string CaptureImage = @"C:\Fixture\ChatGPT.exe";
    static readonly DesktopIdentity CaptureRoot = new(10, 100, CaptureImage, "Fixture.Package!UI");
    sealed class CorrelationProcesses : IAudioProcessSnapshot {
        public Dictionary<int, AudioProcessRecord> Values = new() {
            [10] = new(10, 1, 3, 100, CaptureImage), [20] = new(20, 10, 3, 200, CaptureImage),
            [30] = new(30, 20, 3, 300, CaptureImage), [40] = new(40, 1, 3, 200, @"C:\Other\Browser.exe")
        };
        public int Disposals;
        public Action? Closing;
        public AudioProcessRecord Read(int pid) => Values.GetValueOrDefault(pid)!;
        public void Dispose() { Disposals++; Closing?.Invoke(); }
    }
    sealed class CorrelationEndpoint(CaptureSession[] sessions) : ICaptureEndpoint {
        public string Id => "capture";
        public bool IsActiveCapture => true;
        public CaptureSession[] ReadSessions() => sessions;
        public void Dispose() { }
    }
    static CaptureObservation CaptureValues(params CaptureSession[] values) => CaptureObservation.Read("capture", _ => new CorrelationEndpoint(values));
    static async Task CaptureCorrelations() {
        var session = new CaptureSession("original-session", 1, 30, 0);
        var foreign = new CaptureSession("foreign-session", 1, 40, 0);
        var tree = new CorrelationProcesses(); int captures = 0;
        var result = DesktopCapture.Read(CaptureRoot, "capture", () => true, _ => { captures++; return CaptureValues(session, foreign); }, () => tree);
        Check(result.State == "matched" && result.Identity.InstanceId == session.instanceId && result.Identity.Pid == 30 && result.Identity.ProcessStartTimeUtcTicks == "300" &&
            captures == 2 && tree.Disposals == 1, "current exact Desktop session matched across held ancestry/capture observations; foreign session ignored");

        foreach (var entry in new (CaptureSession[] Sessions, string State)[] {
            ([], "none"), ([foreign], "none"), ([new("inactive", 0, 30, 0), new("expired", 2, 30, 0)], "none"),
            ([session, new("second", 1, 30, 0)], "ambiguous"), ([session, new("second", 1, 20, 0)], "ambiguous"),
            ([session, new("shared", 1, 40, 0x0889000d)], "ambiguous"), ([new("unknown", 1, 0, 0)], "ambiguous"),
            ([session, new("lost-process", 1, 50, 0)], "unavailable"), ([session, session], "unavailable"),
            (Enumerable.Range(0, 33).Select(i => new CaptureSession("active-" + i, 1, 30, 0)).ToArray(), "unavailable")
        }) {
            tree = new();
            result = DesktopCapture.Read(CaptureRoot, "capture", () => true, _ => CaptureValues(entry.Sessions), () => tree);
            Check(result.State == entry.State && result.Identity == null, "capture selection cannot upgrade absence, competing, shared or unreadable sessions: " + entry.State);
        }
        foreach (string change in new[] { "instance", "pid", "inactive", "shared", "foreign-added", "unavailable", "process-exit", "process-reuse", "parent-change" }) {
            tree = new(); captures = 0;
            CaptureObservation ReadCapture(string endpoint) {
                Check(tree.Disposals == 0, "process snapshot remains owned through final capture read");
                if (++captures == 1) return CaptureValues(session);
                if (change == "process-exit") tree.Values.Remove(30);
                if (change == "process-reuse") tree.Values[30] = new(30, 20, 3, 301, CaptureImage);
                if (change == "parent-change") tree.Values[20] = new(20, 1, 3, 200, CaptureImage);
                return change switch {
                    "instance" => CaptureValues(new CaptureSession("replacement-session", 1, 30, 0)),
                    "pid" => CaptureValues(new CaptureSession(session.instanceId, 1, 20, 0)),
                    "inactive" => CaptureValues(new CaptureSession(session.instanceId, 0, 30, 0)),
                    "shared" => CaptureValues(new CaptureSession(session.instanceId, 1, 30, 0x0889000d)),
                    "foreign-added" => CaptureValues(session, foreign),
                    "unavailable" => CaptureObservation.Read("capture", _ => throw new IOException()),
                    _ => CaptureValues(session)
                };
            }
            result = DesktopCapture.Read(CaptureRoot, "capture", () => true, ReadCapture, () => tree);
            Check(result.State == "unavailable" && result.Identity == null && tree.Disposals == 1, "mid-observation change invalidates match and releases resources: " + change);
        }
        tree = new(); tree.Values[20] = new(20, 10, 3, 400, CaptureImage);
        Check(DesktopCapture.Read(CaptureRoot, "capture", () => true, _ => CaptureValues(session), () => tree).State == "unavailable", "existing ancestry rule rejects reused newer parent");
        tree = new(); tree.Closing = () => throw new IOException("cleanup failed");
        Check(DesktopCapture.Read(CaptureRoot, "capture", () => true, _ => CaptureValues(session), () => tree).Identity == null, "cleanup failure cannot return matched identity");
        bool current = true; tree = new(); tree.Closing = () => current = false;
        Check(DesktopCapture.Read(CaptureRoot, "capture", () => current, _ => CaptureValues(session), () => tree).Identity == null, "Desktop replacement at cleanup invalidates matched identity");
        bool opened = false;
        Check(DesktopCapture.Read(CaptureRoot, "capture", () => false, _ => throw new Exception(), () => { opened = true; return new CorrelationProcesses(); }).State == "unavailable" && !opened,
            "retired Desktop never queries capture or processes");
        Reject(() => DesktopCapture.Read(CaptureRoot, "", () => true, _ => CaptureValues(session), () => new CorrelationProcesses()), "invalid endpoint refused before observation");

        int queries = 0;
        var desktopRuntime = new PhoneDesktopRuntime(captureOwner: (desktop, endpoint) => {
            Check(desktop.pid == CaptureRoot.pid && endpoint == "capture", "read-only IPC retains requested Desktop and endpoint"); queries++;
            return queries == 1 ? new(endpoint, "matched", new("original-session", 30, "300")) : new(endpoint, "unavailable", null!);
        });
        await using (var operations = new NativeSipOperations(_ => throw new Exception("No audio allocation"), desktopRuntime)) {
            string epoch = Guid.NewGuid().ToString(); var rpc = new NativeRpc(epoch, operations.InvokeAsync, 1);
            var desktop = new NativeDesktopIdentity(CaptureRoot.pid, CaptureRoot.startTimeUtcTicks, CaptureImage, CaptureRoot.appUserModelId);
            var reply = await rpc.ExecuteAsync(Request(epoch, 1, "desktop.captureOwner", new { desktop, endpointId = "capture" }));
            Check(reply.Ok && reply.Result!.Value.GetProperty("identity").GetProperty("processStartTimeUtcTicks").GetString() == "300", "IPC serializes exact process incarnation without effect receipt");
            reply = await rpc.ExecuteAsync(Request(epoch, 2, "desktop.captureOwner", new { desktop, endpointId = "capture" }));
            Check(reply.Ok && reply.Result!.Value.GetProperty("identity").ValueKind == System.Text.Json.JsonValueKind.Null, "unavailable IPC result cannot retain previous match and consumes no effect capacity");
            reply = await rpc.ExecuteAsync(Request(epoch, 3, "desktop.captureOwner", new { desktop, endpointId = "" }));
            Check(reply.Error == "invalid_request" && queries == 2, "invalid endpoint never reaches capture provider");
        }

        // Real Windows handles, using only this synthetic child. No Desktop or audio device.
        var info = new ProcessStartInfo(Environment.ProcessPath!) { UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true };
        info.ArgumentList.Add(typeof(Program).Assembly.Location); info.ArgumentList.Add("--held-process-fixture");
        using var child = Process.Start(info)!;
        try {
            Check(await child.StandardOutput.ReadLineAsync().WaitAsync(TimeSpan.FromSeconds(5)) == "held-process-ready", "synthetic handle child ready");
            using var snapshot = WindowsAudioProcessOwnership.OpenSnapshot();
            var original = snapshot.Read(child.Id);
            Check(original != null && original.pid == child.Id && original.startTicks == child.StartTime.ToUniversalTime().Ticks && original.Same(snapshot.Read(child.Id)), "actual held process reads preserve original incarnation");
            await child.StandardInput.WriteLineAsync("exit"); await child.StandardInput.FlushAsync();
            await child.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
            Check(child.ExitCode == 0 && snapshot.Read(child.Id) == null, "signaled original process can never be returned as a live replacement");
        } finally {
            if (!child.HasExited) { child.Kill(); await child.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5)); }
        }
    }
}
