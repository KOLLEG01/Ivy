using System.Globalization;

namespace Ivy.PhoneBridge;

public sealed record DesktopCaptureIdentity(string InstanceId, int Pid, string ProcessStartTimeUtcTicks);
public sealed record DesktopCaptureCorrelation(string EndpointId, string State, DesktopCaptureIdentity Identity);

public static class DesktopCapture {
    // The outer observation owns all handles; ancestry checks borrow the same pinned processes.
    private sealed class Processes(IAudioProcessSnapshot source) : IAudioProcessSnapshot {
        private readonly Dictionary<int, AudioProcessRecord> records = new();
        public AudioProcessRecord Read(int pid) {
            var record = source.Read(pid);
            if (record == null) return null;
            if (records.TryGetValue(pid, out var original) && !original.Same(record))
                throw new InvalidOperationException("Process changed during capture correlation.");
            records[pid] = record; return record;
        }
        public bool Current() => records.Values.All(record => record.Same(source.Read(record.pid)));
        public void Dispose() { } // Borrowed; only the enclosing observation closes the source.
    }
    private static CaptureSession[] Active(string endpointId, CaptureObservation observation) {
        if (observation == null || observation.state != "observed" || observation.endpointId != endpointId || observation.sessions == null ||
            observation.sessions.Length > 4096 || observation.sessions.Any(session => session == null) ||
            observation.sessions.Select(session => session.instanceId).Distinct(StringComparer.Ordinal).Count() != observation.sessions.Length)
            throw new InvalidOperationException("Exact current capture inventory required.");
        var active = observation.sessions.Where(session => session.state == "active").OrderBy(session => session.instanceId, StringComparer.Ordinal).ToArray();
        if (active.Length > 32) throw new InvalidOperationException("Active capture limit exceeded.");
        return active;
    }
    private static bool Same(CaptureSession[] left, CaptureSession[] right) => left.Length == right.Length &&
        left.Zip(right).All(pair => pair.First.instanceId == pair.Second.instanceId && pair.First.pid == pair.Second.pid && pair.First.singleProcess == pair.Second.singleProcess);

    public static DesktopCaptureCorrelation Observe(DesktopIdentity desktop, string endpointId) {
        ArgumentNullException.ThrowIfNull(desktop);
        return Read(desktop, endpointId, desktop.Owner().IsCurrent, WindowsCaptureObservation.Observe, WindowsAudioProcessOwnership.OpenSnapshot);
    }
    public static DesktopCaptureCorrelation Read(DesktopIdentity desktop, string endpointId, Func<bool> current,
        Func<string, CaptureObservation> capture, Func<IAudioProcessSnapshot> openProcesses) {
        ArgumentNullException.ThrowIfNull(desktop); CaptureObservation.ValidateText(endpointId);
        ArgumentNullException.ThrowIfNull(current); ArgumentNullException.ThrowIfNull(capture); ArgumentNullException.ThrowIfNull(openProcesses);
        DesktopCaptureCorrelation Result(string state) => new(endpointId, state, null);
        try {
            if (!current()) return Result("unavailable");
            var initial = Active(endpointId, capture(endpointId));
            if (initial.Any(session => !session.singleProcess || session.pid is 0 or > int.MaxValue)) return Result("ambiguous");
            DesktopCaptureCorrelation result;
            using (var source = openProcesses()) {
                var processes = new Processes(source);
                var owned = new Dictionary<int, AudioProcessRecord>();
                foreach (int pid in initial.Select(session => (int)session.pid).Distinct()) {
                    var ownership = AudioProcessOwnership.Read(desktop, pid, current, () => processes);
                    if (ownership.state == "unavailable") throw new InvalidOperationException("Capture process ownership unavailable.");
                    if (ownership.state == "owned") owned.Add(pid, processes.Read(pid) ?? throw new InvalidOperationException("Capture process exited."));
                }
                var final = Active(endpointId, capture(endpointId));
                if (!Same(initial, final) || !processes.Current() || !current()) throw new InvalidOperationException("Capture ownership changed.");
                var matches = final.Where(session => owned.ContainsKey((int)session.pid)).ToArray();
                result = matches.Length == 0 ? Result("none") : matches.Length > 1 ? Result("ambiguous") :
                    new(endpointId, "matched", new(matches[0].instanceId, (int)matches[0].pid,
                        owned[(int)matches[0].pid].startTicks.ToString(CultureInfo.InvariantCulture)));
            }
            return current() ? result : Result("unavailable");
        } catch { return Result("unavailable"); }
    }
}
