using System.Globalization;

namespace Ivy.PhoneBridge;

public sealed record VoiceAudioBinding(string CallId, string EndpointId, VoiceControlIdentity Voice, DesktopCaptureIdentity Capture);

// The admitted controller selects the binding after its own start. This class never discovers,
// starts, replaces or archives Voice, and never treats a caller-supplied IPC value as evidence.
public sealed class CallVoiceAuthority {
    private readonly object sync = new();
    private readonly CallAudioPort port;
    private readonly VoiceAudioBinding binding;
    private readonly Func<bool> current;
    private readonly TimeProvider time;
    private readonly bool revokePortOnLoss;
    private bool revoked, observed;
    private long lastVoice, lastCapture;
    public CallVoiceAuthority(CallAudioPort port, VoiceAudioBinding binding, Func<bool> current, TimeProvider time = null, bool revokePortOnLoss = true) {
        ArgumentNullException.ThrowIfNull(port); ArgumentNullException.ThrowIfNull(binding); ArgumentNullException.ThrowIfNull(current);
        CaptureObservation.ValidateText(binding.EndpointId);
        if (port.CallId != binding.CallId || !port.IsPreparedFor(binding.EndpointId) || binding.Voice == null || binding.Capture == null)
            throw new ArgumentException("Original prepared audio call and exact binding required.");
        var voice = binding.Voice; var capture = binding.Capture;
        bool Id(int[] value) => value != null && value.Length is >= 1 and <= 32;
        if (!long.TryParse(voice.Window, NumberStyles.None, CultureInfo.InvariantCulture, out long window) || window <= 0 ||
            window.ToString(CultureInfo.InvariantCulture) != voice.Window || !Id(voice.ParentId) || !Id(voice.MicrophoneId) || !Id(voice.StopId) ||
            voice.MicrophoneId.SequenceEqual(voice.StopId) || voice.ParentId.SequenceEqual(voice.MicrophoneId) || voice.ParentId.SequenceEqual(voice.StopId))
            throw new ArgumentException("Exact original Voice control identity required.");
        CaptureObservation.ValidateText(capture.InstanceId);
        if (capture.Pid <= 0 || !long.TryParse(capture.ProcessStartTimeUtcTicks, NumberStyles.None, CultureInfo.InvariantCulture, out long ticks) ||
            ticks <= 0 || ticks.ToString(CultureInfo.InvariantCulture) != capture.ProcessStartTimeUtcTicks)
            throw new ArgumentException("Exact original capture process identity required.");
        this.port = port; this.current = current; this.time = time ?? TimeProvider.System; this.revokePortOnLoss = revokePortOnLoss;
        this.binding = binding with { Voice = new(voice.Window, (int[])voice.ParentId.Clone(), (int[])voice.MicrophoneId.Clone(), (int[])voice.StopId.Clone()) };
        port.BindAuthority(binding.EndpointId);
    }
    private bool SameVoice(VoiceControlIdentity voice) => voice != null && voice.Window == binding.Voice.Window &&
        voice.ParentId != null && voice.MicrophoneId != null && voice.StopId != null &&
        voice.ParentId.SequenceEqual(binding.Voice.ParentId) && voice.MicrophoneId.SequenceEqual(binding.Voice.MicrophoneId) &&
        voice.StopId.SequenceEqual(binding.Voice.StopId);
    public bool Update(TimedVoiceControlObservation voice, long captureTimestamp, DesktopCaptureCorrelation capture) {
        lock (sync) {
            if (revoked) return false;
            try {
                if (!current()) { Revoke(); return false; }
                long now = time.GetTimestamp();
                bool Fresh(long timestamp) { var age = time.GetElapsedTime(timestamp, now); return age >= TimeSpan.Zero && age < TimeSpan.FromMilliseconds(500); }
                if (voice == null || !Fresh(voice.Timestamp) || !Fresh(captureTimestamp) ||
                    (observed && (voice.Timestamp <= lastVoice || captureTimestamp <= lastCapture))) {
                    port.ClearAuthority(); return false;
                }
                observed = true; lastVoice = voice.Timestamp; lastCapture = captureTimestamp;
                if (voice.Observation?.State != "active" || !SameVoice(voice.Observation.Identity) ||
                    capture?.State != "matched" || capture.EndpointId != binding.EndpointId || capture.Identity != binding.Capture ||
                    !port.IsPreparedFor(binding.EndpointId)) { port.ClearAuthority(); return false; }
                if (!current()) { Revoke(); return false; }
                port.RefreshAuthority(Math.Min(voice.Timestamp, captureTimestamp));
                if (!current()) { Revoke(); return false; }
                return port.IsOpen;
            } catch { Revoke(); return false; }
        }
    }
    public void Revoke() { lock (sync) { revoked = true; if (revokePortOnLoss) port.Revoke(); else port.ClearAuthority(); } }
}
