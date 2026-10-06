using System.Runtime.InteropServices;
using NAudio.CoreAudioApi;
using NAudio.Wave;

namespace Ivy.PhoneBridge;

public sealed record PhoneLoopbackProbeResult(string State, bool Detected, float Peak, long Samples, int SampleRate, int Channels, int ExcludedProcessId);

internal sealed class LoopbackProbeMeter {
    private readonly object sync = new();
    private float peak;
    private long samples;
    internal void Add(ReadOnlySpan<float> values, bool silent) {
        lock (sync) {
            if (values.Length > 192000 || samples + values.Length > 960000) throw new InvalidOperationException("Probe sample bound exceeded.");
            foreach (float value in values) {
                if (!silent && !float.IsFinite(value)) throw new InvalidOperationException("Invalid probe PCM.");
                if (!silent) peak = Math.Max(peak, Math.Min(1, Math.Abs(value)));
            }
            samples += values.Length;
        }
    }
    internal PhoneLoopbackProbeResult Result(string state) {
        lock (sync) return new(state, state == "completed" && peak > .0001f, peak, samples, 48000, 2, Environment.ProcessId);
    }
}

public static class PhoneLoopbackProbe {
    public static async Task<PhoneLoopbackProbeResult> RunAsync(Func<bool> idle, CancellationToken cancellationToken) {
        if (!OperatingSystem.IsWindowsVersionAtLeast(10, 0, 20348)) throw new PlatformNotSupportedException();
        var meter = new LoopbackProbeMeter();
        var stopped = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(12));
        // Only a recorder is opened. No player, microphone capture, file or audio upload exists.
        await using var recorder = await new WasapiRecorderBuilder()
            .WithFormat(WaveFormat.CreateIeeeFloatWaveFormat(48000, 2)).WithBufferLength(40)
            .WithProcessLoopback((uint)Environment.ProcessId, ProcessLoopbackMode.ExcludeTargetProcessTree).BuildAsync();
        deadline.Token.ThrowIfCancellationRequested();
        if (!idle()) return meter.Result("interrupted");
        recorder.DataAvailable += (data, flags, _, _) => {
            try { meter.Add(MemoryMarshal.Cast<byte, float>(data), (flags & AudioClientBufferFlags.Silent) != 0); }
            catch (Exception error) { stopped.TrySetException(error); }
        };
        recorder.RecordingStopped += (_, args) => {
            if (args.Exception != null) stopped.TrySetException(args.Exception); else stopped.TrySetResult();
        };
        string state = "completed";
        recorder.StartRecording();
        try {
            long end = Environment.TickCount64 + 5000;
            while (Environment.TickCount64 < end) {
                deadline.Token.ThrowIfCancellationRequested();
                if (!idle()) { state = "interrupted"; break; }
                if (stopped.Task.IsCompleted) { await stopped.Task; throw new InvalidOperationException("Probe capture stopped early."); }
                await Task.Delay(50, deadline.Token);
            }
        } finally { recorder.StopRecording(); }
        await stopped.Task.WaitAsync(TimeSpan.FromSeconds(3), deadline.Token);
        return meter.Result(state);
    }
}
