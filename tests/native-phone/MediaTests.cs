using System.Diagnostics;
using System.Net;
using Ivy.PhoneBridge;
using SIPSorcery.Media;
using SIPSorcery.SIP;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;

static partial class Program {
    sealed class AudioPort : IPcmAudioPort {
        public bool IsOpen { get; set; } = true;
        public long Generation { get; set; } = 1;
        public Action? OnRead;
        public float Amplitude = .5f;
        private long captured, received;
        private readonly object sync = new();
        private double energy;
        public long Received { get { lock (sync) return received; } }
        public double Rms { get { lock (sync) return received == 0 ? 0 : Math.Sqrt(energy / received); } }
        public int ReadCaptured(Span<float> output) {
            for (int i = 0; i < output.Length; i++) output[i] = Amplitude * (float)Math.Sin(2 * Math.PI * 440 * captured++ / 48000);
            OnRead?.Invoke(); return output.Length;
        }
        public void WriteReceived(ReadOnlySpan<float> input) {
            lock (sync) {
                foreach (float sample in input) {
                    Check(float.IsFinite(sample) && Math.Abs(sample) <= 1, "decoded PCM is finite and clipped to device bounds");
                    energy += sample * sample;
                }
                received += input.Length;
            }
        }
    }
    sealed class ContinuityPort(bool tone = false) : IPcmAudioPort {
        private long position;
        private readonly Queue<float> queue = new();
        public readonly List<float> Samples = new();
        public bool IsOpen { get; set; } = true;
        public long Generation { get; set; } = 1;
        public int ReadCaptured(Span<float> output) {
            for (int i = 0; i < output.Length; i++)
                output[i] = tone ? .2f * (float)Math.Sin(2 * Math.PI * 997 * position++ / 48000) : queue.TryDequeue(out var value) ? value : 0;
            return output.Length;
        }
        public void WriteReceived(ReadOnlySpan<float> input) {
            foreach (float value in input) { queue.Enqueue(value); Samples.Add(value); }
        }
    }
    sealed class PlayoutPort(int prebufferMs = WebRtcAudioRoute.PlayoutPrebufferMs, int queueMs = 80) : IPcmAudioPort {
        public readonly AudioQueue Queue = new(48 * queueMs, 48 * prebufferMs,
            rebufferAfterUnderrun: true, smoothDiscontinuities: true);
        public bool IsOpen => true;
        public long Generation => 1;
        public int ReadCaptured(Span<float> output) => Queue.Read(output);
        public void WriteReceived(ReadOnlySpan<float> input) => Queue.Write(input);
    }
    static void OpusJitterPlayout() {
        OpusJitterPlayout(60, 80, false);
        OpusJitterPlayout(100, 160, true);
    }
    static void OpusJitterPlayout(int prebufferMs, int queueMs, bool burstJitter) {
        var format = new CodecSettings(["OPUS"]).Formats().Single();
        const int frames = 140;
        var source = new ContinuityPort(true);
        using var tx = new MediaCodec(format, source);
        var arrivals = Enumerable.Range(0, frames).Select(frame => new {
            Frame = frame, Due = frame * 20 + (burstJitter && frame % 20 is >= 8 and <= 11
                ? (11 - frame % 20) * 20 + 12 : frame % 17 == 7 || frame % 9 == 3 ? 12 : 0),
            Packet = new RtpAudioPacket((ushort)frame, (uint)(frame * 960), 1, format.FormatID, tx.Encode(), 1)
        }).Where(value => value.Frame % 17 != 6).OrderBy(value => value.Due).ToArray();
        foreach (int phase in new[] { 0, 7, 19 }) {
            var clock = new Clock(); var packets = new RtpReceiveQueue(20, clock);
            var sink = new PlayoutPort(prebufferMs, queueMs); using var rx = new MediaCodec(format, sink);
            void Drain(int playoutSamples = 0) {
                while (packets.TryTake(out var packet, out _, playoutDeadline: sink.Queue.Count < playoutSamples)) {
                    try { rx.DecodeRtp(packet); } finally { Array.Clear(packet.Payload); }
                }
            }
            int next = 0, played = 0;
            float previous = 0;
            var output = new float[960];
            for (int time = 0; time <= frames * 20 + 100 && played < frames; time++) {
                clock.Ticks = time;
                while (next < arrivals.Length && arrivals[next].Due <= time) { packets.Add(arrivals[next++].Packet); Drain(); }
                // The WebRTC send clock and SIP playout clock have independent phases.
                if (time % 20 == 5) Drain();
                if (time % 20 != phase) continue;
                Drain(output.Length);
                int count = sink.ReadCaptured(output);
                if (played == 0 && count == 0) continue;
                Check(count == 960, $"loss plus jitter cannot insert a playout gap: prebuffer={prebufferMs}, phase={phase}, time={time}, frame={played}, samples={count}");
                double energy = 0;
                foreach (float value in output) {
                    Check(Math.Abs(value - previous) < .15f, "jitter recovery cannot introduce a PCM boundary click");
                    energy += value * value; previous = value;
                }
                Check(energy / output.Length > .0001, "isolated packet loss conceals audible speech instead of a silent frame");
                played++;
            }
            Check(played == frames && sink.Queue.Dropped == 0 && sink.Queue.Underruns == 0,
                "independent clocks preserve every Opus playout frame through isolated loss and jitter");
            Check(rx.ConcealedSamples == packets.Status.MissingPackets * 960 && packets.Status.MissingPackets == frames - arrivals.Length,
                "deadline recovery reconstructs each lost frame once without extending the audio timeline");
        }
        Console.WriteLine($"phone_opus_jitter_playout_passed: prebuffer={prebufferMs}ms queue={queueMs}ms isolated loss jitter={(burstJitter ? 72 : 12)}ms across independent audio clock phases");
    }
    static void CodecContinuity() {
        var opus = new CodecSettings(["OPUS"]).Formats().Single();
        foreach (string name in new[] { "G722", "PCMA", "PCMU", "EVS" }) {
            var phone = new CodecSettings([name]).Formats().Single();
            var source = new ContinuityPort(true); var relay = new ContinuityPort(); var sink = new ContinuityPort();
            using var voiceTx = new MediaCodec(opus, source); using var voiceRx = new MediaCodec(opus, relay);
            using var phoneTx = new MediaCodec(phone, relay); using var phoneRx = new MediaCodec(phone, sink);
            for (int frame = 0; frame < 200; frame++) {
                voiceRx.Decode(voiceTx.Encode()); phoneRx.Decode(phoneTx.Encode());
            }
            var samples = sink.Samples.Skip(9600).ToArray();
            double jump = samples.Zip(samples.Skip(1), (a, b) => Math.Abs(a - b)).Max();
            double rms = Math.Sqrt(samples.Select(value => (double)value * value).Average());
            Check(samples.Length > 170000 && rms is > .08 and < .3 && jump < .15,
                $"continuous Opus-to-{name} resampling has no frame-boundary clicks: jump={jump:0.000000}, rms={rms:0.000000}");
            Console.WriteLine($"phone_codec_continuity: Opus->{name} jump={jump:0.000000} rms={rms:0.000000}");
        }
    }
    static void OpusPacketRecovery() {
        var format = new CodecSettings(["OPUS"]).Formats().Single();
        var source = new ContinuityPort(true); var sink = new ContinuityPort();
        using var tx = new MediaCodec(format, source);
        var rx = new MediaCodec(format, sink);
        var clock = new Clock(); var queue = new RtpReceiveQueue(20, clock);
        const int frames = 200;
        try {
            for (int frame = 0; frame < frames; frame++) {
                var payload = tx.Encode();
                clock.Ticks += 20;
                if (frame > 10 && frame % 17 == 0) continue;
                queue.Add(new((ushort)frame, (uint)(frame * 960), 1, format.FormatID, payload, sink.Generation));
                while (queue.TryTake(out var packet, out _)) {
                    try {
                        rx.DecodeRtp(packet);
                    } finally { Array.Clear(packet.Payload); }
                }
            }
            Check(queue.Status.MissingPackets > 5, "fixture loses isolated Opus packets throughout the utterance");
            Check(sink.Samples.Count >= frames * 960 - 64,
                $"packet loss preserves the playout timeline instead of inserting silence: {sink.Samples.Count} samples");
            var samples = sink.Samples.Skip(9600).ToArray();
            double jump = samples.Zip(samples.Skip(1), (a, b) => Math.Abs(a - b)).Max();
            Check(jump < .15, $"Opus loss recovery preserves overlapping decoder blocks: jump={jump:0.000000}");
            Console.WriteLine($"phone_opus_recovery: missing={queue.Status.MissingPackets} samples={sink.Samples.Count} jump={jump:0.000000}");
            Check(rx.ConcealedSamples == queue.Status.MissingPackets * 960, "concealment duration follows RTP timestamps");
            long before = sink.Samples.Count;
            var stale = new RtpAudioPacket(200, 200 * 960, 1, format.FormatID, tx.Encode(), sink.Generation);
            sink.Generation++; rx.DecodeRtp(stale);
            Check(sink.Samples.Count == before, "old generation cannot play decoded audio or loss concealment");
            rx.DecodeRtp(stale with { Sequence = 203, Timestamp = 203 * 960, Generation = sink.Generation });
            Check(sink.Samples.Count - before is > 0 and <= 960,
                "new authority starts with the new packet and never conceals the previous generation");
            before = sink.Samples.Count; long concealed = rx.ConcealedSamples;
            rx.DecodeRtp(stale with { Sequence = 204, Timestamp = 204 * 960 + 48000, Generation = sink.Generation });
            Check(sink.Samples.Count - before is > 0 and <= 960 && rx.ConcealedSamples == concealed,
                "a long silence cannot synthesize an unbounded stale audio backlog");
            sink.Generation++;
            rx.DecodeRtp(stale with { Timestamp = uint.MaxValue - 959, Generation = sink.Generation });
            before = sink.Samples.Count; concealed = rx.ConcealedSamples;
            rx.DecodeRtp(stale with { Sequence = 0, Timestamp = 960, Generation = sink.Generation });
            Check(sink.Samples.Count - before == 1920 && rx.ConcealedSamples - concealed == 960,
                "RTP timestamp wrap preserves exactly one lost frame without resetting the decoder");
        } finally { rx.Dispose(); }
        OpusJitterPlayout();
    }
    static void Codecs() {
        Reject(() => new CodecSettings(new[] { "PCMA", "PCMA" }).Validate(), "duplicate codec rejected");
        Reject(() => new CodecSettings(new[] { "unknown" }).Validate(), "unknown codec rejected");
        Reject(() => new CodecSettings(new[] { "PCMA" }, 40).Validate(), "noncontract packet duration rejected");
        Reject(() => new CodecSettings(new[] { "PCMA" }, ReceiveReorderMs: -1).Validate(), "negative receive gap delay rejected");
        Reject(() => new CodecSettings(new[] { "PCMA" }, ReceiveReorderMs: 61).Validate(), "unbounded receive gap delay rejected");
        foreach (var format in new CodecSettings(new[] { "G722", "PCMA", "PCMU", "OPUS" }).Formats()) {
            var source = new AudioPort(); var sink = new AudioPort();
            using var tx = new MediaCodec(format, source); using var rx = new MediaCodec(format, sink);
            using var independent = new AudioEncoder(includeOpus: true);
            Check(tx.RtpDuration == (format.FormatName.Equals("opus", StringComparison.OrdinalIgnoreCase) ? 960u : 160u), "20ms RTP clock including G722");
            for (int frame = 0; frame < 25; frame++) {
                var packet = tx.Encode();
                Check(independent.DecodeAudio(packet, format).Length == format.ClockRate / 50, "codec emits exactly20ms mono PCM, including opus/48000/2");
                rx.Decode(packet);
            }
            Check(sink.Received is > 23000 and <= 24000 && sink.Rms is > .15 and < .55, "stream preserves duration and audible waveform after resampling");
            source.OnRead = () => { source.IsOpen = false; source.Generation++; };
            var revoked = tx.Encode();
            using (var clean = new AudioEncoder(includeOpus: true))
                Check(clean.DecodeAudio(revoked, format).All(sample => Math.Abs((int)sample) < 300), "mid-frame permit revocation discards speech and codec history");
            source.OnRead = null;
            using (var clean = new AudioEncoder(includeOpus: true))
                Check(clean.DecodeAudio(tx.Encode(), format).All(sample => Math.Abs((int)sample) < 300), "closed port emits silence");
            long before = sink.Received; sink.IsOpen = false; rx.Decode(revoked);
            Check(sink.Received == before, "closed receive gate discards decoded media");
            sink.IsOpen = true; sink.Generation++; source.IsOpen = true; source.Generation++; source.Amplitude = 0;
            using (var clean = new AudioEncoder(includeOpus: true))
                Check(clean.DecodeAudio(tx.Encode(), format).All(sample => Math.Abs((int)sample) < 300), "new generation clears predictive audio history");
            Reject(() => rx.Decode(new byte[8193]), "oversized encoded payload rejected");
            long oldGeneration = sink.Generation; before = sink.Received; sink.Generation++;
            rx.Decode(revoked, oldGeneration);
            Check(sink.Received == before, "queued payload cannot acquire the decoder's newer audio generation");
        }
    }
    static async Task Until(Func<bool> condition, string message) {
        var elapsed = Stopwatch.StartNew();
        while (!condition() && elapsed.Elapsed < TimeSpan.FromSeconds(5)) await Task.Delay(10);
        Check(condition(), message);
    }
    static TaskCompletionSource<SipCall> Invitation() => new(TaskCreationOptions.RunContinuationsAsynchronously);
    static async Task CodecPreferenceLoopback() {
        foreach (bool fallback in new[] { false, true }) {
            var firstSettings = new CodecSettings(["OPUS", "G722", "PCMA"]);
            var secondSettings = new CodecSettings(fallback ? ["PCMA"] : ["G722", "PCMA", "OPUS"]);
            var firstAudio = new AudioPort(); var secondAudio = new AudioPort();
            await using var first = new SipMediaSession(firstSettings, firstAudio, IPAddress.Loopback);
            await using var second = new SipMediaSession(secondSettings, secondAudio, IPAddress.Loopback);
            var fromFirst = new TaskCompletionSource<int>(TaskCreationOptions.RunContinuationsAsynchronously);
            var fromSecond = new TaskCompletionSource<int>(TaskCreationOptions.RunContinuationsAsynchronously);
            second.OnRtpPacketReceived += (_, media, packet) => { if (media == SDPMediaTypesEnum.audio) fromFirst.TrySetResult(packet.Header.PayloadType); };
            first.OnRtpPacketReceived += (_, media, packet) => { if (media == SDPMediaTypesEnum.audio) fromSecond.TrySetResult(packet.Header.PayloadType); };
            Check(second.SetRemoteDescription(SdpType.offer, first.CreateOffer(IPAddress.Loopback)) == SetDescriptionResultEnum.OK,
                "real SDP offer negotiates overlapping configured codecs");
            Check(first.SetRemoteDescription(SdpType.answer, second.CreateAnswer(IPAddress.Loopback)) == SetDescriptionResultEnum.OK,
                "real SDP answer completes the agreed codec set");
            firstSettings.Preferences[0] = "PCMU"; // Caller mutation cannot replace the admitted media policy.
            await first.Start(); await second.Start();
            int sentByFirst = await fromFirst.Task.WaitAsync(TimeSpan.FromSeconds(5));
            int sentBySecond = await fromSecond.Task.WaitAsync(TimeSpan.FromSeconds(5));
            var allFormats = new CodecSettings(["OPUS", "G722", "PCMA"]).Formats();
            int Expected(string name) => allFormats.Single(value => value.FormatName.Equals(name, StringComparison.OrdinalIgnoreCase)).FormatID;
            Check(sentByFirst == Expected(fallback ? "PCMA" : "OPUS") && sentBySecond == Expected(fallback ? "PCMA" : "G722"),
                "actual outgoing RTP uses each direction's highest configured common codec, with same-call fallback");
            await Until(() => firstAudio.Received >= 4800 && secondAudio.Received >= 4800, "both chosen RTP payloads decode through the actual negotiated receive set");
            Check(firstAudio.Rms > .1 && secondAudio.Rms > .1, "codec preference preserves bidirectional synthetic audio");
        }
    }
    static async Task SipLoopback(bool includeRegistration = true) {
        await CodecPreferenceLoopback();
        if (includeRegistration) { await RegistrationLoopback(); await RegistrationLoopback(negotiatedLifetime: true); }
        var codec = new CodecSettings(new[] { "G722", "PCMA", "PCMU", "OPUS" });
        var portA = new AudioPort(); var portB = new AudioPort();
        var binding = new SipBinding(IPAddress.Loopback.ToString(), 0, "udp");
        await using var first = new SipTransportHost(binding, codec, _ => portA, _ => false);
        await using var second = new SipTransportHost(binding, codec, _ => portB, incoming => incoming.PeerAddress == IPAddress.Loopback.ToString());
        var invitation = Invitation(); second.Incoming += call => invitation.TrySetResult(call);
        string destination = $"sip:fixture@127.0.0.1:{second.LocalEndpoint.Port}";
        var outgoing = first.PrepareOutgoing(Guid.NewGuid().ToString());
        Check(outgoing.Observation.SipCallId == outgoing.Observation.Id, "outgoing wire Call-ID is known before dispatch");
        Reject(() => first.PrepareOutgoing(Guid.NewGuid().ToString()), "one admitted call per host");
        var dial = outgoing.DialAsync(destination, null, null, 5);
        var incoming = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5));
        Check(incoming.Observation.State == "ringing", "incoming call awaits explicit answer");
        var answered = incoming.AnswerAsync();
        Check((await dial.WaitAsync(TimeSpan.FromSeconds(10))).State == "connected", "outgoing SIP200/ACK connects");
        Check((await answered.WaitAsync(TimeSpan.FromSeconds(10))).State == "connected", "incoming SIP200/ACK connects");
        Check(outgoing.Observation.SipCallId == incoming.Observation.SipCallId, "both peers correlate the original SIP Call-ID");
        await Until(() => outgoing.Media.ReceivedPackets >= 8 && incoming.Media.ReceivedPackets >= 8, "real bidirectional localhost RTP");
        Check(portA.Rms > .1 && portB.Rms > .1, "real RTP delivers synthetic audio both ways");
        outgoing.Hangup();
        await Until(() => incoming.Observation.State == "local_ended", "remote BYE closes the incoming call");
        Check(outgoing.Media.IsClosed && incoming.Media.IsClosed, "hangup closes both media sessions");
        await outgoing.Media.Start(); Check(outgoing.Media.IsClosed && outgoing.Media.CreateOffer() == null, "closed media cannot restart or offer after delayed DNS");
        await first.ReleaseAsync(outgoing); await second.ReleaseAsync(incoming);

        // Unknown callers are rejected before the audio factory or call admission runs.
        int allocations = 0;
        await using var denied = new SipTransportHost(binding, codec, _ => { allocations++; return new AudioPort(); }, _ => false);
        var refused = first.PrepareOutgoing(Guid.NewGuid().ToString());
        Check((await refused.DialAsync($"sip:fixture@127.0.0.1:{denied.LocalEndpoint.Port}", null, null, 3)).State == "local_ended", "forbidden caller cannot connect");
        Check(allocations == 0, "no media allocation before admission"); await first.ReleaseAsync(refused);

        // Ring timeout and cancellation operate on the same call; no replacement INVITE.
        await using var ringing = new SipTransportHost(binding with { IncomingRingSeconds = 1 }, codec, _ => new AudioPort(), _ => true);
        var ringInvitation = Invitation(); int invitations = 0;
        ringing.Incoming += call => { Interlocked.Increment(ref invitations); ringInvitation.TrySetResult(call); };
        var expired = first.PrepareOutgoing(Guid.NewGuid().ToString());
        var expireDial = expired.DialAsync($"sip:fixture@127.0.0.1:{ringing.LocalEndpoint.Port}", null, null, 4);
        var ringCall = await ringInvitation.Task.WaitAsync(TimeSpan.FromSeconds(5));
        await using var third = new SipTransportHost(binding, codec, _ => new AudioPort(), _ => false);
        var busy = third.PrepareOutgoing(Guid.NewGuid().ToString());
        Check((await busy.DialAsync($"sip:fixture@127.0.0.1:{ringing.LocalEndpoint.Port}", null, null, 3)).State == "local_ended", "second caller receives busy");
        Check((await expireDial.WaitAsync(TimeSpan.FromSeconds(6))).State == "local_ended", "unanswered ring deadline rejects call");
        Check(invitations == 1 && ringCall.Media.IsClosed, "busy and retransmissions do not create another admission");
        await first.ReleaseAsync(expired);
        await Until(() => ringing.CurrentCall == null, "unclaimed expired offer releases its original owner automatically");

        invitation = Invitation(); var cancelled = first.PrepareOutgoing(Guid.NewGuid().ToString());
        var cancelledDial = cancelled.DialAsync(destination, null, null, 5);
        incoming = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5));
        cancelled.Hangup(); await cancelledDial.WaitAsync(TimeSpan.FromSeconds(7));
        await Until(() => incoming.Observation.State == "local_ended", "remote CANCEL ends ringing call");
        bool duplicateRejected = false;
        try { await cancelled.DialAsync(destination, null, null, 5); } catch (InvalidOperationException) { duplicateRejected = true; }
        Check(duplicateRejected, "original call cannot be redialed after cancellation");
        await first.ReleaseAsync(cancelled);
        await Until(() => second.CurrentCall == null, "unclaimed cancelled offer releases its original owner automatically");
    }
    static async Task RegistrationLoopback(bool negotiatedLifetime = false) {
        using var registrar = new SIPTransport();
        var channel = new SIPUDPChannel(new IPEndPoint(IPAddress.Loopback, 0)); registrar.AddSIPChannel(channel);
        int registrations = 0, removals = 0;
        registrar.SIPTransportRequestReceived += async (local, remote, request) => {
            Check(request.Method == SIPMethodsEnum.REGISTER && IPAddress.IsLoopback(remote.Address), "fixture only receives local REGISTER");
            if (request.Header.Expires == 0) Interlocked.Increment(ref removals); else Interlocked.Increment(ref registrations);
            var response = SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.Ok, null);
            response.Header.Expires = request.Header.Expires; response.Header.Contact = request.Header.Contact;
            if (negotiatedLifetime && request.Header.Expires != 0) {
                response.Header.Expires = 0;
                response.Header.Contact[0].Expires = 120;
            }
            await registrar.SendResponseAsync(response);
        };
        var settings = new SipRegistrationSettings("sip:fixture@127.0.0.1", $"sip:127.0.0.1:{channel.ListeningEndPoint.Port}", "fixture", "fixture", null, 60, 10);
        Reject(() => (settings with { ExpirySeconds = 0 }).Validate(SIPProtocolsEnum.udp), "invalid registration expiry is rejected, not silently defaulted");
        Reject(() => settings.Validate(SIPProtocolsEnum.tcp), "registration cannot silently switch transport");
        await using var client = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp"), new CodecSettings(new[] { "PCMA" }), _ => new AudioPort(), _ => false);
        var registration = client.StartRegistration(settings, "synthetic-fixture-only");
        await Until(() => registration.Observation.State == "registered", "real localhost registration receives200");
        await registration.SendKeepAliveAsync();
        Check(registration.Observation.LastKeepAliveAt != null && registration.Observation.LastKeepAliveError == null &&
            registration.Observation.RemoteEndpoint != null && registration.Observation.GrantedExpirySeconds > 0,
            "keepalive succeeds through original registered socket and exposes flow diagnostics");
        Reject(() => client.StartRegistration(settings, "synthetic-fixture-only"), "second registration on same host rejected");
        registration = await client.ReconnectRegistrationAsync(settings, "synthetic-fixture-only");
        await Until(() => registration.Observation.State == "registered", "explicit reconnect obtains a fresh registration without replacing SIP transport");
        await registration.DisposeAsync();
        Check(registrations == 2 && removals == 2 && registration.Observation.State == "stopped" && registration.Observation.RemoteRemoved == true,
            "local stop sends exact zero-expiry registration and confirms removal");
        await registration.DisposeAsync(); Check(removals == 2, "repeated dispose cannot send another unregister");
    }
    static async Task RegistrationRejectThenRecover() {
        using var registrar = new SIPTransport();
        var channel = new SIPUDPChannel(new IPEndPoint(IPAddress.Loopback, 0)); registrar.AddSIPChannel(channel);
        int attempts = 0;
        registrar.SIPTransportRequestReceived += async (_, _, request) => {
            if (request.Method != SIPMethodsEnum.REGISTER) return;
            int attempt = Interlocked.Increment(ref attempts);
            var response = SIPResponse.GetResponse(request,
                attempt == 1 ? SIPResponseStatusCodesEnum.Forbidden : SIPResponseStatusCodesEnum.Ok, null);
            if (attempt != 1) { response.Header.Expires = request.Header.Expires; response.Header.Contact = request.Header.Contact; }
            await registrar.SendResponseAsync(response);
        };
        await using var client = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp"),
            new CodecSettings(new[] { "PCMA" }), _ => new AudioPort(), _ => false);
        var settings = new SipRegistrationSettings("sip:fixture@127.0.0.1",
            $"sip:127.0.0.1:{channel.ListeningEndPoint.Port}", "fixture", "fixture", null, 60, 10);
        var registration = client.StartRegistration(settings, "synthetic-fixture-only");
        var elapsed = Stopwatch.StartNew();
        while (registration.Observation.State != "registered" && elapsed.Elapsed < TimeSpan.FromSeconds(20)) await Task.Delay(50);
        Check(registration.Observation.State == "registered" && Volatile.Read(ref attempts) >= 2,
            "one definite registrar refusal is retried and recovers without reconnecting the SIP host");
        await registration.DisposeAsync();
    }
    static void RegistrationExpiry() {
        var header = new SIPHeader { Expires = 0, Contact = [new SIPContactHeader(null, SIPURI.ParseSIPURI("sip:fixture@127.0.0.1")) { Expires = 120 }] };
        Check(SipRegistration.EffectiveExpiry(header, 60) == 120, "contact expiry overrides zero global header and shorter requested lifetime");
        header.Contact[0].Expires = 0; header.Expires = 120;
        Check(SipRegistration.EffectiveExpiry(header, 60) == 0, "explicit contact removal cannot inherit positive global expiry");
        header.Contact[0].Expires = -1;
        Check(SipRegistration.EffectiveExpiry(header, 60) == 120, "absent contact expiry uses server header without requested cap");
        header.Expires = -1;
        Check(SipRegistration.EffectiveExpiry(header, 60) == 60, "absent server lifetime retains requested lifetime");
    }
}
