using System.Net;
using System.Diagnostics;
using SIPSorcery.Media;
using SIPSorcery.Net;
using SIPSorceryMedia.Abstractions;

namespace Ivy.PhoneBridge;

// Digital call audio only: no Windows endpoints, browser, or Desktop session.
// Both directions remain bounded and are discarded whenever call authority closes.
public sealed class WebRtcAudioRoute : ICallAudioRoute {
    internal const int PlayoutPrebufferMs = 60;
    public int QueueMs { get; }
    public int PlaybackPrebufferMs { get; }
    private readonly object sync = new();
    private readonly Func<bool> permitted;
    private readonly RTCPeerConnection peer = new AudioPeer();
    private readonly AudioQueue received, outgoing;
    private readonly RtpReceiveQueue packets = new(20);
    private readonly CancellationTokenSource stop = new();
    private readonly TaskCompletionSource connected = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly TaskCompletionSource gathered = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private MediaCodec encoder, decoder;
    private RTCDataChannel events;
    private Task sending, disposal;
    private AudioFormat format;
    private bool resumed, closed, failed, hadAuthority;
    private bool rtpReady;
    private uint? probationSource;
    private ushort probationSequence;
    // RTP received before DTLS/call audio readiness has no playback authority. In
    // particular, a server bootstrap packet must not seed the live SRTP sequence.
    private sealed class ReadyAudioStream(RtpSessionConfig config, int index) : AudioStream(config, index) {
        protected override void DispatchPendingPackages() => ClearPendingPackages();
    }
    private sealed class AudioPeer() : RTCPeerConnection(null) {
        protected override AudioStream GetOrCreateAudioStream(int index) {
            if (index < AudioStreamList.Count) return AudioStreamList[index];
            if (index != 0) throw new InvalidOperationException("One realtime audio stream required.");
            var stream = new ReadyAudioStream(rtpSessionConfig, index) { MediaInsertionOrder = index };
            AudioStreamList.Add(stream); return stream;
        }
    }
    private long generation, sentPackets, receivedPackets;
    public string Offer { get; private set; }
    public bool Failed { get { lock (sync) return failed; } }
    public bool IsOpen { get { lock (sync) {
        bool open = !closed && !failed && resumed && permitted() && peer.connectionState == RTCPeerConnectionState.connected;
        if (open) hadAuthority = true;
        return open;
    } } }
    public long Generation => Interlocked.Read(ref generation);
    public MediaStatus Status { get { lock (sync) return new(closed ? "closed" : failed ? "failed" : IsOpen ? "open" : "suspended", "webrtc",
        AudioSettings.SampleRate, 1, received.Dropped, outgoing.Dropped, received.Count, outgoing.Count, null); } }
    public object Observation { get { lock (sync) return new { state = peer.connectionState.ToString(), sentPackets, receivedPackets,
        queueMs = QueueMs, playbackPrebufferMs = PlaybackPrebufferMs,
        receive = packets.Status, concealedSamples = decoder?.ConcealedSamples ?? 0,
        captureUnderruns = received.Underruns, renderUnderruns = outgoing.Underruns, audio = Status }; } }

    private sealed class CodecPort(WebRtcAudioRoute owner) : IPcmAudioPort {
        public bool IsOpen => owner.IsOpen;
        public long Generation => owner.Generation;
        public int ReadCaptured(Span<float> output) => owner.outgoing.Read(output);
        public void WriteReceived(ReadOnlySpan<float> input) => owner.received.Write(input);
    }
    public WebRtcAudioRoute(Func<bool> permitted, int queueMs = 80, int? playbackPrebufferMs = null) {
        ArgumentNullException.ThrowIfNull(permitted);
        if (queueMs is < 20 or > 200) throw new ArgumentException("Bounded realtime audio queues required.");
        QueueMs = queueMs;
        PlaybackPrebufferMs = playbackPrebufferMs ?? Math.Min(PlayoutPrebufferMs, queueMs);
        if (PlaybackPrebufferMs < 0 || PlaybackPrebufferMs > queueMs)
            throw new ArgumentException("Realtime prebuffer must fit its audio queue.");
        this.permitted = permitted;
        int capacity = AudioSettings.SampleRate * queueMs / 1000;
        // Keep a short playout reserve for packet/sender jitter. Re-arm after a
        // speech pause or starvation so each new utterance has the same reserve.
        int prebuffer = AudioSettings.SampleRate * PlaybackPrebufferMs / 1000;
        received = new(capacity, prebuffer, rebufferAfterUnderrun: true, smoothDiscontinuities: true);
        outgoing = new(capacity, prebuffer, rebufferAfterUnderrun: true, smoothDiscontinuities: true);
        using var audio = new AudioEncoder(includeOpus: true);
        peer.addTrack(new MediaStreamTrack(audio.SupportedFormats.Where(value =>
            value.FormatName.Equals("opus", StringComparison.OrdinalIgnoreCase)).ToList(), MediaStreamStatusEnum.SendRecv));
        peer.OnAudioFormatsNegotiated += formats => {
            lock (sync) {
                if (closed) return;
                format = formats.First();
                encoder?.Dispose(); decoder?.Dispose();
                var port = new CodecPort(this);
                encoder = new(format, port); decoder = new(format, port);
            }
        };
        peer.onicegatheringstatechange += state => { if (state == RTCIceGatheringState.complete) gathered.TrySetResult(); };
        peer.onconnectionstatechange += state => {
            lock (sync) {
                if (state == RTCPeerConnectionState.connected) {
                    var security = peer.AudioStream.GetSecurityContext();
                    peer.AudioStream.SetSecurityContext(security.ProtectRtpPacket,
                        (byte[] buffer, int length, out int outputLength) => {
                            // RFC 3550 startup sequence probation: require consecutive
                            // packets before seeding SRTP's replay window. A lone
                            // transport probe may use an unrelated sequence/timestamp.
                            outputLength = 0;
                            if (!rtpReady) {
                                var header = new RTPHeader(buffer);
                                bool consecutive = probationSource == header.SyncSource &&
                                    header.SequenceNumber == unchecked((ushort)(probationSequence + 1));
                                probationSource = header.SyncSource; probationSequence = header.SequenceNumber;
                                if (!consecutive) return -4;
                            }
                            int result = security.UnprotectRtpPacket(buffer, length, out outputLength);
                            if (result == 0) rtpReady = true;
                            return result;
                        }, security.ProtectRtcpPacket, security.UnprotectRtcpPacket);
                    connected.TrySetResult();
                }
                if (!closed && state is RTCPeerConnectionState.failed or RTCPeerConnectionState.closed) {
                    failed = true; Clear();
                    connected.TrySetException(new NativeRpcException("phone_webrtc_failed"));
                }
            }
        };
        peer.OnRtpPacketReceived += Receive;
    }
    public async Task PrepareAsync(CancellationToken cancellationToken = default) {
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(stop.Token, cancellationToken);
        events = await peer.createDataChannel("oai-events");
        var offer = peer.createOffer(null);
        await peer.setLocalDescription(offer);
        if (peer.iceGatheringState != RTCIceGatheringState.complete)
            await gathered.Task.WaitAsync(TimeSpan.FromSeconds(5), linked.Token);
        linked.Token.ThrowIfCancellationRequested();
        Offer = peer.localDescription.sdp.ToString();
        sending = StartSender();
    }
    public async Task AcceptAsync(string sdp, CancellationToken cancellationToken = default) {
        if (string.IsNullOrWhiteSpace(sdp) || sdp.Length > 48000) throw new ArgumentException("Bounded WebRTC answer required.");
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(stop.Token, cancellationToken);
        if (peer.setRemoteDescription(new RTCSessionDescriptionInit { type = RTCSdpType.answer, sdp = sdp }) != SetDescriptionResultEnum.OK)
            throw new NativeRpcException("phone_webrtc_sdp_invalid");
        await connected.Task.WaitAsync(TimeSpan.FromSeconds(15), linked.Token);
    }
    private void Clear(bool forceGeneration = false) {
        bool dirty = hadAuthority || received.Count != 0 || outgoing.Count != 0 || packets.Status.QueuedPackets != 0;
        received.Clear(); outgoing.Clear(); packets.Clear(); hadAuthority = false;
        if (dirty || forceGeneration) Interlocked.Increment(ref generation);
    }
    public void Resume() { lock (sync) { if (closed || failed) throw new NativeRpcException("phone_webrtc_failed"); resumed = true; } }
    public void Suspend() { lock (sync) { resumed = false; Clear(true); } }
    public int ReadCaptured(Span<float> output) {
        lock (sync) {
            if (!IsOpen) { Clear(); output.Clear(); return 0; }
            // SIP and WebRTC have independent clock phases. Recover a queued
            // gap at the actual playout deadline, before padding PCM with silence.
            Drain(output.Length);
            return received.Read(output);
        }
    }
    public void WriteReceived(ReadOnlySpan<float> input) { lock (sync) { if (IsOpen) outgoing.Write(input); else Clear(); } }
    private void Receive(IPEndPoint endpoint, SDPMediaTypesEnum media, RTPPacket packet) {
        if (media != SDPMediaTypesEnum.audio) return;
        lock (sync) {
            if (closed || failed) return;
            if (!IsOpen) { packets.Skip(packet.Header.SyncSource, packet.Header.SequenceNumber); return; }
            if (packet.Header.PayloadType != format.FormatID) return;
            packets.Add(new(packet.Header.SequenceNumber, packet.Header.Timestamp, packet.Header.SyncSource, packet.Header.PayloadType, packet.Payload, Generation));
            Drain();
        }
    }
    private void Drain(int playoutSamples = 0) {
        while (packets.TryTake(out var packet, out _, playoutDeadline: received.Count < playoutSamples)) {
            try {
                decoder.DecodeRtp(packet); receivedPackets++;
            } finally { Array.Clear(packet.Payload); }
        }
    }
    private Task StartSender() {
        var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() => {
            try { Send(); }
            catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
            catch { lock (sync) { failed = true; Clear(); } }
            finally { completion.TrySetResult(); }
        // Match the existing SIP pump so other process work cannot routinely
        // preempt one half of the same 20 ms audio path.
        }) { IsBackground = true, Name = "Ivy WebRTC audio", Priority = ThreadPriority.Highest };
        try { thread.Start(); }
        catch { lock (sync) { failed = true; Clear(); } completion.TrySetResult(); }
        return completion.Task;
    }
    private void Send() {
        using var timerResolution = WindowsTimerResolutionLease.Acquire();
        long frameTicks = Math.Max(1, Stopwatch.Frequency / 50);
        long next = Stopwatch.GetTimestamp() + frameTicks;
        while (!stop.IsCancellationRequested) {
            long remaining;
            while ((remaining = next - Stopwatch.GetTimestamp()) > 0) {
                int milliseconds = (int)Math.Min(10, remaining * 1000 / Stopwatch.Frequency);
                if (milliseconds > 0 && stop.Token.WaitHandle.WaitOne(milliseconds)) return;
                if (milliseconds == 0) Thread.SpinWait(32);
            }
            if (stop.IsCancellationRequested) return;
            lock (sync) {
                if (closed || failed) return;
                if (!IsOpen || encoder == null) Clear();
                else {
                    Drain();
                    peer.SendAudio(encoder.RtpDuration, encoder.Encode()); sentPackets++;
                }
            }
            // Thread-pool stalls cannot coalesce audio ticks. Short delays catch up,
            // while a long process suspension never produces an unbounded RTP burst.
            next += frameTicks;
            long now = Stopwatch.GetTimestamp();
            if (now - next > frameTicks * 2) next = now + frameTicks;
        }
    }
    public ValueTask DisposeAsync() {
        lock (sync) { disposal ??= DisposeCoreAsync(); return new(disposal); }
    }
    private async Task DisposeCoreAsync() {
        lock (sync) { closed = true; resumed = false; Clear(); }
        await stop.CancelAsync();
        peer.close(); peer.Dispose();
        if (sending != null) await sending;
        lock (sync) { encoder?.Dispose(); decoder?.Dispose(); }
        stop.Dispose();
    }
}
