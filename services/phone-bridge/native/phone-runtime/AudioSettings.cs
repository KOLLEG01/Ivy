namespace Ivy.PhoneBridge;

public sealed record AudioSettings(string CaptureEndpointId, string RenderEndpointId, string SourceMode,
    int CaptureBufferMs, int RenderLatencyMs, int QueueMs, AudioMutePolicy MutePolicy = null, int PlaybackPrebufferMs = 20,
    bool EnableWasapiLowLatency = true, string LoopbackRenderEndpointId = null) {
    public const int SampleRate = 48000;
    public void Validate() {
        foreach (var id in new[] { CaptureEndpointId, RenderEndpointId })
            if (string.IsNullOrWhiteSpace(id) || id.Length > 32767 || id.Contains('\0'))
                throw new ArgumentException("Exact capture and render endpoint IDs are required.");
        if (CaptureEndpointId == RenderEndpointId || SourceMode is not ("system_excluding_runtime" or "desktop_process" or "microphone" or "virtual_speaker") ||
            CaptureBufferMs is < 10 or > 100 || RenderLatencyMs is < 2 or > 100 || QueueMs is < 20 or > 200 || QueueMs < CaptureBufferMs ||
            PlaybackPrebufferMs < 0 || PlaybackPrebufferMs > QueueMs)
            throw new ArgumentException("Invalid native audio routing or latency configuration.");
        if (SourceMode == "virtual_speaker" && (string.IsNullOrWhiteSpace(LoopbackRenderEndpointId) ||
            LoopbackRenderEndpointId.Length > 32767 || LoopbackRenderEndpointId.Contains('\0') || LoopbackRenderEndpointId == RenderEndpointId))
            throw new ArgumentException("Virtual speaker loopback requires a separate render endpoint.");
        MutePolicy?.Validate(RenderEndpointId);
    }
}

public sealed record AudioMutePolicy(string MutedRenderEndpointId = null, bool UnmuteReceive = false, int IntervalSeconds = 60) {
    public void Validate(string receiveEndpointId) {
        if (IntervalSeconds is < 1 or > 3600 || MutedRenderEndpointId == receiveEndpointId ||
            MutedRenderEndpointId != null && (string.IsNullOrWhiteSpace(MutedRenderEndpointId) || MutedRenderEndpointId.Length > 32767 || MutedRenderEndpointId.Contains('\0')))
            throw new ArgumentException("Mute policy requires distinct exact render endpoints and a bounded interval.");
    }
}
