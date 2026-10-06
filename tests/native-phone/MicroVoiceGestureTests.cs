using Ivy.PhoneBridge;

static partial class Program {
    sealed class GestureDevice : IMicroVoiceInput {
        public long Generation = 9;
        public string PressPhase = "submitted";
        public bool ThrowAfterPress;
        public readonly List<(bool Pressed, long Generation)> Writes = [];
        public MicroAttachmentStatus Status => new("ready", Generation, null);
        public MicroInputReceipt SendMicrophone(bool pressed, long generation, Func<bool> original) {
            if (Generation != generation || !original()) return new("not_submitted", pressed, generation);
            Writes.Add((pressed, generation));
            if (pressed && ThrowAfterPress) throw new IOException("lost write reply");
            return new(pressed ? PressPhase : "submitted", pressed, generation);
        }
        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
    static void MicroGestures() {
        foreach (bool cleanup in new[] { false, true }) {
            var device = new GestureDevice(); var pauses = new List<int>();
            var result = MicroVoiceGesture.Dispatch(device, 9, cleanup, () => true, () => true, pauses.Add);
            Check(result.Phase == "submitted" && pauses.SequenceEqual(cleanup ? new[] { 650 } : new[] { 50, 70, 50 }),
                "Realtime Voice latches within the current Desktop's 350ms double-tap deadline");
            Check(device.Writes.SequenceEqual(cleanup ? new[] { (true, 9L), (false, 9L) } :
                new[] { (true, 9L), (false, 9L), (true, 9L), (false, 9L) }) &&
                (cleanup ? result.LatchPress == null && result.LatchRelease == null :
                    result.LatchPress?.Phase == "submitted" && result.LatchRelease?.Phase == "submitted"),
                "startup sends a same-generation latch tap and cleanup sends one long press");
        }
        var unavailable = new GestureDevice();
        Check(MicroVoiceGesture.Dispatch(unavailable, 9, false, () => false, () => true, _ => throw new Exception()).Phase == "not_submitted" &&
            unavailable.Writes.Count == 0, "known refusal creates no release and is the only fallback-eligible outcome");
        foreach (bool lostReply in new[] { false, true }) {
            var unknown = new GestureDevice { PressPhase = "outcome_unknown", ThrowAfterPress = lostReply };
            var result = MicroVoiceGesture.Dispatch(unknown, 9, false, () => true, () => true, _ => throw new Exception());
            Check(result.Phase == "outcome_unknown" && unknown.Writes.Count == 1, "unknown press is never retried or presented as safe fallback");
        }
        var replaced = new GestureDevice();
        Check(MicroVoiceGesture.Dispatch(replaced, 9, false, () => true, () => true, _ => replaced.Generation++).Phase == "outcome_unknown" &&
            replaced.Writes.SequenceEqual(new[] { (true, 9L) }), "reconnection cannot receive a delayed release from old generation");
        var cancelled = new GestureDevice(); bool active = true;
        Check(MicroVoiceGesture.Dispatch(cancelled, 9, false, () => active, () => true, _ => active = false).Phase == "outcome_unknown" &&
            cancelled.Writes.Count == 2, "call cancellation releases the first press without starting a new latch tap");
        var interrupted = new GestureDevice();
        Check(MicroVoiceGesture.Dispatch(interrupted, 9, false, () => true, () => true, _ => throw new OperationCanceledException()).Phase == "outcome_unknown" &&
            interrupted.Writes.Count == 2, "interrupted hold still releases once without claiming complete gesture");
    }
}
