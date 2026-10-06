using System.Runtime.InteropServices;
using NAudio.CoreAudioApi;
using NAudio.Wave;
using NAudio.Wave.SampleProviders;

namespace Ivy.PhoneBridge;

public sealed record MediaStatus(string State, string SourceMode, int SampleRate, int Channels,
    long CaptureDroppedSamples, long RenderDroppedSamples, int CaptureQueuedSamples, int RenderQueuedSamples, double? RenderLatencyMs,
    bool? LowLatencyActive = null, string LowLatencyUnavailableReason = null);

// Owns only this call's WASAPI resources. The caller supplies a bounded, monotonic permit tied to
// its current call/session observations. Every audio read/write checks it; a positive OS session
// observation alone is never used as permission here.
public sealed class WindowsAudioRoute : ICallAudioRoute {
    private readonly object sync = new();
    private readonly AudioSettings settings;
    private readonly Func<bool> permitted;
    private readonly AudioQueue captured, rendered;
    private MMDevice renderDevice, captureDevice;
    private WasapiRecorder recorder;
    private WasapiPlayer player;
    private string state = "suspended";
    private long audioGeneration;
    private double? renderLatency;
    private bool? lowLatencyActive;
    private string lowLatencyUnavailableReason;
    private Task closing;
    private AudioMuteGuard muteGuard;

    private WindowsAudioRoute(AudioSettings settings, Func<bool> permitted) {
        settings.Validate(); ArgumentNullException.ThrowIfNull(permitted);
        this.settings = settings; this.permitted = permitted;
        captured = new AudioQueue(AudioSettings.SampleRate * settings.QueueMs / 1000);
        rendered = new AudioQueue(AudioSettings.SampleRate * settings.QueueMs / 1000,
            AudioSettings.SampleRate * settings.PlaybackPrebufferMs / 1000);
    }

    public static async Task<WindowsAudioRoute> OpenAsync(AudioSettings settings, DesktopIdentity desktop,
        Func<bool> permitted, CancellationToken cancellationToken) {
        if (settings.SourceMode is "system_excluding_runtime" or "desktop_process" && !OperatingSystem.IsWindowsVersionAtLeast(10, 0, 20348))
            throw new PlatformNotSupportedException("Phone process loopback requires Windows build20348 or later.");
        if (desktop == null && settings.SourceMode == "desktop_process") throw new ArgumentException("Desktop process capture requires its owner.");
        var route = new WindowsAudioRoute(settings, permitted);
        try {
            cancellationToken.ThrowIfCancellationRequested();
            if (desktop != null && !desktop.Owner().IsCurrent()) throw new InvalidOperationException("Desktop ownership unavailable.");
            using (var devices = new MMDeviceEnumerator()) {
                using var capture = devices.GetDevice(settings.CaptureEndpointId);
                if (capture.ID != settings.CaptureEndpointId || capture.DataFlow != DataFlow.Capture || capture.State != DeviceState.Active)
                    throw new InvalidOperationException("Configured capture endpoint unavailable.");
                route.renderDevice = devices.GetDevice(settings.RenderEndpointId);
                if (route.renderDevice.ID != settings.RenderEndpointId || route.renderDevice.DataFlow != DataFlow.Render || route.renderDevice.State != DeviceState.Active)
                    throw new InvalidOperationException("Configured render endpoint unavailable.");
            }
            if (settings.MutePolicy != null) route.muteGuard = await Task.Run(() => AudioMuteGuard.Start(settings.MutePolicy,
                settings.RenderEndpointId, () => { lock (route.sync) { if (route.state != "closed") { route.state = "failed"; route.captured.Clear(); route.rendered.Clear(); } } }));
            // Only an explicitly configured mute policy may change exact endpoint mute states.
            var playerBuilder = new WasapiPlayerBuilder().WithDevice(route.renderDevice).WithSharedMode()
                .WithLatency(settings.RenderLatencyMs).WithEventSync().WithMmcssThreadPriority("Pro Audio");
            if (settings.EnableWasapiLowLatency) playerBuilder.WithLowLatency(required: false);
            route.player = playerBuilder.Build();
            if (route.player.DeviceMixFormat.Channels is not (1 or 2)) throw new InvalidOperationException("Phone render endpoint must support mono or stereo.");
            ISampleProvider output = new RenderOutput(route, route.player.DeviceMixFormat.SampleRate, route.player.DeviceMixFormat.Channels);
            route.player.Init(new SampleToWaveProvider(output));
            route.renderLatency = route.player.LatencyMilliseconds;
            route.lowLatencyActive = route.player.LowLatencyActive;
            var unavailable = route.player.LowLatencyUnavailableReason;
            route.lowLatencyUnavailableReason = unavailable == null ? null : unavailable[..Math.Min(unavailable.Length, 1024)];
            route.player.PlaybackStopped += route.Stopped;
            var captureBuilder = new WasapiRecorderBuilder().WithFormat(WaveFormat.CreateIeeeFloatWaveFormat(AudioSettings.SampleRate, 2))
                .WithBufferLength(settings.CaptureBufferMs).WithMmcssThreadPriority("Pro Audio");
            if (settings.SourceMode is "microphone" or "virtual_speaker") {
                using var devices = new MMDeviceEnumerator();
                route.captureDevice = devices.GetDevice(settings.SourceMode == "microphone" ? settings.CaptureEndpointId : settings.LoopbackRenderEndpointId);
                if (route.captureDevice.State != DeviceState.Active || route.captureDevice.DataFlow !=
                    (settings.SourceMode == "microphone" ? DataFlow.Capture : DataFlow.Render))
                    throw new InvalidOperationException("Configured capture endpoint is unavailable.");
                captureBuilder.WithDevice(route.captureDevice);
                if (settings.SourceMode == "virtual_speaker") captureBuilder.WithLoopbackCapture();
            } else {
                uint targetPid = checked((uint)(settings.SourceMode == "desktop_process" ? desktop.pid : Environment.ProcessId));
                captureBuilder.WithProcessLoopback(targetPid, settings.SourceMode == "desktop_process"
                    ? ProcessLoopbackMode.IncludeTargetProcessTree : ProcessLoopbackMode.ExcludeTargetProcessTree);
            }
            route.recorder = await captureBuilder.BuildAsync();
            cancellationToken.ThrowIfCancellationRequested();
            if (desktop != null && !desktop.Owner().IsCurrent()) throw new InvalidOperationException("Desktop changed during audio preparation.");
            route.recorder.DataAvailable += route.Captured;
            route.recorder.RecordingStopped += route.Stopped;
            route.recorder.StartRecording(); route.player.Play();
            return route;
        } catch (Exception error) {
            try { await route.DisposeAsync(); }
            catch (Exception cleanup) { throw new AggregateException("Audio preparation cleanup is unconfirmed.", error, cleanup); }
            throw new AudioPreparationReleasedException(error);
        }
    }

    public MediaStatus Status {
        get { lock (sync) return new MediaStatus(state, settings.SourceMode, AudioSettings.SampleRate, 1,
            captured.Dropped, rendered.Dropped, captured.Count, rendered.Count, renderLatency, lowLatencyActive, lowLatencyUnavailableReason); }
    }
    public bool IsOpen { get { lock (sync) return CanTransfer(); } }
    public bool Failed { get { lock (sync) return state == "failed"; } }
    public long Generation { get { lock (sync) return audioGeneration; } }
    public void Resume() {
        lock (sync) {
            if (state is "closed" or "failed") throw new InvalidOperationException("Audio route cannot resume.");
            if (!Allowed()) throw new InvalidOperationException("Current call audio permit required.");
            if (state == "open") return;
            captured.Clear(); rendered.Clear(); audioGeneration++; state = "open";
        }
    }
    public void Suspend() { lock (sync) { if (state == "open") state = "suspended"; captured.Clear(); rendered.Clear(); } }
    private bool Allowed() { try { return permitted(); } catch { return false; } }
    private bool CanTransfer() {
        if (state != "open") return false;
        if (Allowed()) return true;
        state = "suspended"; captured.Clear(); rendered.Clear(); return false;
    }
    private void Fail() { lock (sync) { if (state != "closed") state = "failed"; captured.Clear(); rendered.Clear(); } }
    private void Stopped(object sender, StoppedEventArgs args) { Fail(); }
    private void Captured(ReadOnlySpan<byte> bytes, AudioClientBufferFlags flags, long position, long timestamp) {
        try {
            lock (sync) {
                if (!CanTransfer()) return;
                if (bytes.Length % 8 != 0) throw new InvalidOperationException("Unaligned capture frame.");
                var stereo = MemoryMarshal.Cast<byte, float>(bytes);
                Span<float> mono = stackalloc float[512];
                for (int offset = 0; offset < stereo.Length; offset += mono.Length * 2) {
                    int count = Math.Min(mono.Length, (stereo.Length - offset) / 2);
                    for (int i = 0; i < count; i++) {
                        float value = (flags & AudioClientBufferFlags.Silent) != 0 ? 0 : (stereo[offset + i * 2] + stereo[offset + i * 2 + 1]) * 0.5f;
                        if (!float.IsFinite(value)) throw new InvalidOperationException("Invalid capture sample.");
                        mono[i] = Math.Clamp(value, -1f, 1f);
                    }
                    captured.Write(mono[..count]);
                }
            }
        } catch { Fail(); }
    }
    public int ReadCaptured(Span<float> output) {
        lock (sync) {
            if (!CanTransfer()) { output.Clear(); return 0; }
            return captured.Read(output);
        }
    }
    public void WriteReceived(ReadOnlySpan<float> input) {
        lock (sync) {
            if (!CanTransfer()) return;
            foreach (float sample in input) if (!float.IsFinite(sample) || sample is < -1 or > 1) { Fail(); return; }
            rendered.Write(input);
        }
    }
    private int Render(Span<float> output) {
        lock (sync) {
            if (CanTransfer()) rendered.Read(output); else output.Clear();
            return output.Length; // WASAPI always receives a complete buffer, with silence on loss.
        }
    }
    private sealed class RenderSource(WindowsAudioRoute owner) : ISampleProvider {
        public WaveFormat WaveFormat { get; } = WaveFormat.CreateIeeeFloatWaveFormat(AudioSettings.SampleRate, 1);
        public int Read(Span<float> buffer) => owner.Render(buffer);
    }
    private sealed class RenderOutput(WindowsAudioRoute owner, int rate, int channels) : ISampleProvider {
        private long generation = -1;
        private ISampleProvider pipeline;
        public WaveFormat WaveFormat { get; } = WaveFormat.CreateIeeeFloatWaveFormat(rate, channels);
        public int Read(Span<float> buffer) {
            lock (owner.sync) {
                if (!owner.CanTransfer()) { pipeline = null; buffer.Clear(); return buffer.Length; }
                if (pipeline == null || generation != owner.audioGeneration) {
                    // Resamplers retain interpolation samples. Recreate on every resume so prior
                    // audio cannot emerge after the mono queues have already been cleared.
                    pipeline = new RenderSource(owner);
                    if (rate != AudioSettings.SampleRate) pipeline = new WdlResamplingSampleProvider(pipeline, rate);
                    if (channels == 2) pipeline = new MonoToStereoSampleProvider(pipeline);
                    generation = owner.audioGeneration;
                }
                int read = pipeline.Read(buffer); buffer[read..].Clear();
                if (!owner.CanTransfer()) { pipeline = null; buffer.Clear(); }
                return buffer.Length;
            }
        }
    }
    public ValueTask DisposeAsync() {
        lock (sync) {
            if (closing != null) return new ValueTask(closing);
            state = "closed"; captured.Clear(); rendered.Clear();
            closing = CloseDevicesAsync(); return new ValueTask(closing);
        }
    }
    private async Task CloseDevicesAsync() {
        // Begin asynchronously so audio callbacks never wait for a lock held by disposal.
        await Task.Yield();
        try {
            if (recorder != null) {
                recorder.DataAvailable -= Captured; recorder.RecordingStopped -= Stopped;
                await recorder.DisposeAsync();
            }
        } finally {
            try {
                if (player != null) { player.PlaybackStopped -= Stopped; await player.DisposeAsync(); }
            } finally {
                try { if (muteGuard != null) await muteGuard.DisposeAsync(); }
                finally { captureDevice?.Dispose(); renderDevice?.Dispose(); }
            }
        }
    }
}
