using System.Text.Json;

namespace Ivy.PhoneBridge;

public sealed record NativeDesktopApplication(string AppUserModelId, bool StartIfMissing);
public sealed record NativeHotkey(string[] Modifiers, int KeyCode, bool FocusBeforeHotkey);

// Only OS primitives. A chord result does not identify Voice or establish media authority.
public sealed partial class PhoneDesktopRuntime : IAsyncDisposable {
    private readonly Func<string, IDesktopApplication> application;
    private readonly Func<DesktopIdentity, IDesktopOwner> owner;
    private readonly Func<IKeyboardInput> keyboard;
    private readonly Func<string, CaptureObservation> capture;
    private readonly Func<DesktopIdentity, int, AudioProcessOwnership> process;
    private readonly Func<DesktopIdentity, string[], DesktopControlsObservation> controls;
    private readonly bool customControls;
    private readonly Func<DesktopIdentity, string, DesktopCaptureCorrelation> captureOwner;
    private readonly Func<DesktopIdentity, IDesktopVoiceReader> voiceReader;
    private readonly Func<DesktopIdentity, bool> exited;
    public PhoneDesktopRuntime(Func<string, IDesktopApplication> application = null,
        Func<DesktopIdentity, IDesktopOwner> owner = null, Func<IKeyboardInput> keyboard = null,
        Func<string, CaptureObservation> capture = null, Func<DesktopIdentity, int, AudioProcessOwnership> process = null,
        Func<DesktopIdentity, string[], DesktopControlsObservation> controls = null,
        Func<DesktopIdentity, string, DesktopCaptureCorrelation> captureOwner = null,
        Func<DesktopIdentity, IDesktopVoiceReader> voiceReader = null,
        Func<MicroSettings, IMicroVoiceInput> microFactory = null,
        Func<DesktopIdentity, bool> exited = null) {
        this.application = application ?? (id => new WindowsDesktopApplication(id));
        this.owner = owner ?? (identity => identity.Owner());
        this.keyboard = keyboard ?? (() => new WindowsKeyboardInput());
        this.capture = capture ?? WindowsCaptureObservation.Observe;
        this.process = process ?? WindowsAudioProcessOwnership.Observe;
        customControls = controls != null; this.controls = controls ?? WindowsDesktopControls.Observe;
        this.captureOwner = captureOwner ?? DesktopCapture.Observe;
        this.voiceReader = voiceReader ?? WindowsDesktopControls.OpenVoice;
        this.microFactory = microFactory ?? (settings => new MicroAttachment(settings));
        this.exited = exited ?? ProcessExited;
    }
    internal bool IsCurrent(DesktopIdentity desktop) => owner(desktop).IsCurrent();
    internal bool HasExited(DesktopIdentity desktop) => exited(desktop);
    private static bool ProcessExited(DesktopIdentity desktop) {
        try {
            using var process = System.Diagnostics.Process.GetProcessById(desktop.pid);
            return process.HasExited || process.StartTime.ToUniversalTime().Ticks.ToString(System.Globalization.CultureInfo.InvariantCulture) != desktop.startTimeUtcTicks;
        } catch (ArgumentException) { return true; }
        catch { return false; } // Unavailable observation does not prove process exit.
    }
    internal TimedVoiceControlObservation VoiceControls(DesktopIdentity desktop, TimeProvider time, Action onReadStarted = null) {
        if (!customControls) {
            var observed = WindowsDesktopControls.ObserveTimed(desktop, DesktopVoiceControls.Labels, time, onReadStarted: onReadStarted);
            return new(observed.Timestamp, DesktopVoiceControls.Classify(observed.Observation));
        }
        long timestamp = time.GetTimestamp();
        onReadStarted?.Invoke();
        return new(timestamp, DesktopVoiceControls.Classify(controls(desktop, DesktopVoiceControls.Labels)));
    }
    internal DesktopCaptureCorrelation VoiceCapture(DesktopIdentity desktop, string endpointId) => captureOwner(desktop, endpointId);
    internal IDesktopVoiceReader OpenVoice(DesktopIdentity desktop) => voiceReader(desktop);
    internal bool PrepareVoiceFocus(DesktopIdentity desktop, Func<bool> current) => new CurrentOwner(owner(desktop), current).TryFocus();
    internal HotkeyResult VoiceToggle(DesktopIdentity desktop, NativeHotkey hotkey, Func<bool> current, bool focusAlreadyPrepared = false) =>
        VoiceHotkey.Dispatch(new CurrentOwner(owner(desktop), current), keyboard(), hotkey.Modifiers, hotkey.KeyCode, hotkey.FocusBeforeHotkey, focusAlreadyPrepared);
    internal static object Identity(DesktopIdentity value) => value == null ? null :
        new { value.pid, value.startTimeUtcTicks, value.imagePath, value.appUserModelId };
    public Task<object> ObserveAsync(string appUserModelId) {
        DesktopLaunch.ValidateApplication(appUserModelId);
        return Task.Run<object>(() => {
            var observed = application(appUserModelId).Observe();
            if (observed == null) observed = DesktopObservation.Unavailable();
            return new { observed.state, identity = Identity(observed.identity) };
        });
    }
    public Task<object> CaptureAsync(string endpointId) {
        CaptureObservation.ValidateText(endpointId);
        return Task.Run<object>(() => {
            var observed = capture(endpointId);
            return new { observed.endpointId, observed.state,
                sessions = observed.sessions.Select(session => new { session.instanceId, session.state, session.pid, session.singleProcess }).ToArray() };
        });
    }
    public Task<object> ProcessAsync(DesktopIdentity desktop, int pid) {
        ArgumentNullException.ThrowIfNull(desktop);
        if (pid <= 0) throw new ArgumentException("Positive audio process ID required.");
        return Task.Run<object>(() => { var observed = process(desktop, pid); return new { observed.pid, observed.state }; });
    }
    public Task<object> CaptureOwnerAsync(DesktopIdentity desktop, string endpointId) {
        ArgumentNullException.ThrowIfNull(desktop); CaptureObservation.ValidateText(endpointId);
        return Task.Run<object>(() => captureOwner(desktop, endpointId));
    }
    public Task<object> ControlsAsync(DesktopIdentity desktop, string[] labels) {
        ArgumentNullException.ThrowIfNull(desktop); DesktopControls.ValidateLabels(labels);
        labels = (string[])labels.Clone();
        return Task.Run<object>(() => controls(desktop, labels));
    }
    private sealed class CurrentApplication(IDesktopApplication application, Func<bool> current) : IDesktopApplication {
        public DesktopObservation Observe() {
            if (!current()) return DesktopObservation.Unavailable();
            var result = application.Observe(); return current() ? result : DesktopObservation.Unavailable();
        }
        public int Activate() {
            if (!current()) throw new InvalidOperationException("Original call ended before activation.");
            return application.Activate();
        }
    }
    private sealed class CurrentOwner(IDesktopOwner owner, Func<bool> current) : IDesktopOwner {
        public bool IsCurrent() => current() && owner.IsCurrent() && current();
        public bool TryFocus() => IsCurrent() && owner.TryFocus() && IsCurrent();
        public bool IsFocused() => IsCurrent() && owner.IsFocused() && IsCurrent();
    }
    internal object Launch(NativeDesktopApplication config, Func<bool> current) {
        var result = DesktopLaunch.Dispatch(new CurrentApplication(application(config.AppUserModelId), current), config.StartIfMissing);
        return new { result.phase, identity = Identity(result.identity), result.activationPid, result.errorCode };
    }
    internal object Hotkey(DesktopIdentity desktop, NativeHotkey hotkey, Func<bool> current) {
        var result = VoiceToggle(desktop, hotkey, current);
        return new { result.phase, result.requested, result.submitted, result.keyUpSubmitted, result.errorCode };
    }
}

// Per-call resource lifetime only; original outcomes remain in NativeRpc and PhoneJournal.
public sealed class CallDesktopSession : IAsyncDisposable {
    private readonly object sync = new();
    private readonly string callId;
    private readonly PhoneDesktopRuntime runtime;
    private readonly Func<bool, bool> current;
    private readonly CallAudioPort audio;
    private CallVoiceSession voice;
    private DesktopIdentity voiceDesktop;
    private NativeVoiceInput voiceInput;
    private readonly Dictionary<string, Task<object>> work = new(StringComparer.Ordinal);
    private bool closed;
    private Task closing;
    private int voiceGeneration;
    private bool voicePaused;
    public CallDesktopSession(string callId, PhoneDesktopRuntime runtime, Func<bool, bool> current, CallAudioPort audio = null) {
        if (!Guid.TryParseExact(callId, "D", out _)) throw new ArgumentException("Original call required.");
        this.callId = callId; this.runtime = runtime; this.current = current; this.audio = audio;
    }
    public Task<object> Invoke(string method, JsonElement args) {
        string action = method["call.desktop.".Length..];
        if (action is "pauseVoice" or "resumeVoice") return Transition(action, args);
        Func<bool, object> dispatch = null; DesktopIdentity requestedDesktop = null; NativeVoiceInput requestedInput = null;
        if (action == "launch") {
            NativeSipOperations.Read<JsonElement>(args, "callId", "application");
            var config = NativeSipOperations.Read<NativeDesktopApplication>(args.GetProperty("application"), "appUserModelId", "startIfMissing");
            DesktopLaunch.ValidateApplication(config.AppUserModelId);
            dispatch = _ => runtime.Launch(config, () => Current(false));
        } else if (action is "startVoice" or "stopVoice") {
            NativeSipOperations.Read<JsonElement>(args, "callId", "desktop", "voiceInput");
            var desktop = NativeSipOperations.Read<NativeDesktopIdentity>(args.GetProperty("desktop"), "pid", "startTimeUtcTicks", "imagePath", "appUserModelId").ToIdentity();
            requestedDesktop = desktop; requestedInput = NativeVoiceInput.Read(args.GetProperty("voiceInput"));
        } else throw new ArgumentException("Unknown Desktop call action.");
        if (args.GetProperty("callId").GetString() != callId) throw new NativeRpcException("runtime_not_ready");
        bool cleanup = action == "stopVoice";
        lock (sync) {
            if (closed) throw new NativeRpcException("runtime_stopping");
            if (work.ContainsKey(action)) throw new NativeRpcException("operation_conflict");
            // Cleanup can cancel an in-flight start. Other actions retain serial ownership.
            if (!cleanup && work.Values.Any(task => !task.IsCompleted)) throw new NativeRpcException("runtime_not_ready");
            Task<object> pending;
            if (action == "startVoice") {
                if (work.ContainsKey("stopVoice")) throw new NativeRpcException("operation_conflict");
                if (!Current(false) || audio == null) throw new NativeRpcException("runtime_not_ready");
                voiceDesktop = requestedDesktop; voiceInput = requestedInput.Snapshot();
                voice = new CallVoiceSession(audio, voiceDesktop, voiceInput, runtime, isCleanup => (isCleanup || !Volatile.Read(ref closed)) && current(isCleanup));
                pending = VoiceResult(voice.StartAsync(), action);
            } else if (action == "stopVoice") {
                if (voice != null && (requestedDesktop.pid != voiceDesktop.pid || requestedDesktop.startTimeUtcTicks != voiceDesktop.startTimeUtcTicks ||
                    requestedDesktop.appUserModelId != voiceDesktop.appUserModelId || !StringComparer.OrdinalIgnoreCase.Equals(requestedDesktop.imagePath, voiceDesktop.imagePath) ||
                    !voiceInput.Matches(requestedInput))) throw new ArgumentException("Cleanup must retain the original Desktop and Voice input settings.");
                audio?.Revoke();
                pending = VoiceResult(voice?.StopAsync() ?? Task.FromResult(new VoiceLifetimeResult("not_started", null)), action);
            } else pending = Task.Run<object>(() => {
                if (!Current(false)) throw new NativeRpcException("runtime_not_ready");
                return new { callId, action, result = dispatch(false) };
            });
            work.Add(action, pending); return pending;
        }
    }
    private Task<object> Transition(string action, JsonElement args) {
        NativeSipOperations.Read<JsonElement>(args, "callId", "desktop", "voiceInput", "generation");
        int generation = args.GetProperty("generation").GetInt32();
        var requestedDesktop = NativeSipOperations.Read<NativeDesktopIdentity>(args.GetProperty("desktop"), "pid", "startTimeUtcTicks", "imagePath", "appUserModelId").ToIdentity();
        var requestedInput = NativeVoiceInput.Read(args.GetProperty("voiceInput"));
        lock (sync) {
            if (closed || !Current(false) || voice == null || work.ContainsKey("stopVoice") || args.GetProperty("callId").GetString() != callId)
                throw new NativeRpcException("runtime_not_ready");
            if (generation is < 0 or > 128 || generation != voiceGeneration + (action == "resumeVoice" ? 1 : 0) ||
                work.ContainsKey(action + ":" + generation) || work.Values.Any(value => !value.IsCompleted)) throw new NativeRpcException("operation_conflict");
            if (requestedDesktop.pid != voiceDesktop.pid || requestedDesktop.startTimeUtcTicks != voiceDesktop.startTimeUtcTicks ||
                requestedDesktop.appUserModelId != voiceDesktop.appUserModelId || !StringComparer.OrdinalIgnoreCase.Equals(requestedDesktop.imagePath, voiceDesktop.imagePath) ||
                !voiceInput.Matches(requestedInput)) throw new ArgumentException("Voice transitions retain the original Desktop and input configuration.");
            Task<object> pending;
            if (action == "pauseVoice") {
                if (voicePaused) throw new NativeRpcException("operation_conflict");
                pending = PauseGeneration(voice, generation);
            } else {
                if (!voicePaused) throw new NativeRpcException("runtime_not_ready");
                voicePaused = false; voiceGeneration = generation;
                voice = new CallVoiceSession(audio, voiceDesktop, voiceInput with { AdoptExisting = false }, runtime,
                    cleanup => (cleanup || !Volatile.Read(ref closed)) && current(cleanup));
                pending = VoiceResult(voice.StartAsync(), action);
            }
            work.Add(action + ":" + generation, pending); return pending;
        }
    }
    private async Task<object> PauseGeneration(CallVoiceSession original, int generation) {
        var pending = original.PauseForTransitionAsync();
        await Task.Yield();
        var result = await pending;
        if (result.State == "stopped") {
            lock (sync) if (closed || work.ContainsKey("stopVoice") || !Current(false)) throw new NativeRpcException("runtime_not_ready");
            await original.RetireForTransitionAsync();
            lock (sync) {
                if (closed || work.ContainsKey("stopVoice") || !Current(false)) throw new NativeRpcException("runtime_not_ready");
                voicePaused = true;
            }
        }
        return new { callId, action = "pauseVoice", generation, result };
    }
    private bool Current(bool cleanup) => !Volatile.Read(ref closed) && current(cleanup);
    internal bool HasOriginalVoiceCapture(DesktopIdentity desktop) {
        CallVoiceSession selected;
        lock (sync) {
            if (closed || voicePaused || voice == null || voiceDesktop == null ||
                desktop.pid != voiceDesktop.pid || desktop.startTimeUtcTicks != voiceDesktop.startTimeUtcTicks ||
                desktop.appUserModelId != voiceDesktop.appUserModelId ||
                !StringComparer.OrdinalIgnoreCase.Equals(desktop.imagePath, voiceDesktop.imagePath)) return false;
            selected = voice;
        }
        return selected.HasOriginalCapture();
    }
    private async Task<object> VoiceResult(Task<VoiceLifetimeResult> task, string action) => new { callId, action, result = await task };
    public ValueTask DisposeAsync() {
        lock (sync) {
            if (closing != null) return new ValueTask(closing);
            Volatile.Write(ref closed, true);
            audio?.Revoke();
            closing = Task.WhenAll(Drain(work.Values.ToArray()), voice?.DisposeAsync().AsTask() ?? Task.CompletedTask); return new ValueTask(closing);
        }
    }
    private static async Task Drain(Task<object>[] tasks) {
        // Failure is already retained by the original RPC receipt. Completion still proves that
        // no original Windows call can enter later on behalf of a replacement SIP call.
        try { await Task.WhenAll(tasks); } catch { }
    }
}
