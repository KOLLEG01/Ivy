using System.Net;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.Media;
using SIPSorcery.SIP.App;

static partial class Program {
    static RtpAudioPacket Packet(ushort sequence, uint source = 10, long generation = 1) => new(sequence, (uint)sequence * 160, source, 0, new byte[] { 1, 2, 3 }, generation);
    static void RtpQueues() {
        var clock = new Clock(); var queue = new RtpReceiveQueue(20, clock);
        Check(queue.Add(Packet(65534)) && queue.TryTake(out var first, out _) && first.Sequence == 65534, "first RTP passes without startup buffering");
        Check(queue.Add(Packet(0)) && !queue.TryTake(out _, out _), "out-of-order packet waits across sequence wrap");
        Check(!queue.Add(Packet(0)) && queue.Status.DuplicatePackets == 1, "queued duplicate is not decoded twice");
        Check(queue.Add(Packet(65535)) && queue.TryTake(out var penultimate, out bool gap) && penultimate.Sequence == 65535 && !gap,
            "late gap filler restores ordering");
        Check(queue.TryTake(out var wrapped, out gap) && wrapped.Sequence == 0 && !gap, "65535 advances to0 without checked overflow");
        Check(!queue.Add(Packet(65535)) && queue.Status.LatePackets == 1, "already delivered packet is discarded");
        clock.Ticks = 10; queue.Add(Packet(2)); clock.Ticks = 29;
        Check(!queue.TryTake(out _, out _), "missing packet waits from original arrival");
        clock.Ticks = 30;
        Check(queue.TryTake(out var afterLoss, out gap) && afterLoss.Sequence == 2 && gap && queue.Status.MissingPackets == 1,
            "gap expires without another packet and requests decoder reset");
        Check(queue.Status.MaximumResidenceMs == 20, "actual monotonic queue residence is retained");
        clock.Ticks = 29; Reject(() => queue.TryTake(out _, out _), "backwards clock fails closed");

        clock = new Clock(); queue = new RtpReceiveQueue(20, clock); queue.Add(Packet(0)); queue.TryTake(out _, out _);
        queue.Add(Packet(2)); clock.Ticks = 20;
        Check(!queue.Add(Packet(1)) && queue.TryTake(out var expired, out gap) && expired.Sequence == 2 && gap,
            "an arrival at the expired gap deadline cannot restore stale audio before the next timer tick");

        clock = new Clock(); queue = new RtpReceiveQueue(20, clock); queue.Add(Packet(0)); queue.TryTake(out _, out _);
        queue.Add(Packet(2));
        Check(!queue.TryTake(out _, out _), "a queued gap retains its reorder window while PCM has reserve");
        Check(queue.TryTake(out var due, out gap, playoutDeadline: true) && due.Sequence == 2 && gap && queue.Status.MissingPackets == 1,
            "an actual PCM deadline releases the next packet for codec concealment without a silence frame");
        Check(!queue.Add(Packet(1)) && queue.Status.LatePackets == 1 && !queue.TryTake(out _, out _),
            "a deadline-concealed packet cannot replay after arriving late");

        clock = new Clock(); queue = new RtpReceiveQueue(60, clock); queue.Add(Packet(0)); queue.TryTake(out _, out _);
        for (ushort seq = 2; seq <= 9; seq++) queue.Add(Packet(seq));
        Check(queue.Status.QueuedPackets == 8 && !queue.Add(Packet(10)), "packet/byte backlog stays bounded under a gap flood");
        queue.Add(Packet(1));
        for (ushort seq = 1; seq <= 8; seq++) Check(queue.TryTake(out var next, out _) && next.Sequence == seq, "capacity prefers near packets to far backlog");
        Check(queue.Status.OverflowPackets == 2 && queue.Status.QueuedPackets == 0, "both rejected and replaced overflow packets counted");
        queue.Add(Packet(10)); clock.Ticks = 59; queue.Add(Packet(11)); clock.Ticks = 60;
        Check(queue.TryTake(out var deadline, out gap) && deadline.Sequence == 10 && gap, "new arrivals do not extend an existing gap deadline");
        queue.Clear(); // Advances past11, even though it was never decoded.
        Check(!queue.Add(Packet(11)), "discarded old permission backlog cannot return after resume");
        queue.Skip(10, 15); Check(!queue.Add(Packet(14)), "packets seen while gated advance the watermark");
        Check(!queue.Add(Packet(16, source: 99)) && queue.Status.ForeignPackets == 1, "a different SSRC cannot take over an existing call");
        var original = Packet(16, generation: 2); queue.Add(original); Array.Fill(original.Payload, (byte)9);
        Check(queue.TryTake(out var resumed, out gap) && gap && resumed.Generation == 2 && resumed.Payload[0] == 1,
            "resumed queue owns a payload copy and requests fresh decoder history");
        queue.RejectPeer(); Check(queue.Status.ForeignPackets == 2, "unrelated endpoint rejection is observable");
        Reject(() => queue.Add(Packet(17) with { Payload = new byte[8193] }), "oversized packet never retained");
        Reject(() => new RtpReceiveQueue(61), "unbounded reorder delay rejected");

        queue = new RtpReceiveQueue(0, new Clock()); queue.Add(Packet(0)); queue.TryTake(out _, out _); queue.Add(Packet(30000));
        Check(queue.TryTake(out var jumped, out gap) && jumped.Sequence == 30000 && gap && queue.Status.MissingPackets == 29999,
            "large forward loss uses one bounded transition, not one allocation per missing packet");
    }
    sealed class RecordingPort : IPcmAudioPort {
        public bool IsOpen { get; set; } = true;
        public long Generation { get; set; } = 1;
        private readonly object sync = new();
        private readonly List<int> signs = new();
        public int[] Signs { get { lock (sync) return signs.ToArray(); } }
        public int ReadCaptured(Span<float> output) { output.Clear(); return output.Length; }
        public void WriteReceived(ReadOnlySpan<float> input) {
            double total = 0; foreach (float value in input) total += value;
            lock (sync) signs.Add(Math.Sign(total));
        }
    }
    static async Task RtpLoopback() {
        var settings = new CodecSettings(new[] { "PCMU" }, ReceiveReorderMs: 60); var port = new RecordingPort();
        using var source = new RTPSession(false, false, false, IPAddress.Loopback);
        source.addTrack(new MediaStreamTrack(settings.Formats()));
        await using var target = new SipMediaSession(settings, port, IPAddress.Loopback);
        var offer = source.CreateOffer(IPAddress.Loopback);
        Check(target.SetRemoteDescription(SdpType.offer, offer) == SetDescriptionResultEnum.OK, "controlled RTP source offer negotiated");
        Check(source.SetRemoteDescription(SdpType.answer, target.CreateAnswer(IPAddress.Loopback)) == SetDescriptionResultEnum.OK, "controlled RTP source answer negotiated");
        await source.Start(); await target.Start();
        using var encoder = new AudioEncoder();
        var format = settings.Formats().Single();
        byte[] positive = encoder.EncodeAudio(Enumerable.Repeat((short)8000, 160).ToArray(), format);
        byte[] negative = encoder.EncodeAudio(Enumerable.Repeat((short)-8000, 160).ToArray(), format);
        void Send(ushort sequence, byte[] payload) => source.SendRtpRaw(SDPMediaTypesEnum.audio, payload, unchecked((uint)(ushort)(sequence - 65534) * 160), 0, format.FormatID, sequence);
        Send(65534, positive); await Until(() => target.ReceivedPackets == 1, "initial actual UDP audio decoded");
        Send(0, positive); Send(0, positive); Send(65535, negative);
        await Until(() => target.ReceivedPackets == 3, "actual UDP packets reorder across65535 and duplicate is suppressed");
        Check(port.Signs.SequenceEqual(new[] { 1, -1, 1 }) && target.ReceiveStatus.DuplicatePackets == 1 && !target.Failed,
            "PCM order follows RTP sequence rather than packet arrival");
        Send(2, negative); // Sequence1 never arrives. No following input may be needed to drain it.
        await Until(() => target.ReceivedPackets == 4, "receive timer drains a loss with no subsequent UDP packet");
        Check(target.ReceiveStatus.MissingPackets == 1 && target.ReceiveStatus.MaximumResidenceMs >= 60,
            "actual gap loss and observed residence are visible in status");
        Send(1, positive); await Until(() => target.ReceiveStatus.LatePackets >= 1, "late gap filler cannot replay audio");
        port.IsOpen = false; port.Generation++;
        Send(3, positive); await Until(() => target.ReceiveStatus.GatedPackets >= 1, "closed audio packet discarded on real receive path");
        int before = port.Signs.Length; port.IsOpen = true; port.Generation++;
        Send(3, positive); Send(4, negative);
        await Until(() => port.Signs.Length > before, "fresh generation receives fresh RTP only");
        Check(port.Signs.Length == before + 1 && port.Signs.Last() == -1, "old gated sequence is not replayed under new audio authority");
        target.Close("fixture_complete"); source.Close("fixture_complete");
    }
}
