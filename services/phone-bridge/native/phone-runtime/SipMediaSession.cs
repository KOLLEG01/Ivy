using System.Net;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;
using SIPSorceryMedia.Abstractions;

namespace Ivy.PhoneBridge;

public sealed class SipMediaSession : RTPSession, IAsyncDisposable {
    private readonly object transmit = new(), receive = new(), lifecycle = new();
    private readonly IPcmAudioPort audio;
    private readonly CallWaitingAudio waitingAudio;
    private readonly Dictionary<string, int> preference;
    private readonly List<SDPAudioVideoMediaFormat> configuredFormats;
    private readonly RtpReceiveQueue receiveQueue;
    private Dictionary<int, AudioFormat> receiveFormats = new();
    private Dictionary<int, string> evsAnswers = new();
    private readonly CancellationTokenSource stop = new();
    private MediaCodec sender, receiver;
    private Task pump = Task.CompletedTask;
    private Task receivePump = Task.CompletedTask;
    private long receiveGeneration = -1;
    private bool receiveOpen;
    private Task disposal;
    private bool started, closed;
    private int failed;
    private long sentPackets, receivedPackets;
    private readonly object reportSync = new();
    private long reportsReceived, reportsSent;
    private DateTimeOffset? lastReportAt;
    private object[] peerReceptionReports = [];
    private string transmittedCodec;
    private int? transmittedPayload;
    public string PreferredCodec { get; }
    public (string Codec, int? Payload, long Packets) Transmission { get { lock (transmit) return (transmittedCodec, transmittedPayload, sentPackets); } }
    private SipDtmfReceiver dtmf;
    private Func<bool> needsDtmf = () => false;
    private Action<char, DtmfInputSource> dtmfDigit;
    private readonly SemaphoreSlim receivePending = new(0, 1);
    private readonly RtpDtmfEventTracker toneEvents = new();
    private HashSet<int> tonePayloads = new();
    private object[] remoteEventFormats = [], localEventFormats = [];
    private readonly long[] rawPayloadCounts = new long[128];
    private long inBandAnalyzedPackets, inBandDetectedDigits, rfc4733DetectedDigits;
    private long rawRtpPackets, rawOfferedEventPackets, parsedEventPackets, libraryEventPackets, unsupportedPayloadPackets;
    private RTPChannel dtmfChannel;
    private bool challengeRestricted;
    private ScreeningAudio screening;
    internal void BeginWaiting() => waitingAudio.Begin();
    internal void EndWaiting() => waitingAudio.End();
    internal void CommandFeedback(bool success) => waitingAudio.Acknowledge(success);
    internal void PrepareScreening(ScreeningSettings settings, Func<bool> waiting) {
        lock (transmit) {
            if (screening != null) throw new InvalidOperationException("Original screening can start only once.");
            var prepared = new ScreeningAudio(audio, waiting); prepared.Load(settings); screening = prepared;
            if (sender != null) { var format = sender.Format; sender.Dispose(); sender = new MediaCodec(format, screening); }
        }
    }
    internal void BridgeScreening() { lock (transmit) screening?.Bridge(); }
    internal Action<char, DtmfInputSource> Digit;
    public string SendCodec { get { lock (transmit) return sender?.Format.FormatName; } }
    public int? SendPayload { get { lock (transmit) return sender?.Format.FormatID; } }
    public object Diagnostics {
        get {
            object[] formats, rawPayloads, remoteEvents, localEvents; object inBand; int[] offeredEventPayloadIds;
            lock (receive) {
                formats = receiveFormats.Values.Select(format => (object)new {
                    codec = format.FormatName, payload = format.FormatID, clockRate = format.RtpClockRate }).ToArray();
                offeredEventPayloadIds = tonePayloads.Order().ToArray();
                remoteEvents = remoteEventFormats;
                localEvents = localEventFormats;
                rawPayloads = rawPayloadCounts.Select((packets, payload) => new { payload, packets })
                    .Where(value => value.packets > 0).Select(value => (object)value).ToArray();
                inBand = dtmf?.Diagnostics;
            }
            object rtcp;
            lock (reportSync) rtcp = new { reportsReceived, reportsSent, lastReportAt, peerReceptionReports };
            return new { sendCodec = SendCodec, sendPayload = SendPayload, receiveFormats = formats, rtcp,
                localRtp = AudioStream.GetRTPChannel()?.RTPLocalEndPoint?.ToString(), remoteRtp = AudioStream.DestinationEndPoint?.ToString(),
                sentPackets = SentPackets, receivedPackets = ReceivedPackets, receive = ReceiveStatus, audio = AudioStatus,
                realtime = (audio as CallAudioPort)?.Realtime?.Observation,
                dtmf = new { offeredEventPayloads = offeredEventPayloadIds.Length, offeredEventPayloadIds,
                    remoteEventFormats = remoteEvents, localEventFormats = localEvents,
                    negotiatedEventPayloadId = AudioStream.NegotiatedRtpEventPayloadID,
                    rawPayloads, inBand,
                    rawRtpPackets = Interlocked.Read(ref rawRtpPackets),
                    rawOfferedEventPackets = Interlocked.Read(ref rawOfferedEventPackets),
                    parsedEventPackets = Interlocked.Read(ref parsedEventPackets),
                    libraryEventPackets = Interlocked.Read(ref libraryEventPackets),
                    unsupportedPayloadPackets = Interlocked.Read(ref unsupportedPayloadPackets),
                    inBandAnalyzedPackets = Interlocked.Read(ref inBandAnalyzedPackets),
                    inBandDetectedDigits = Interlocked.Read(ref inBandDetectedDigits),
                    rfc4733DetectedDigits = Interlocked.Read(ref rfc4733DetectedDigits) } };
        }
    }
    internal void EnableDtmf(Action<char, DtmfInputSource> digit, Func<bool> needed) {
        lock (receive) {
            Digit = digit;
            dtmfDigit = (tone, source) => { Interlocked.Increment(ref inBandDetectedDigits); digit(tone, source); };
            needsDtmf = needed;
        }
        OnRtpEvent += (endpoint, value, header) => {
            Interlocked.Increment(ref libraryEventPackets);
            if (AudioStream.DestinationEndPoint?.Equals(endpoint) == true &&
                DtmfAccessCodeGate.TryConvertRtpEvent(value.EventID, out char tone) &&
                toneEvents.IsNewEvent(header.SyncSource, header.Timestamp, value.EventID)) {
                Interlocked.Increment(ref rfc4733DetectedDigits);
                digit(tone, DtmfInputSource.Rfc4733);
            }
        };
    }
    internal void ChallengeCodecs(bool enabled) {
        lock (lifecycle) {
            challengeRestricted = enabled;
            AudioLocalTrack.Capabilities.Clear();
            AudioLocalTrack.Capabilities.AddRange(enabled
                ? new MediaStreamTrack(new CodecSettings(["PCMA", "PCMU"]).Formats()).Capabilities : configuredFormats);
        }
    }
    public bool Failed => Volatile.Read(ref failed) != 0;
    public long SentPackets => Interlocked.Read(ref sentPackets);
    public long ReceivedPackets => Interlocked.Read(ref receivedPackets);
    public RtpReceiveStatus ReceiveStatus { get { lock (receive) return receiveQueue.Status; } }
    public AudioAttachmentStatus AudioStatus => (audio as CallAudioPort)?.Status;
    internal CallAudioPort CallAudio => audio as CallAudioPort;
    public Task PrepareAudioAsync(AudioSettings settings, DesktopIdentity desktop) {
        lock (lifecycle) {
            if (closed || Failed || audio is not CallAudioPort owned) throw new NativeRpcException("runtime_not_ready");
            return owned.PrepareAsync(settings, desktop);
        }
    }
    public event Action MediaFailed;

    public SipMediaSession(CodecSettings settings, IPcmAudioPort audio, IPAddress bindAddress = null, string callId = null, SIPSorcery.Sys.PortRange rtpPorts = null)
        : base(false, false, false, bindAddress, 0, rtpPorts) {
        try {
            ArgumentNullException.ThrowIfNull(audio);
            if (audio is ICallAudioPort owned && owned.CallId != callId) throw new ArgumentException("Audio belongs to a different original call.");
            this.audio = audio;
            waitingAudio = new CallWaitingAudio(audio);
            OnSendReport += (media, _) => { if (media == SDPMediaTypesEnum.audio) lock (reportSync) reportsSent++; };
            OnReceiveReport += (endpoint, media, report) => {
                if (media != SDPMediaTypesEnum.audio) return;
                var samples = report.SenderReport?.ReceptionReports ?? report.ReceiverReport?.ReceptionReports;
                lock (reportSync) {
                    reportsReceived++; lastReportAt = DateTimeOffset.UtcNow;
                    peerReceptionReports = samples?.Take(8).Select(sample => (object)new {
                        ssrc = sample.SSRC, fractionLost256 = sample.FractionLost, packetsLost = sample.PacketsLost,
                        extendedHighestSequence = sample.ExtendedHighestSequenceNumber, jitterTimestampUnits = sample.Jitter,
                    }).ToArray() ?? [];
                }
            };
            settings.Validate();
            PreferredCodec = settings.Preferences[0];
            preference = settings.Preferences.Select((name, index) => (name, index))
                .ToDictionary(value => value.name, value => value.index, StringComparer.OrdinalIgnoreCase);
            preference.TryAdd("PCMA", 100); preference.TryAdd("PCMU", 101);
            receiveQueue = new RtpReceiveQueue(settings.ReceiveReorderMs);
            AcceptRtpFromAny = false;
            addTrack(new MediaStreamTrack(settings.Formats()));
            dtmfChannel = AudioStream.GetRTPChannel();
            dtmfChannel.OnRTPDataReceived += AlternateTelephoneEvent;
            NormalizeEvsTrack();
            configuredFormats = AudioLocalTrack.Capabilities.ToList();
            if (preference.ContainsKey("EVS")) {
                // Configuration cannot advertise a missing or incompatible packaged native codec.
                using var encoder = new EvsNativeCodec(true);
                using var decoder = new EvsNativeCodec(false);
            }
            OnAudioFormatsNegotiated += Negotiated;
            OnRtpPacketReceived += Received;
        } catch {
            try { Close("initialization_failed"); }
            finally {
                try { if (this.audio is ICallAudioPort owned) owned.DisposeAsync().AsTask().GetAwaiter().GetResult(); }
                finally { stop.Dispose(); }
            }
            throw;
        }
    }
    private void Negotiated(List<AudioFormat> formats) {
        try {
            // Negotiation may enumerate static RTP formats before dynamic ones. Select from
            // the agreed set using our configured priority, not the parser's enumeration order.
            formats = formats.Select(value => EvsFormat.IsEvs(value) && evsAnswers.TryGetValue(value.FormatID, out string parameters)
                ? new AudioFormat(value.FormatID, "EVS", 32000, 16000, 1, parameters) : value).ToList();
            var format = formats.Where(value => preference.ContainsKey(value.FormatName))
                .OrderBy(value => preference[value.FormatName]).First();
            lock (transmit) {
                if (IsClosed || Failed) return;
                sender?.Dispose(); sender = new MediaCodec(format, (IPcmAudioPort)screening ?? waitingAudio);
            }
            lock (receive) {
                if (IsClosed || Failed) return;
                receiveFormats = formats.Where(value => preference.ContainsKey(value.FormatName)).ToDictionary(value => value.FormatID);
                receiveQueue.Clear(); receiver?.Dispose(); receiver = null;
            }
        } catch { Fail(); }
    }
    private bool CurrentReceive() {
        bool open = waitingAudio.IsOpen; long generation = waitingAudio.Generation;
        if (open != receiveOpen || generation != receiveGeneration) {
            receiveQueue.Clear(); receiver?.Dispose(); receiver = null; receiveOpen = open; receiveGeneration = generation;
        }
        return open;
    }
    private void AlternateTelephoneEvent(int localPort, IPEndPoint endpoint, byte[] bytes) {
        // RTPSession drops event payloads other than its single negotiated event ID before
        // OnRtpPacketReceived. Observe the original channel, retaining the same peer boundary.
        Interlocked.Increment(ref rawRtpPackets);
        if (IsClosed || Failed || localPort != dtmfChannel.RTPPort || bytes.Length < 16 || bytes.Length > 8192 || bytes[0] >> 6 != 2) return;
        lock (receive) {
            int payload = bytes[1] & 127;
            rawPayloadCounts[payload]++;
            if (tonePayloads.Contains(payload)) Interlocked.Increment(ref rawOfferedEventPackets);
            if (IsClosed || Failed || Digit == null || AudioStream.DestinationEndPoint?.Equals(endpoint) != true) return;
            if (!tonePayloads.Contains(payload) || payload == AudioStream.NegotiatedRtpEventPayloadID) return;
            try {
                var packet = new RTPPacket(bytes);
                var data = packet.GetPayloadBytes();
                if (data.Length < 4 || !DtmfAccessCodeGate.TryConvertRtpEvent(data[0], out char digit)) return;
                if (toneEvents.IsNewEvent(packet.Header.SyncSource, packet.Header.Timestamp, data[0])) {
                    Interlocked.Increment(ref rfc4733DetectedDigits);
                    Digit(digit, DtmfInputSource.Rfc4733);
                }
            } catch (ArgumentException) { /* Malformed untrusted datagram is not a media failure. */ }
              catch (IndexOutOfRangeException) { }
        }
    }
    private void Received(IPEndPoint endpoint, SDPMediaTypesEnum media, RTPPacket packet) {
        if (IsClosed || Failed) return;
        try {
            if (media != SDPMediaTypesEnum.audio) return;
            lock (receive) {
                if (IsClosed || Failed) return;
                if (AudioStream.DestinationEndPoint == null || !AudioStream.DestinationEndPoint.Equals(endpoint)) { receiveQueue.RejectPeer(); return; }
                if (tonePayloads.Contains(packet.Header.PayloadType)) {
                    Interlocked.Increment(ref parsedEventPackets);
                    if (packet.Payload.Length >= 4 && DtmfAccessCodeGate.TryConvertRtpEvent(packet.Payload[0], out char digit) &&
                        toneEvents.IsNewEvent(packet.Header.SyncSource, packet.Header.Timestamp, packet.Payload[0])) {
                        Interlocked.Increment(ref rfc4733DetectedDigits);
                        Digit?.Invoke(digit, DtmfInputSource.Rfc4733);
                    }
                    return;
                }
                if (!receiveFormats.ContainsKey(packet.Header.PayloadType)) {
                    Interlocked.Increment(ref unsupportedPayloadPackets);
                    return;
                }
                if (needsDtmf()) {
                    Interlocked.Increment(ref inBandAnalyzedPackets);
                    dtmf ??= new SipDtmfReceiver(dtmfDigit);
                    dtmf.Receive(receiveFormats[packet.Header.PayloadType], packet.Payload);
                }
                else if (dtmf != null) { dtmf.Dispose(); dtmf = null; }
                if (!CurrentReceive()) { receiveQueue.Skip(packet.Header.SyncSource, packet.Header.SequenceNumber); return; }
                receiveQueue.Add(new(packet.Header.SequenceNumber, packet.Header.Timestamp, packet.Header.SyncSource,
                    packet.Header.PayloadType, packet.Payload, receiveGeneration));
                DrainReceive();
                if (receiveQueue.Status.QueuedPackets > 0 && receivePending.CurrentCount == 0) receivePending.Release();
            }
        } catch { Fail(); }
    }
    private void DrainReceive() {
        if (IsClosed || Failed || !CurrentReceive()) return;
        while (receiveQueue.TryTake(out var packet, out bool discontinuity)) {
            try {
                if (!CurrentReceive() || packet.Generation != receiveGeneration) continue;
                var format = receiveFormats[packet.PayloadType];
                if (discontinuity || receiver == null || !receiver.Format.Equals(format)) {
                    receiver?.Dispose(); receiver = new MediaCodec(format, waitingAudio);
                }
                receiver.Decode(packet.Payload, packet.Generation); Interlocked.Increment(ref receivedPackets);
            } finally { Array.Clear(packet.Payload); }
        }
    }
    public override Task Start() {
        lock (lifecycle) {
            if (closed || started) return Task.CompletedTask;
            var result = base.Start();
            started = true; pump = StartPump(); receivePump = ReceiveAsync();
            return result;
        }
    }
    private async Task ReceiveAsync() {
        try {
            while (!stop.IsCancellationRequested) {
                await receivePending.WaitAsync(stop.Token);
                // Only a sequence gap needs a deadline wakeup. In-order packets drain on arrival.
                int delay; lock (receive) delay = receiveQueue.Status.ReorderMs;
                await Task.Delay(Math.Max(1, delay), stop.Token);
                lock (receive) {
                    DrainReceive();
                    if (receiveQueue.Status.QueuedPackets > 0 && receivePending.CurrentCount == 0) receivePending.Release();
                }
            }
        } catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
        catch { Fail(); }
    }
    public override SDP CreateOffer(IPAddress connectionAddress = null) {
        lock (lifecycle) {
            if (closed) return null;
            NormalizeEvsTrack(offering: true);
            var offer = PacketTime(base.CreateOffer(connectionAddress));
            lock (receive) localEventFormats = EventFormats(offer);
            return offer;
        }
    }
    public override SDP CreateAnswer(IPAddress connectionAddress) {
        var answer = base.CreateAnswer(connectionAddress);
        if (answer != null) foreach (var track in answer.Media.Where(value => value.Media == SDPMediaTypesEnum.audio))
            foreach (var format in track.MediaFormats.Values.ToArray())
                if (format.Name().Equals("EVS", StringComparison.OrdinalIgnoreCase) && evsAnswers.TryGetValue(format.ID, out string parameters))
                    track.MediaFormats[format.ID] = new SDPAudioVideoMediaFormat(format.Kind, format.ID, format.Rtpmap, parameters);
        answer = PacketTime(answer);
        lock (receive) localEventFormats = EventFormats(answer);
        return answer;
    }
    private static object[] EventFormats(SDP description) => description == null ? [] :
        description.Media.Where(track => track.Media == SDPMediaTypesEnum.audio && track.Port > 0)
            .SelectMany(track => track.MediaFormats.Values)
            .Where(format => format.Name().Equals("telephone-event", StringComparison.OrdinalIgnoreCase))
            .Select(format => {
                return (object)new { payload = format.ID, clockRate = RtpClockRate(format) };
            }).ToArray();
    private static int RtpClockRate(SDPAudioVideoMediaFormat format) {
        var parts = format.Rtpmap?.Split('/');
        return parts is { Length: > 1 } && int.TryParse(parts[1], out int parsed) ? parsed : 0;
    }
    private void NormalizeEvsTrack(bool offering = false, string rtpmap = "EVS/16000/1") {
        var formats = AudioLocalTrack?.Capabilities;
        if (formats == null) return;
        for (int index = 0; index < formats.Count; index++) {
            var format = formats[index];
            if (EvsFormat.Mono(format.Rtpmap)) formats[index] = new SDPAudioVideoMediaFormat(format.Kind, format.ID, rtpmap,
                offering ? EvsFormat.OfferParameters : format.Fmtp);
        }
    }
    private static SDP PacketTime(SDP description) {
        if (description != null) foreach (var track in description.Media.Where(value => value.Media == SDPMediaTypesEnum.audio &&
            value.MediaFormats.Values.Any(format => format.Name().Equals("EVS", StringComparison.OrdinalIgnoreCase)))) {
            foreach (string attribute in track.ExtraMediaAttributes.Where(value => value.StartsWith("a=ptime:", StringComparison.OrdinalIgnoreCase) ||
                value.StartsWith("a=maxptime:", StringComparison.OrdinalIgnoreCase)).ToArray()) track.ExtraMediaAttributes.Remove(attribute);
            track.ExtraMediaAttributes.Add("a=ptime:20"); track.ExtraMediaAttributes.Add("a=maxptime:40");
        }
        return description;
    }
    private void PrioritizeRemoteAudio(SDP description) {
        foreach (var track in description.Media.Where(value => value.Media == SDPMediaTypesEnum.audio)) {
            var selectedClockRate = track.MediaFormats.Values
                .Where(format => preference.ContainsKey(format.Name()) && configuredFormats.Any(local =>
                    local.Name().Equals(format.Name(), StringComparison.OrdinalIgnoreCase)))
                .OrderBy(format => preference[format.Name()])
                .Select(RtpClockRate).FirstOrDefault();
            var ordered = track.MediaFormats.Values.Select((format, index) => new { format, index })
                .OrderBy(value => preference.GetValueOrDefault(value.format.Name(), int.MaxValue))
                .ThenBy(value => value.format.Name().Equals("telephone-event", StringComparison.OrdinalIgnoreCase) &&
                    selectedClockRate != 0 && RtpClockRate(value.format) != selectedClockRate ? 1 : 0)
                .ThenBy(value => value.index).Select(value => value.format).ToArray();
            track.MediaFormats.Clear();
            foreach (var format in ordered) track.MediaFormats.Add(format.ID, format);
        }
    }
    public override SetDescriptionResultEnum SetRemoteDescription(SdpType type, SDP description) {
        ArgumentNullException.ThrowIfNull(description);
        var normalized = SDP.ParseSDPDescription(description.ToString());
        var offeredEvents = normalized.Media.Where(track => track.Media == SDPMediaTypesEnum.audio && track.Port > 0)
            .SelectMany(track => track.MediaFormats.Values).Where(format => format.Name().Equals("telephone-event", StringComparison.OrdinalIgnoreCase))
            .Select(format => format.ID).ToHashSet();
        var answers = new Dictionary<int, string>();
        string remoteEvsRtpmap = null;
        foreach (var track in normalized.Media.Where(value => value.Media == SDPMediaTypesEnum.audio)) {
            foreach (var format in track.MediaFormats.Values.ToArray()) {
                if (!format.Name().Equals("EVS", StringComparison.OrdinalIgnoreCase)) continue;
                if (!EvsFormat.Mono(format.Rtpmap) || !EvsFormat.TrySelect(format.Fmtp, type == SdpType.offer, out string selected)) {
                    track.MediaFormats.Remove(format.ID); continue;
                }
                remoteEvsRtpmap ??= format.Rtpmap;
                answers[format.ID] = selected;
                track.MediaFormats[format.ID] = new SDPAudioVideoMediaFormat(format.Kind, format.ID, format.Rtpmap, format.Fmtp);
            }
        }
        // The library narrows local capabilities to the previous answer. A subsequent remote
        // offer must be matched against our configured codecs, not that previous intersection.
        var previousFormats = AudioLocalTrack.Capabilities.ToList();
        var previousAnswers = evsAnswers;
        void ReplaceFormats(List<SDPAudioVideoMediaFormat> formats) {
            AudioLocalTrack.Capabilities.Clear(); AudioLocalTrack.Capabilities.AddRange(formats);
        }
        if (type == SdpType.offer && !challengeRestricted) ReplaceFormats(configuredFormats);
        if (remoteEvsRtpmap != null) NormalizeEvsTrack(rtpmap: remoteEvsRtpmap);
        PrioritizeRemoteAudio(normalized);
        evsAnswers = answers; // Negotiated is raised synchronously by SetRemoteDescription.
        try {
            var result = base.SetRemoteDescription(type, normalized);
            if (result != SetDescriptionResultEnum.OK) { evsAnswers = previousAnswers; ReplaceFormats(previousFormats); }
            else lock (receive) {
                tonePayloads = offeredEvents;
                remoteEventFormats = EventFormats(normalized);
            }
            return result;
        } catch { evsAnswers = previousAnswers; ReplaceFormats(previousFormats); throw; }
    }
    private Task StartPump() {
        var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() => {
            try { Pump(); completion.TrySetResult(); }
            catch (OperationCanceledException) when (stop.IsCancellationRequested) { completion.TrySetResult(); }
            catch (Exception error) { completion.TrySetException(error); Fail(); }
        }) { IsBackground = true, Name = "Ivy RTP audio", Priority = ThreadPriority.Highest };
        try { thread.Start(); }
        catch (Exception error) { completion.TrySetException(error); Fail(); }
        return completion.Task;
    }
    private void Pump() {
        using var timerResolution = WindowsTimerResolutionLease.Acquire();
        long frameTicks = Math.Max(1, System.Diagnostics.Stopwatch.Frequency / 50);
        long next = System.Diagnostics.Stopwatch.GetTimestamp() + frameTicks;
        long? audioFailedAt = null;
        try {
            while (!stop.IsCancellationRequested) {
                long remaining;
                while ((remaining = next - System.Diagnostics.Stopwatch.GetTimestamp()) > 0) {
                    int milliseconds = (int)Math.Min(10, remaining * 1000 / System.Diagnostics.Stopwatch.Frequency);
                    if (milliseconds > 0 && stop.Token.WaitHandle.WaitOne(milliseconds)) return;
                    if (milliseconds == 0) Thread.SpinWait(32);
                }
                // Keep the SIP dialog for one bounded repair of the same Voice owner's
                // Windows route. PCM is silent while the failed route is fenced.
                if (audio is ICallAudioPort owned && owned.Failed) {
                    if (owned is not CallAudioPort repairable || !repairable.RecoveryPending) { Fail(); return; }
                    audioFailedAt ??= System.Diagnostics.Stopwatch.GetTimestamp();
                    if (System.Diagnostics.Stopwatch.GetElapsedTime(audioFailedAt.Value) >= TimeSpan.FromSeconds(15)) { Fail(); return; }
                } else audioFailedAt = null;
                lock (transmit) {
                    if (sender == null || IsClosed || Failed) continue;
                    var packet = sender.Encode();
                    AudioStream.SendAudioFrame(sender.RtpDuration, sender.Format.FormatID, packet);
                    transmittedCodec = sender.Format.FormatName; transmittedPayload = sender.Format.FormatID;
                    Interlocked.Increment(ref sentPackets);
                }
                next += frameTicks;
                // A long process suspension cannot be repaired by emitting an unbounded RTP
                // burst. Resume from now; ordinary short scheduling delays still catch up.
                long now = System.Diagnostics.Stopwatch.GetTimestamp();
                if (now - next > frameTicks * 2) next = now + frameTicks;
            }
        } catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
    }
    private void Fail() {
        if (Interlocked.Exchange(ref failed, 1) != 0) return;
        Close("media_failed");
        try { MediaFailed?.Invoke(); } catch { /* The owning call reads Failed even if its notification handler failed. */ }
    }
    public override void Close(string reason) {
        lock (lifecycle) {
            if (closed) return; closed = true;
            try { if (audio is ICallAudioPort owned) owned.Revoke(); }
            finally {
                stop.Cancel();
                // Protocol diagnostics use fixed local reasons, never caller-provided SIP text.
                base.Close("phone_media_closed");
            }
        }
        lock (receive) { receiveQueue?.Clear(); receiver?.Dispose(); receiver = null; }
    }
    public ValueTask DisposeAsync() {
        lock (lifecycle) { disposal ??= DisposeCoreAsync(); return new ValueTask(disposal); }
    }
    private async Task DisposeCoreAsync() {
        await Task.Yield();
        try { Close("disposed"); }
        finally {
            try { await Task.WhenAll(pump, receivePump); }
            finally {
                OnAudioFormatsNegotiated -= Negotiated; OnRtpPacketReceived -= Received;
                if (dtmfChannel != null) dtmfChannel.OnRTPDataReceived -= AlternateTelephoneEvent;
                try {
                    lock (transmit) { sender?.Dispose(); sender = null; screening?.Dispose(); }
                    lock (receive) { receiveQueue?.Clear(); receiver?.Dispose(); receiver = null; dtmf?.Dispose(); dtmf = null; }
                } finally {
                    // CTS is disposed only after media callbacks/pumps and owned audio have stopped.
                    try { if (audio is ICallAudioPort owned) await owned.DisposeAsync(); }
                    finally { receivePending.Dispose(); stop.Dispose(); }
                }
            }
        }
    }
}
