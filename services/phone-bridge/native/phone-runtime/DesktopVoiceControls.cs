namespace Ivy.PhoneBridge;

public sealed record VoiceControlIdentity(string Window, int[] ParentId, int[] MicrophoneId, int[] StopId);
public sealed record VoiceControlObservation(string State, VoiceControlIdentity Identity);

// One input to the live call owner. Never identifies a task, grants audio or retains a positive.
public static class DesktopVoiceControls {
    // Prefer the current conversation's control over the separate "new voice chat"
    // launcher when both are visible in the same Desktop window.
    private static readonly string[] starts = ["Start voice chat", "Sprachchat starten", "Start new voice chat", "Neuen Sprachchat starten"];
    private static readonly string[] stops = ["Stop voice chat", "End voice chat", "Sprachchat stoppen", "Sprachchat beenden"];
    private static readonly string[] mute = ["Mute microphone", "Mikrofon stummschalten"];
    private static readonly string[] unmute = ["Unmute microphone", "Mikrofonstummschaltung aufheben"];
    private static readonly string[] transitions = ["Cancel voice chat", "Ending voice chat…", "Starting voice chat",
        "Sprachchat abbrechen", "Sprachchat wird beendet…", "Sprachchat wird gestartet"];
    public static string[] Labels => [.. starts, .. stops, .. mute, .. unmute, .. transitions];
    internal static string[] StartLabels => (string[])starts.Clone();
    internal static string[] StopLabels => (string[])stops.Clone();

    private sealed class Snapshot(DesktopControlWindow[] windows) : IDesktopControlSource {
        public DesktopControlWindow[] Read(string[] labels) => windows;
        public void Dispose() { }
    }
    private sealed record Item(DesktopControlWindow Window, DesktopControl Control) {
        public bool Visible => !Window.Minimized && !Control.Offscreen;
        public bool Ready => Visible && Control.Enabled;
    }
    private static bool Is(string[] labels, Item item) => labels.Contains(item.Control.Name, StringComparer.Ordinal);
    private static VoiceControlObservation Result(string state) => new(state, null);

    public static VoiceControlObservation Classify(DesktopControlsObservation observation) {
        if (observation == null || observation.State != "observed") return Result("unavailable");
        // Use the same boundary checks/copying as the native observer, including duplicate IDs.
        var checkedObservation = DesktopControls.Read(Labels, () => true, () => new Snapshot(observation.Windows));
        if (checkedObservation.State != "observed") return Result("unavailable");
        var items = checkedObservation.Windows.SelectMany(window => window.Controls.Select(control => new Item(window, control))).ToArray();
        var voice = items.Where(item => !Is(starts, item)).ToArray();
        if (voice.Length == 0) return Result(items.Any(item => Is(starts, item) && item.Ready) ? "idle" : "unavailable");

        var microphones = voice.Where(item => Is(mute, item) || Is(unmute, item)).ToArray();
        var endings = voice.Where(item => Is(stops, item)).ToArray();
        var changing = voice.Where(item => Is(transitions, item)).ToArray();
        if (microphones.Length > 1 || endings.Length > 1 || changing.Length > 1) return Result("ambiguous");
        // Hidden controls still preclude an idle claim. Do not combine different presentations.
        if (voice.Any(item => !item.Visible)) return Result("unavailable");
        if (changing.Length > 0) return Result("transition");
        if (microphones.Length != 1 || endings.Length != 1) return Result("unavailable");
        var microphone = microphones[0]; var stop = endings[0];
        if (microphone.Window.Handle != stop.Window.Handle || !microphone.Control.ParentId.SequenceEqual(stop.Control.ParentId))
            return Result("ambiguous");
        if (!microphone.Ready || !stop.Ready) return Result("transition");
        bool muted = Is(unmute, microphone);
        if (microphone.Control.Toggle != (muted ? 1 : 0) || stop.Control.Toggle is 1 or 2) return Result("unavailable");
        return new(muted ? "muted" : "active", new(microphone.Window.Handle,
            microphone.Control.ParentId, microphone.Control.Id, stop.Control.Id));
    }
}
