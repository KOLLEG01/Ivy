namespace Ivy.PhoneBridge;

public sealed record RtpAudioPacket(ushort Sequence, uint Timestamp, uint Source, int PayloadType, byte[] Payload, long Generation);
public sealed record RtpReceiveStatus(int QueuedPackets, int MaximumPackets, int ReorderMs, long DuplicatePackets,
    long LatePackets, long OverflowPackets, long ForeignPackets, long GatedPackets, long MissingPackets, double MaximumResidenceMs);

// The owning receive lock serializes every method and status read. Payloads are copied on admission.
// No per-missing-packet allocation and no wall-clock dependency, including the65535->0 transition.
public sealed class RtpReceiveQueue {
    public const int Capacity = 8, MaximumPayload = 8192;
    private sealed record Entry(RtpAudioPacket Packet, long Arrived);
    private readonly Dictionary<ushort, Entry> queued = new();
    private readonly TimeProvider clock;
    private readonly int reorderMs;
    private ushort? expected;
    private uint? source;
    private long? lastClock;
    private long duplicate, late, overflow, foreign, gated, missing;
    private double maximumResidence;
    private bool resetDecoder;
    public RtpReceiveQueue(int reorderMs = 20, TimeProvider clock = null) {
        if (reorderMs is < 0 or > 60) throw new ArgumentException("Bounded receive gap delay required.");
        this.reorderMs = reorderMs; this.clock = clock ?? TimeProvider.System;
    }
    public RtpReceiveStatus Status => new(queued.Count, Capacity, reorderMs, duplicate, late, overflow, foreign, gated, missing, maximumResidence);
    private long Now() {
        long now = clock.GetTimestamp();
        if (lastClock.HasValue && now < lastClock.Value) throw new InvalidOperationException("Receive monotonic clock moved backwards.");
        lastClock = now; return now;
    }
    private int Distance(ushort sequence) => unchecked((short)(sequence - expected.Value));
    private bool Header(uint ssrc, ushort sequence) {
        if (source.HasValue && source != ssrc) { foreign++; return false; }
        source ??= ssrc; expected ??= sequence;
        if (Distance(sequence) < 0) { late++; return false; }
        return true;
    }
    public void RejectPeer() { foreign++; }
    public void Skip(uint ssrc, ushort sequence) {
        Now();
        if (!Header(ssrc, sequence)) return;
        Clear();
        if (Distance(sequence) >= 0) expected = unchecked((ushort)(sequence + 1));
        gated++; resetDecoder = true;
    }
    public bool Add(RtpAudioPacket packet) {
        if (packet == null || packet.Payload == null || packet.Payload.Length is < 1 or > MaximumPayload || packet.PayloadType is < 0 or > 127)
            throw new ArgumentException("Bounded RTP audio payload required.");
        long now = Now(); ExpireGap(now);
        if (!Header(packet.Source, packet.Sequence)) return false;
        if (queued.ContainsKey(packet.Sequence)) { duplicate++; return false; }
        if (queued.Count == Capacity) {
            var farthest = queued.Values.MaxBy(value => Distance(value.Packet.Sequence)); overflow++;
            if (Distance(packet.Sequence) >= Distance(farthest.Packet.Sequence)) return false;
            queued.Remove(farthest.Packet.Sequence); Array.Clear(farthest.Packet.Payload);
        }
        queued.Add(packet.Sequence, new(packet with { Payload = packet.Payload.ToArray() }, now)); return true;
    }
    public bool TryTake(out RtpAudioPacket packet, out bool discontinuity) {
        packet = null; discontinuity = false; long now = Now();
        if (queued.Count == 0) return false;
        ExpireGap(now);
        if (!queued.TryGetValue(expected.Value, out var entry)) return false;
        queued.Remove(expected.Value); expected = unchecked((ushort)(expected.Value + 1));
        maximumResidence = Math.Max(maximumResidence, clock.GetElapsedTime(entry.Arrived, now).TotalMilliseconds);
        packet = entry.Packet; discontinuity = resetDecoder; resetDecoder = false; return true;
    }
    private void ExpireGap(long now) {
        if (queued.Count == 0 || queued.ContainsKey(expected.Value)) return;
        long oldest = queued.Values.Min(value => value.Arrived);
        if (clock.GetElapsedTime(oldest, now).TotalMilliseconds < reorderMs) return;
        var nearest = queued.Values.MinBy(value => Distance(value.Packet.Sequence));
        missing += Distance(nearest.Packet.Sequence); expected = nearest.Packet.Sequence; resetDecoder = true;
    }
    /** Keep SSRC and advance beyond discarded backlog; a new permit cannot replay old packets. */
    public void Clear() {
        if (queued.Count != 0) {
            var last = queued.Values.MaxBy(value => Distance(value.Packet.Sequence));
            expected = unchecked((ushort)(last.Packet.Sequence + 1));
            foreach (var entry in queued.Values) Array.Clear(entry.Packet.Payload);
            gated += queued.Count; queued.Clear();
        }
        resetDecoder = true;
    }
}
