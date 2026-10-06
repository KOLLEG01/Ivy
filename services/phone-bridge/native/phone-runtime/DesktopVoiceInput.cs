using System.Diagnostics;
using System.Text.Json;

namespace Ivy.PhoneBridge;

public sealed record NativeVoiceInput(MicroSettings Micro, NativeHotkey HotkeyFallback = null, bool AdoptExisting = false) {
    internal static NativeVoiceInput Read(JsonElement value) {
        var names = value.EnumerateObject().Select(property => property.Name).ToHashSet(StringComparer.Ordinal);
        names.Remove("adoptExisting");
        if (!names.SetEquals(new[] { "micro" }) && !names.SetEquals(new[] { "micro", "hotkeyFallback" }))
            throw new ArgumentException("Exact Voice input settings required.");
        var microValue = value.GetProperty("micro");
        var micro = microValue.ValueKind == JsonValueKind.Null ? null : NativeSipOperations.Read<MicroSettings>(microValue, "usbipExecutable", "executableHash", "port");
        NativeHotkey fallback = null;
        if (value.TryGetProperty("hotkeyFallback", out var hotkey) && hotkey.ValueKind != JsonValueKind.Null)
            fallback = NativeSipOperations.Read<NativeHotkey>(hotkey, "modifiers", "keyCode", "focusBeforeHotkey");
        bool adopt = value.TryGetProperty("adoptExisting", out var existing) && existing.GetBoolean();
        return new NativeVoiceInput(micro, fallback, adopt).Snapshot();
    }
    internal bool Matches(NativeVoiceInput other) => other != null && Micro == other.Micro && AdoptExisting == other.AdoptExisting &&
        (HotkeyFallback == null ? other.HotkeyFallback == null : other.HotkeyFallback != null &&
            HotkeyFallback.KeyCode == other.HotkeyFallback.KeyCode && HotkeyFallback.FocusBeforeHotkey == other.HotkeyFallback.FocusBeforeHotkey &&
            HotkeyFallback.Modifiers.SequenceEqual(other.HotkeyFallback.Modifiers));
    public NativeVoiceInput Snapshot() {
        Micro?.Validate();
        if (HotkeyFallback != null) VoiceHotkey.Plan(HotkeyFallback.Modifiers, HotkeyFallback.KeyCode);
        return this with { HotkeyFallback = HotkeyFallback == null ? null :
            HotkeyFallback with { Modifiers = (string[])HotkeyFallback.Modifiers.Clone() } };
    }
}
public sealed record VoiceDispatchResult(string Phase, string Method, HotkeyResult Hotkey, MicroGestureResult Micro);
internal sealed record VoiceInputPlan(string Method, long? Generation, IMicroVoiceInput Device);

public sealed partial class PhoneDesktopRuntime {
    private readonly object inputSync = new();
    private readonly Func<MicroSettings, IMicroVoiceInput> microFactory;
    private IMicroVoiceInput microInput;
    private MicroSettings selectedMicro;
    private bool inputClosed;
    private bool microAttempted;
    private long microRetryAfter;
    private Task inputDisposal;
    private string microError;

    internal MicroAttachmentStatus MicroStatus {
        get {
            lock (inputSync) {
                RetryConflictingMicro();
                return microInput?.Status ?? (microAttempted ? new("unavailable", null, microError ?? "micro_client_unavailable") : null);
            }
        }
    }
    internal void PrepareMicro(MicroSettings settings) => SelectVoiceInput(new(settings), false, () => true);

    internal VoiceInputPlan SelectVoiceInput(NativeVoiceInput settings, bool waitForReady, Func<bool> original) {
        if (settings.Micro == null) return settings.HotkeyFallback != null ? new("hotkey", null, null) : new("micro", null, null);
        IMicroVoiceInput device;
        lock (inputSync) {
            if (inputClosed) return new("micro", null, null);
            if (selectedMicro != null && selectedMicro != settings.Micro) throw new InvalidOperationException("Micro configuration changed within the native owner.");
            if (!microAttempted || microInput == null && microError == "micro_attachment_conflict" && Environment.TickCount64 >= microRetryAfter) {
                selectedMicro = settings.Micro; microAttempted = true;
                try { microInput = microFactory(settings.Micro); }
                catch (Exception error) when (error is IOException or UnauthorizedAccessException or System.Net.Sockets.SocketException) {
                    microError = error is System.Net.Sockets.SocketException ? "micro_attachment_conflict" : "micro_client_unavailable";
                    if (error is System.Net.Sockets.SocketException) microRetryAfter = Environment.TickCount64 + 1000;
                }
            }
            device = microInput;
        }
        var began = Stopwatch.StartNew();
        for (;;) {
            if (!original()) return new("micro", null, null);
            if (device == null) break;
            var status = device.Status;
            if (status.State == "ready" && status.Generation != null) return new("micro", status.Generation, device);
            if (!waitForReady || status.State is not ("starting" or "attached") || began.Elapsed >= TimeSpan.FromSeconds(10)) break;
            Thread.Sleep(50);
        }
        return settings.HotkeyFallback != null ? new("hotkey", null, null) : new("micro", null, device);
    }
    private void RetryConflictingMicro() {
        if (inputClosed || microInput != null || selectedMicro == null || microError != "micro_attachment_conflict" || Environment.TickCount64 < microRetryAfter) return;
        try { microInput = microFactory(selectedMicro); microError = null; }
        catch (System.Net.Sockets.SocketException) { microRetryAfter = Environment.TickCount64 + 1000; }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { microError = "micro_client_unavailable"; }
    }
    internal static VoiceInputPlan FallbackInput() => new("hotkey", null, null);
    internal bool PrepareInputFocus(VoiceInputPlan plan, NativeVoiceInput settings, DesktopIdentity desktop, Func<bool> original) =>
        plan.Method switch {
            // Codex routes ACT10 to its own primary window. Foreground focus is neither
            // required nor part of the Micro protocol.
            "micro" => original() && IsCurrent(desktop),
            "hotkey" => settings.HotkeyFallback?.FocusBeforeHotkey != true || PrepareVoiceFocus(desktop, original),
            _ => false
        };

    internal VoiceDispatchResult DispatchVoice(VoiceInputPlan plan, NativeVoiceInput settings, DesktopIdentity desktop, bool cleanup,
        Func<bool> beforePress, Func<bool> releaseAuthority, MicroGestureResult priorMicro = null, bool priorMicroWasDictation = false) {
        if (plan.Method == "micro") {
            if (plan.Generation == null || plan.Device == null) return new("not_submitted", "micro", null, null);
            var result = MicroVoiceGesture.Dispatch(plan.Device, plan.Generation.Value, cleanup,
                () => IsCurrent(desktop) && beforePress(), () => IsCurrent(desktop) && releaseAuthority());
            return new(result.Phase, "micro", null, result);
        }
        if (settings.HotkeyFallback == null || priorMicro != null && priorMicro.Phase != "not_submitted" && !priorMicroWasDictation)
            throw new InvalidOperationException("Hotkey fallback requires explicit configuration and no unknown Micro effect.");
        var hotkey = VoiceToggle(desktop, settings.HotkeyFallback, beforePress, focusAlreadyPrepared: true);
        return new(hotkey.phase, "hotkey", hotkey, priorMicro);
    }
    public ValueTask DisposeAsync() {
        lock (inputSync) {
            inputClosed = true;
            return new(inputDisposal ??= microInput?.DisposeAsync().AsTask() ?? Task.CompletedTask);
        }
    }
}
