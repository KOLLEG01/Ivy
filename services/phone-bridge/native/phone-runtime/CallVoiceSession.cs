namespace Ivy.PhoneBridge;

public sealed record VoiceLifetimeResult(string State, object Dispatch, string Reason = null);
public sealed record VoiceBaselineObservation(bool Cleanup, string ControlsState, string CaptureState,
    double ControlsMs, double CaptureMs, double AgeMs, string Reason);

// Micro controls Voice. Windows capture identifies its audio session. UI labels and short
// renewable audio permits are not part of call ownership.
public sealed class CallVoiceSession : IAsyncDisposable {
    private readonly CallAudioPort port;
    private readonly DesktopIdentity desktop;
    private readonly string endpoint;
    private readonly NativeVoiceInput input;
    private readonly PhoneDesktopRuntime runtime;
    private readonly Func<bool, bool> current;
    private readonly TimeProvider time;
    private readonly Action<VoiceBaselineObservation> observeBaseline;
    private readonly ManualResetEventSlim stop = new();
    private readonly TaskCompletionSource<VoiceLifetimeResult> started = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly TaskCompletionSource<VoiceLifetimeResult> finished = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly object sync = new();
    private bool launched, disposed, preserveAudio;
    private DesktopCaptureIdentity confirmedCapture;
    private Task disposal;
    public CallVoiceSession(CallAudioPort port, DesktopIdentity desktop, NativeVoiceInput input, PhoneDesktopRuntime runtime,
        Func<bool, bool> current, TimeProvider time = null, Action<VoiceBaselineObservation> observeBaseline = null) {
        ArgumentNullException.ThrowIfNull(port); ArgumentNullException.ThrowIfNull(desktop); ArgumentNullException.ThrowIfNull(input);
        var target = port.VoiceTarget;
        if (desktop.pid != target.Desktop.pid || desktop.startTimeUtcTicks != target.Desktop.startTimeUtcTicks ||
            desktop.appUserModelId != target.Desktop.appUserModelId || !StringComparer.OrdinalIgnoreCase.Equals(desktop.imagePath, target.Desktop.imagePath))
            throw new ArgumentException("Voice must use the audio call's Desktop.");
        this.port = port; this.desktop = desktop; endpoint = target.EndpointId;
        this.input = input.Snapshot(); this.runtime = runtime; this.current = current;
        this.time = time ?? TimeProvider.System; this.observeBaseline = observeBaseline;
    }
    private bool Current(bool cleanup) => current(cleanup) && runtime.IsCurrent(desktop);
    private DesktopCaptureCorrelation Capture() => runtime.VoiceCapture(desktop, endpoint);
    private bool Absent(DesktopCaptureCorrelation value) => value?.EndpointId == endpoint && value.State == "none" && value.Identity == null;
    private bool Matched(DesktopCaptureCorrelation value) => value?.EndpointId == endpoint && value.State == "matched" && value.Identity != null;
    internal bool HasOriginalCapture() {
        try {
            if (stop.IsSet || !started.Task.IsCompletedSuccessfully || started.Task.Result.State != "active" || finished.Task.IsCompleted ||
                !Current(false)) return false;
            var observed = Capture();
            return Matched(observed) && observed.Identity == Volatile.Read(ref confirmedCapture);
        } catch { return false; }
    }
    public Task<VoiceLifetimeResult> StartAsync() {
        lock (sync) {
            if (launched || disposed || stop.IsSet) throw new InvalidOperationException("Voice start may run once per call.");
            launched = true;
            var thread = new Thread(Run) { IsBackground = true, Name = "Ivy call Voice" };
            try { thread.SetApartmentState(ApartmentState.MTA); thread.Start(); }
            catch { launched = false; disposed = true; stop.Dispose(); throw; }
            return started.Task;
        }
    }
    public Task<VoiceLifetimeResult> StopAsync() {
        lock (sync) {
            if (disposed) return finished.Task;
            preserveAudio = false; port.Revoke(); stop.Set();
            if (!launched) { started.TrySetResult(new("not_started", null)); finished.TrySetResult(new("not_started", null)); }
            return finished.Task;
        }
    }
    internal Task<VoiceLifetimeResult> PauseForTransitionAsync() {
        lock (sync) {
            if (disposed || stop.IsSet || !started.Task.IsCompletedSuccessfully || started.Task.Result.State != "active")
                throw new InvalidOperationException("A Voice transition requires an active session.");
            preserveAudio = true; port.ClearAuthority(); stop.Set(); return finished.Task;
        }
    }
    internal async Task RetireForTransitionAsync() {
        var result = await finished.Task;
        lock (sync) {
            if (disposed || !preserveAudio || result.State != "stopped") throw new InvalidOperationException("Voice must stop before replacement.");
            port.ReleaseVoiceAuthority(); disposed = true; stop.Dispose(); disposal = Task.CompletedTask;
        }
    }
    private VoiceDispatchResult Dispatch(bool cleanup, ref VoiceInputPlan plan) {
        MicroGestureResult refused = null;
        for (int attempt = 0; attempt < 2; attempt++) {
            if (!runtime.PrepareInputFocus(plan, input, desktop, () => Current(cleanup))) return null;
            var result = runtime.DispatchVoice(plan, input, desktop, cleanup,
                () => (cleanup || !stop.IsSet) && current(cleanup), () => current(true), refused);
            if (result.Phase != "not_submitted" || plan.Method != "micro" || input.HotkeyFallback == null || attempt != 0) return result;
            refused = result.Micro; plan = PhoneDesktopRuntime.FallbackInput();
        }
        return null;
    }
    private VoiceDispatchResult DispatchDictationFallback(ref VoiceInputPlan plan, MicroGestureResult priorMicro) {
        if (!runtime.PrepareInputFocus(plan, input, desktop, () => Current(false))) return null;
        return runtime.DispatchVoice(plan, input, desktop, false,
            () => !stop.IsSet && current(false), () => current(true), priorMicro, priorMicroWasDictation: true);
    }
    private void Run() {
        VoiceDispatchResult dispatch = null;
        DesktopCaptureIdentity binding = null;
        VoiceInputPlan plan = null;
        MicroVoiceObservation microBaseline = null;
        bool possibleStart = false;
        string reason = "voice_cancelled";
        VoiceLifetimeResult ending = new("not_started", null);
        try {
            if (stop.IsSet || !Current(false)) return;
            long began = time.GetTimestamp(); var baseline = Capture();
            // A concurrent Windows audio graph update can make one read unavailable.
            // Retry observations only; never dispatch input without a proven idle baseline.
            for (int retry = 0; retry < 20 && baseline?.State == "unavailable" && !stop.IsSet && Current(false); retry++) {
                stop.Wait(50);
                baseline = Capture();
            }
            reason = Absent(baseline) ? null : "capture_not_absent";
            try { observeBaseline?.Invoke(new(false, "not_required", baseline?.State, 0,
                time.GetElapsedTime(began).TotalMilliseconds, time.GetElapsedTime(began).TotalMilliseconds, reason)); } catch { }
            if (input.AdoptExisting && Matched(baseline)) { binding = baseline.Identity; possibleStart = true; }
            else {
                if (!Absent(baseline)) return;
                if (!Current(false) || stop.IsSet) { reason = "voice_cancelled"; return; }
                if (input.Micro != null) {
                    // ACT10 is Codex's own Realtime Voice control. The exact process capture,
                    // rather than localized Desktop UI, confirms that the session started.
                    var microOnly = input with { HotkeyFallback = null };
                    plan = runtime.SelectVoiceInput(microOnly, waitForReady: true, () => !stop.IsSet && current(false));
                    if (plan.Method == "micro" && plan.Generation != null && plan.Device != null)
                        microBaseline = plan.Device.ObserveVoice(plan.Generation.Value);
                    dispatch = Dispatch(false, ref plan, microOnly);
                    if (dispatch?.Phase == "not_submitted" && input.HotkeyFallback != null) {
                        plan = PhoneDesktopRuntime.FallbackInput();
                        dispatch = Dispatch(false, ref plan);
                    }
                } else {
                    plan = runtime.SelectVoiceInput(input, waitForReady: true, () => !stop.IsSet && current(false));
                    dispatch = Dispatch(false, ref plan);
                }
                possibleStart = dispatch?.Phase is "submitted" or "outcome_unknown";
                if (dispatch?.Phase != "submitted") { reason = possibleStart ? "input_outcome_unknown" : "input_not_submitted"; return; }
            }
            reason = "capture_unconfirmed"; began = time.GetTimestamp();
            DesktopCaptureIdentity candidate = null;
            long? matchedSince = null, dictationSeenAt = null;
            while (binding == null && !stop.IsSet && Current(false) && time.GetElapsedTime(began) < TimeSpan.FromSeconds(30)) {
                var capture = Capture();
                if (plan?.Method == "micro" && microBaseline != null && plan.Generation != null && plan.Device != null) {
                    var observed = plan.Device.ObserveVoice(plan.Generation.Value);
                    if (observed.Generation == microBaseline.Generation && observed.DictationSequence > microBaseline.DictationSequence)
                        dictationSeenAt ??= time.GetTimestamp();
                }
                if (dictationSeenAt != null) {
                    reason = "micro_dictation_detected";
                    if (input.HotkeyFallback == null) { possibleStart = false; return; }
                    if (Absent(capture)) {
                        var priorMicro = dispatch?.Micro;
                        possibleStart = false;
                        plan = PhoneDesktopRuntime.FallbackInput();
                        dispatch = DispatchDictationFallback(ref plan, priorMicro);
                        possibleStart = dispatch?.Phase is "submitted" or "outcome_unknown";
                        if (dispatch?.Phase != "submitted") { reason = possibleStart ? "input_outcome_unknown" : "input_not_submitted"; return; }
                        reason = "capture_unconfirmed"; began = time.GetTimestamp();
                        microBaseline = null; dictationSeenAt = null; candidate = null; matchedSince = null;
                        continue;
                    }
                    if (time.GetElapsedTime(dictationSeenAt.Value) >= TimeSpan.FromSeconds(5)) { possibleStart = false; return; }
                    stop.Wait(50); continue;
                }
                if (Matched(capture)) {
                    if (plan?.Method != "micro") binding = capture.Identity;
                    else {
                        if (candidate != capture.Identity) { candidate = capture.Identity; matchedSince = time.GetTimestamp(); }
                        if (matchedSince != null && time.GetElapsedTime(matchedSince.Value) >= TimeSpan.FromMilliseconds(500) &&
                            time.GetElapsedTime(began) >= TimeSpan.FromMilliseconds(750)) binding = capture.Identity;
                    }
                } else {
                    candidate = null; matchedSince = null;
                    if (capture?.State == "ambiguous") break;
                }
                if (binding == null) stop.Wait(50);
            }
            if (binding == null || stop.IsSet || !Current(false)) return;
            port.BindAuthority(endpoint); port.GrantAuthority();
            Volatile.Write(ref confirmedCapture, binding);
            port.ArmVoiceRecovery();
            started.TrySetResult(new("active", dispatch));
            long? missingSince = null, uncertainSince = null;
            bool suspended = false;
            while (!stop.Wait(1000) && Current(false)) {
                var capture = Capture();
                if (Matched(capture) && capture.Identity == binding) {
                    missingSince = uncertainSince = null;
                    if (suspended) { port.GrantAuthority(); suspended = false; }
                    continue;
                }
                if (!suspended) { port.ClearAuthority(); suspended = true; }
                if (capture?.State == "none") {
                    missingSince ??= time.GetTimestamp(); uncertainSince = null;
                    if (time.GetElapsedTime(missingSince.Value) >= TimeSpan.FromSeconds(10)) {
                        reason = "voice_capture_ended"; break;
                    }
                } else {
                    uncertainSince ??= time.GetTimestamp(); missingSince = null;
                    if (time.GetElapsedTime(uncertainSince.Value) >= TimeSpan.FromSeconds(5)) {
                        reason = "capture_owner_changed"; break;
                    }
                }
            }
        } catch { reason = "voice_observation_failed"; }
        finally {
            lock (sync) { if (preserveAudio) port.ClearAuthority(); else port.Revoke(); }
            try { if (possibleStart) ending = Cleanup(binding, ref plan); }
            catch { ending = new("outcome_unknown", null); }
            started.TrySetResult(new(possibleStart ? "outcome_unknown" : "not_started", dispatch, reason));
            finished.TrySetResult(ending);
        }
    }
    private VoiceDispatchResult Dispatch(bool cleanup, ref VoiceInputPlan plan, NativeVoiceInput settings) {
        if (!runtime.PrepareInputFocus(plan, settings, desktop, () => Current(cleanup))) return null;
        return runtime.DispatchVoice(plan, settings, desktop, cleanup,
            () => (cleanup || !stop.IsSet) && current(cleanup), () => current(true));
    }
    private VoiceLifetimeResult Cleanup(DesktopCaptureIdentity binding, ref VoiceInputPlan plan) {
        // Never send input to a replacement Desktop after the original process has exited.
        if (!runtime.IsCurrent(desktop)) return new(runtime.HasExited(desktop) ? "stopped" : "outcome_unknown", null);
        if (!current(true)) return new("outcome_unknown", null);
        var capture = Capture();
        if (plan?.Method == "micro") {
            // Capture can disappear just before Codex has closed Realtime Voice. Always send
            // its explicit long-ACT10 end command for the session that this call started.
            if (Matched(capture) && binding != null && capture.Identity != binding || capture?.State == "ambiguous")
                return new("outcome_unknown", null);
            if (plan.Device?.Status.Generation != plan.Generation)
                plan = runtime.SelectVoiceInput(input with { HotkeyFallback = null }, waitForReady: false, () => Current(true));
            var invoked = Dispatch(true, ref plan, input with { HotkeyFallback = null });
            if (invoked?.Phase != "submitted") return new("outcome_unknown", invoked);
            long invokedAt = time.GetTimestamp();
            while (time.GetElapsedTime(invokedAt) < TimeSpan.FromSeconds(5)) {
                if (runtime.HasExited(desktop)) return new("stopped", invoked);
                if (Current(true) && Absent(Capture())) return new("stopped", invoked);
                Thread.Sleep(100);
            }
            // Some Desktop releases accept Micro's long ACT10 command without ending
            // Realtime Voice. Only after the original, exact capture remains bound may
            // the explicitly configured hotkey provide a second cleanup path.
            capture = Capture();
            if (input.HotkeyFallback == null || !Current(true) || !Matched(capture) || binding == null || capture.Identity != binding)
                return new("outcome_unknown", invoked);
            plan = PhoneDesktopRuntime.FallbackInput();
            var fallback = Dispatch(true, ref plan, input);
            if (fallback?.Phase != "submitted") return new("outcome_unknown", fallback);
            invokedAt = time.GetTimestamp();
            while (time.GetElapsedTime(invokedAt) < TimeSpan.FromSeconds(5)) {
                if (runtime.HasExited(desktop)) return new("stopped", fallback);
                if (Current(true) && Absent(Capture())) return new("stopped", fallback);
                Thread.Sleep(100);
            }
            return new("outcome_unknown", fallback);
        }
        if (plan == null && Absent(capture)) return new("stopped", null);
        if (capture?.State == "ambiguous" || Matched(capture) && binding != null && capture.Identity != binding ||
            plan == null && !Matched(capture)) return new("outcome_unknown", null);
        // A call cancelled immediately after its own start can first correlate capture here.
        // A HID reconnect may occur during a long call. A new stop gesture may use its
        // current generation after the exact original capture was confirmed above.
        // The start gesture's delayed key-up still stays bound to its own generation.
        if (plan == null || plan.Method == "micro" && plan.Device?.Status.Generation != plan.Generation)
            plan = runtime.SelectVoiceInput(input, waitForReady: false, () => Current(true));
        var result = Dispatch(true, ref plan);
        if (result?.Phase != "submitted") return new("outcome_unknown", result);
        long began = time.GetTimestamp();
        while (time.GetElapsedTime(began) < TimeSpan.FromSeconds(5)) {
            if (runtime.HasExited(desktop) || Current(true) && Absent(Capture())) return new("stopped", result);
            Thread.Sleep(100);
        }
        return new("outcome_unknown", result);
    }
    public ValueTask DisposeAsync() { lock (sync) return new(disposal ??= DisposeCoreAsync()); }
    private async Task DisposeCoreAsync() {
        await Task.Yield(); var result = await StopAsync();
        // The stop receipt describes the original gesture. Capture may become absent
        // after its five-second observation window; a later exact observation can
        // safely release the owner without sending another Voice command.
        if (result.State == "outcome_unknown") {
            bool absent = false;
            for (int attempt = 0; attempt < 20; attempt++) {
                if (runtime.HasExited(desktop) || Current(true) && Absent(Capture())) { absent = true; break; }
                await Task.Delay(100);
            }
            if (!absent) throw new InvalidOperationException("Voice cleanup remains unknown.");
        }
        lock (sync) { disposed = true; stop.Dispose(); }
    }
}
