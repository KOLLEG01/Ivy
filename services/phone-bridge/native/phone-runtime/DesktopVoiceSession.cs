namespace Ivy.PhoneBridge;

public interface IDesktopVoiceSource : IDesktopControlSource {
    DesktopControlWindow[] Refresh(VoiceControlIdentity identity);
}
public sealed record TimedVoiceControlObservation(long Timestamp, VoiceControlObservation Observation);
public interface IDesktopVoiceReader : IDisposable { TimedVoiceControlObservation Read(); }

// Owns one set of original controls on the provider's thread. Discovery cannot authorize audio.
public sealed class DesktopVoiceSession : IDesktopVoiceReader {
    private readonly IDesktopVoiceSource source;
    private readonly Func<bool> current;
    private readonly TimeProvider time;
    private readonly int thread = Environment.CurrentManagedThreadId;
    private readonly VoiceControlIdentity identity;
    private bool disposed;
    private sealed class Borrowed(Func<DesktopControlWindow[]> read) : IDesktopControlSource {
        public DesktopControlWindow[] Read(string[] labels) => read();
        public void Dispose() { }
    }
    private static VoiceControlIdentity Copy(VoiceControlIdentity value) => new(value.Window,
        (int[])value.ParentId.Clone(), (int[])value.MicrophoneId.Clone(), (int[])value.StopId.Clone());
    private static bool Same(VoiceControlIdentity left, VoiceControlIdentity right) => left != null && right != null &&
        left.Window == right.Window && left.ParentId.SequenceEqual(right.ParentId) &&
        left.MicrophoneId.SequenceEqual(right.MicrophoneId) && left.StopId.SequenceEqual(right.StopId);
    private DesktopVoiceSession(IDesktopVoiceSource source, Func<bool> current, TimeProvider time, VoiceControlIdentity identity) {
        this.source = source; this.current = current; this.time = time; this.identity = Copy(identity);
    }
    public static DesktopVoiceSession Open(Func<IDesktopVoiceSource> open, Func<bool> current, TimeProvider time = null) {
        ArgumentNullException.ThrowIfNull(open); ArgumentNullException.ThrowIfNull(current);
        IDesktopVoiceSource source = null;
        try {
            if (!current()) throw new InvalidOperationException("Current Desktop required.");
            source = open() ?? throw new InvalidOperationException("Voice provider required.");
            var discovery = DesktopControls.Read(DesktopVoiceControls.Labels, current,
                () => new Borrowed(() => source.Read(DesktopVoiceControls.Labels)));
            var classified = DesktopVoiceControls.Classify(discovery);
            if (classified.Identity == null) throw new InvalidOperationException("One original Voice control group required.");
            var session = new DesktopVoiceSession(source, current, time ?? TimeProvider.System, classified.Identity);
            source = null; return session;
        } finally { source?.Dispose(); }
    }
    private void CheckThread() {
        if (Environment.CurrentManagedThreadId != thread) throw new InvalidOperationException("Voice provider must remain on its owning thread.");
    }
    public TimedVoiceControlObservation Read() {
        CheckThread(); long timestamp = time.GetTimestamp();
        var unavailable = new TimedVoiceControlObservation(timestamp, new("unavailable", null));
        if (disposed) return unavailable;
        var refreshed = DesktopControls.Read(DesktopVoiceControls.Labels, current,
            () => new Borrowed(() => source.Refresh(Copy(identity))));
        var classified = DesktopVoiceControls.Classify(refreshed);
        var age = time.GetElapsedTime(timestamp, time.GetTimestamp());
        if (age < TimeSpan.Zero || age >= TimeSpan.FromMilliseconds(500) || !Same(identity, classified.Identity)) return unavailable;
        return new(timestamp, classified);
    }
    public void Dispose() {
        CheckThread(); if (disposed) return; disposed = true; source.Dispose();
    }
}
