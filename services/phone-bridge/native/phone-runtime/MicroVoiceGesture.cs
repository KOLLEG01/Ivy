namespace Ivy.PhoneBridge;

public interface IMicroVoiceInput : IAsyncDisposable {
    MicroAttachmentStatus Status { get; }
    MicroInputReceipt SendMicrophone(bool pressed, long generation, Func<bool> original);
    MicroVoiceObservation ObserveVoice(long generation) => new("unavailable", generation, 0);
}
public sealed record MicroGestureResult(string Phase, MicroInputReceipt Press, MicroInputReceipt Release,
    MicroInputReceipt LatchPress = null, MicroInputReceipt LatchRelease = null);

// Codex Desktop's Realtime Micro mode starts on the first press, but a short single
// tap stops again at its 350ms deadline. A second press before that deadline latches
// Voice. A long press ends a latched session. Neither receipt asserts Voice state.
public static class MicroVoiceGesture {
    public static MicroGestureResult Dispatch(IMicroVoiceInput device, long generation, bool cleanup,
        Func<bool> beforePress, Func<bool> releaseAuthority, Action<int> pause = null) {
        ArgumentNullException.ThrowIfNull(device); ArgumentNullException.ThrowIfNull(beforePress);
        ArgumentNullException.ThrowIfNull(releaseAuthority);
        if (generation is < 1 or > 9007199254740991) throw new ArgumentException("Exact original Micro generation required.");
        pause ??= Thread.Sleep;
        MicroInputReceipt press;
        try { press = device.SendMicrophone(true, generation, beforePress); }
        catch { return new("outcome_unknown", new("outcome_unknown", true, generation), null); }
        if (press.Phase == "not_submitted") return new("not_submitted", press, null);
        if (press.Phase != "submitted") return new("outcome_unknown", press, null);
        MicroInputReceipt release = null; bool interrupted = false;
        try { pause(cleanup ? 650 : 50); }
        catch { interrupted = true; }
        finally {
            // Original generation only. A new HID handle must never receive a delayed key-up
            // from this gesture. Release remains cleanup even when the SIP call was cancelled.
            try { release = device.SendMicrophone(false, generation, releaseAuthority); }
            catch { interrupted = true; }
        }
        if (interrupted || release?.Phase != "submitted") return new("outcome_unknown", press, release);
        if (cleanup) return new("submitted", press, release);

        // The second tap belongs to the same original HID generation. If any part
        // is uncertain, do not substitute a hotkey or repeat a possibly delivered
        // press. The call's normal cleanup will end any Voice that did start.
        MicroInputReceipt latchPress = null, latchRelease = null;
        try { pause(70); }
        catch { return new("outcome_unknown", press, release); }
        try { latchPress = device.SendMicrophone(true, generation, beforePress); }
        catch { return new("outcome_unknown", press, release, new("outcome_unknown", true, generation)); }
        if (latchPress.Phase != "submitted") return new("outcome_unknown", press, release, latchPress);
        interrupted = false;
        try { pause(50); }
        catch { interrupted = true; }
        finally {
            try { latchRelease = device.SendMicrophone(false, generation, releaseAuthority); }
            catch { interrupted = true; }
        }
        return new(!interrupted && latchRelease?.Phase == "submitted" ? "submitted" : "outcome_unknown",
            press, release, latchPress, latchRelease);
    }
}
