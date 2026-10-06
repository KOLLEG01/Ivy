using NAudio.CoreAudioApi;

namespace Ivy.PhoneBridge;

internal interface IAudioMuteEndpoint : IDisposable {
    string Id { get; }
    bool ActiveRender { get; }
    bool Muted { get; set; }
}

// Optional explicit endpoint policy. No default-device lookup and no restoration of stale
// Windows state on release. Shared call routes must agree on each endpoint's requested state.
internal sealed class AudioMuteGuard : IAsyncDisposable {
    private static readonly object leasesSync = new();
    private static readonly Dictionary<string, (bool Muted, int Owners)> leases = new(StringComparer.Ordinal);
    private readonly Dictionary<string, bool> desired = new(StringComparer.Ordinal);
    private readonly Func<string, IAudioMuteEndpoint> open;
    private readonly CancellationTokenSource stop = new();
    private readonly Task worker;
    private int disposed;
    private AudioMuteGuard(AudioMutePolicy policy, string receive, Action failed, Func<string, IAudioMuteEndpoint> open, TimeProvider time) {
        policy.Validate(receive); this.open = open;
        if (policy.MutedRenderEndpointId != null) desired.Add(policy.MutedRenderEndpointId, true);
        if (policy.UnmuteReceive) desired.Add(receive, false);
        lock (leasesSync) {
            if (desired.Any(pair => leases.TryGetValue(pair.Key, out var lease) && lease.Muted != pair.Value))
                throw new InvalidOperationException("Active calls require conflicting endpoint mute states.");
            foreach (var pair in desired) leases[pair.Key] = (pair.Value, leases.GetValueOrDefault(pair.Key).Owners + 1);
        }
        try { Enforce(); worker = RunAsync(policy.IntervalSeconds, failed, time); }
        catch { Release(); stop.Dispose(); throw; }
    }
    internal static AudioMuteGuard Start(AudioMutePolicy policy, string receive, Action failed,
        Func<string, IAudioMuteEndpoint> open = null, TimeProvider time = null) =>
        new(policy, receive, failed, open ?? (id => new WindowsEndpoint(id)), time ?? TimeProvider.System);
    private void Enforce() {
        foreach (var pair in desired) {
            using var endpoint = open(pair.Key);
            if (endpoint.Id != pair.Key || !endpoint.ActiveRender) throw new InvalidOperationException("Original mute-policy endpoint is unavailable.");
            if (endpoint.Muted != pair.Value) endpoint.Muted = pair.Value;
            if (endpoint.Muted != pair.Value) throw new InvalidOperationException("Windows did not confirm the requested endpoint mute state.");
        }
    }
    private async Task RunAsync(int seconds, Action failed, TimeProvider time) {
        try {
            while (true) { await Task.Delay(TimeSpan.FromSeconds(seconds), time, stop.Token); Enforce(); }
        } catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
          catch { failed(); }
    }
    private void Release() {
        lock (leasesSync) foreach (string id in desired.Keys) {
            var lease = leases[id];
            if (lease.Owners == 1) leases.Remove(id); else leases[id] = (lease.Muted, lease.Owners - 1);
        }
    }
    public async ValueTask DisposeAsync() {
        if (Interlocked.Exchange(ref disposed, 1) != 0) return;
        stop.Cancel();
        try { await worker; } finally { Release(); stop.Dispose(); }
    }
    private sealed class WindowsEndpoint : IAudioMuteEndpoint {
        private readonly MMDevice device;
        internal WindowsEndpoint(string id) { using var devices = new MMDeviceEnumerator(); device = devices.GetDevice(id); }
        public string Id => device.ID;
        public bool ActiveRender => device.DataFlow == DataFlow.Render && device.State == DeviceState.Active;
        public bool Muted { get => device.AudioEndpointVolume.Mute; set => device.AudioEndpointVolume.Mute = value; }
        public void Dispose() => device.Dispose();
    }
}
