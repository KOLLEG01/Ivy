using System.Net;
using System.Diagnostics;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;
using SIPSorceryMedia.Abstractions;

static partial class Program {
    static async Task EvsPerformance() {
        const int frames = 100;
        var pcm = new short[EvsNativeCodec.Samples];
        for (int index = 0; index < pcm.Length; index++)
            pcm[index] = (short)Math.Round(Math.Sin(2 * Math.PI * 997 * index / EvsNativeCodec.SampleRate) * short.MaxValue * .2);
        using var encoder = new EvsNativeCodec(true);
        using var decoder = new EvsNativeCodec(false);
        byte[] packet = encoder.Encode(pcm, false);
        _ = decoder.Decode(packet, false);
        var watch = Stopwatch.StartNew();
        for (int frame = 0; frame < frames; frame++) packet = encoder.Encode(pcm, false);
        watch.Stop();
        double encodeMs = watch.Elapsed.TotalMilliseconds / frames;
        watch.Restart();
        for (int frame = 0; frame < frames; frame++) _ = decoder.Decode(packet, false);
        watch.Stop();
        double decodeMs = watch.Elapsed.TotalMilliseconds / frames;
        var leftAudio = new AudioPort(); var rightAudio = new AudioPort();
        await using var left = new SipMediaSession(new CodecSettings(["EVS"]), leftAudio, IPAddress.Loopback);
        await using var right = new SipMediaSession(new CodecSettings(["EVS"]), rightAudio, IPAddress.Loopback);
        Check(right.SetRemoteDescription(SdpType.offer, left.CreateOffer(IPAddress.Loopback)) == SetDescriptionResultEnum.OK,
            "performance fixture negotiates EVS offer");
        Check(left.SetRemoteDescription(SdpType.answer, right.CreateAnswer(IPAddress.Loopback)) == SetDescriptionResultEnum.OK,
            "performance fixture negotiates EVS answer");
        await left.Start(); await right.Start();
        await Task.Delay(TimeSpan.FromSeconds(5));
        Check(left.SentPackets is >= 240 and <= 260 && right.SentPackets is >= 240 and <= 260,
            "dedicated RTP media threads sustain the exact20ms cadence");
        Console.WriteLine($"phone_evs_performance: encode={encodeMs:0.000}ms decode={decodeMs:0.000}ms total={encodeMs + decodeMs:0.000}ms " +
            $"per20ms frame; loopback left={left.SentPackets / 5d:0.0}fps right={right.SentPackets / 5d:0.0}fps");
    }
    static void EvsParameters() {
        Check(EvsFormat.Mono("EVS/16000") && EvsFormat.Mono("evs/16000/1") && !EvsFormat.Mono("EVS/32000/1") && !EvsFormat.Mono("EVS/16000/2"),
            "both valid mono RTPMAP forms use the fixed16kHz RTP clock");
        Check(EvsFormat.Channels("EVS/16000/2") == 2 && EvsFormat.Channels("EVS/16000/3") == 0, "bounded stereo RTPMAP");
        foreach (string offer in new[] { "", "br=5.9-24.4;bw=nb-swb;max-red=0", EvsFormat.OfferParameters, "br=8-16.4", "bw=nb-wb", "br=32-128",
            "br=24.4;bw=swb;hf-only=1", "br-send=8-16.4;br-recv=24.4;bw-send=wb;bw-recv=swb",
            "br=24.4;br-send=24.40;br-recv=24.4;bw=swb;bw-send=swb" }) {
            Check(EvsFormat.TrySelect(offer, true, out string selected) && !selected.Contains("dtx=") && !selected.Contains("cmr=") &&
                !selected.Contains("ch-aw-recv="), "admitted EVS offer retains IvySIP's interoperable optional-parameter defaults");
        }
        Check(EvsFormat.TrySelect("br-send=8-16.4;br-recv=24.4;bw-send=wb;bw-recv=swb;hf-only=1", true, out var asymmetric) &&
            asymmetric.Contains("br-send=24.4") && asymmetric.Contains("br-recv=8-16.4") && asymmetric.Contains("bw-send=swb") &&
            asymmetric.Contains("bw-recv=wb") && EvsFormat.HeaderFull(asymmetric), "asymmetric offer swaps send/receive constraints without widening them");
        Check(EvsFormat.Sender(EvsFormat.OfferParameters) == (128000, 3), "unrestricted peer selects128kbit/s Fullband");
        Check(EvsFormat.Sender("br=5.9-24.4;bw=nb-swb") == (24400, 2), "legacy peer retains24.4kbit/s SWB");
        Check(EvsFormat.Sender("br=8-16.4;bw=nb-wb") == (16400, 1), "restricted peer selects its highest valid rate and bandwidth");
        Check(EvsFormat.TrySelect(EvsFormat.Parameters(2), true, out _, 2), "stereo with Header-Full-only is supported");
        Check(!EvsFormat.TrySelect("br=128;bw=fb;channels=2", true, out _, 2) &&
            !EvsFormat.TrySelect("br=128;bw=fb;channels=2;hf-only=1;ch-send=1", true, out _, 2), "stereo requires complete unambiguous channel blocks");
        foreach (string offer in new[] { "br=5.9;bw=wb", "br=128;bw=nb", "br=7.2;bw=fb", "br=24.4-5.9", "br=12", "bw=wb-fb",
            "dtx=1", "dtx-recv=1", "cmr=0", "cmr=1", "evs-mode-switch=1", "ch-aw-recv=3", "hf-only=2", "channels=2",
            "br=24.4;br=24.4", "br=24.4;BR=24.4", "br=24.4;br-send=16.4", "br=24.4\r\na=evil:1", new string('x', 1025) })
            Check(!EvsFormat.TrySelect(offer, true, out _), "invalid or incompatible EVS offer cannot advertise unsupported media: " + offer[..Math.Min(offer.Length, 60)]);
        Check(EvsFormat.TrySelect(EvsFormat.OfferParameters, false, out _), "answer retaining offered parameters is accepted");
        Check(EvsFormat.TrySelect("br=7.2-24.4;bw=nb-swb", false, out _), "answer may narrow rates and omit optional DTX and CMR defaults");
        foreach (string answer in new[] { "", "br=5.9-128;bw=nb-fb", "br=7.2-128;bw=nb-fb;dtx=1" })
            Check(!EvsFormat.TrySelect(answer, false, out _), "answer cannot erase, widen or enable unsupported offered constraints");
    }
    static void EvsManagedCodecs() {
        new CodecSettings(["EVS", "OPUS", "G722", "PCMA", "PCMU"]).Validate();
        foreach (var (channels, full) in new[] { (1, false), (1, true), (2, true) }) {
            string parameters = EvsFormat.Parameters(channels) + (channels == 1 && full ? ";hf-only=1" : "");
            // Parsed SDP reports the RTP rate as ClockRate. PCM processing must still use48kHz.
            var format = new AudioFormat(112, "EVS", 16000, 16000, channels, parameters);
            var source = new AudioPort(); var sink = new AudioPort();
            using var tx = new MediaCodec(format, source); using var rx = new MediaCodec(format, sink);
            Check(tx.RtpDuration == 320, "EVS20ms consumes320 RTP ticks, never640 or960");
            using var independent = new EvsNativeCodec(false, channels: channels);
            for (int frame = 0; frame < 50; frame++) {
                byte[] packet = tx.Encode(); byte[] original = packet.ToArray();
                Check(packet.Length == channels * (full ? 321 : 320), "128kbit/s payload with negotiated channels/header mode");
                Check(independent.Decode(packet, full).Length == 960, "native ABI yields20ms of48kHz mono PCM");
                rx.Decode(packet); Check(packet.SequenceEqual(original), "managed decoding never mutates the RTP payload");
            }
            Check(sink.Received is > 47000 and <= 48000 && sink.Rms is > .1 and < .6,
                "actual EVS encode/decode preserves one second of audible synthetic audio");
            source.OnRead = () => { source.IsOpen = false; source.Generation++; };
            byte[] revoked = tx.Encode();
            using (var clean = new EvsNativeCodec(false, channels: channels))
                Check(clean.Decode(revoked, full).All(sample => Math.Abs((int)sample) < 300), "revocation discards encoder prediction and captured speech");
            source.OnRead = null; source.IsOpen = true; source.Generation++; source.Amplitude = 0;
            using (var clean = new EvsNativeCodec(false, channels: channels))
                Check(clean.Decode(tx.Encode(), full).All(sample => Math.Abs((int)sample) < 300), "new permission generation has no previous speech history");
            long before = sink.Received; sink.IsOpen = false; rx.Decode(revoked);
            Check(sink.Received == before, "closed receive gate emits no decoded EVS audio");
            sink.IsOpen = true; long old = sink.Generation++;
            rx.Decode(revoked, old); Check(sink.Received == before, "queued previous-generation EVS payload remains discarded");
            Reject(() => rx.Decode(new byte[4097]), "bounded native EVS payload");
            using var decoder = new EvsNativeCodec(false, channels: channels);
            Reject(() => decoder.Decode(new byte[] { 13 }, true), "reserved header-full frame type rejected");
            Check(decoder.Decode(revoked, full).Length == 960, "invalid payload does not poison decoder state");
            Reject(() => decoder.Encode(new short[960], full), "receive handle cannot become sender");
        }
    }
    static async Task EvsNegotiation() {
        foreach (var (channels, full) in new[] { (1, false), (1, true), (2, true) }) foreach (bool incompatible in new[] { false, true })
        foreach (string profile in new[] { EvsFormat.OfferParameters, "br=5.9-24.4;bw=nb-swb", "br=8-16.4;bw=wb" }) {
            var settings = new CodecSettings(["EVS", "PCMA"]);
            var firstAudio = new AudioPort(); var secondAudio = new AudioPort();
            await using var first = new SipMediaSession(settings, firstAudio, IPAddress.Loopback);
            await using var second = new SipMediaSession(settings, secondAudio, IPAddress.Loopback);
            var offer = first.CreateOffer(IPAddress.Loopback);
            var media = offer.Media.Single(value => value.Media == SDPMediaTypesEnum.audio);
            var evs = media.MediaFormats.Values.Single(value => EvsFormat.Channels(value.Rtpmap) == channels);
            foreach (var other in media.MediaFormats.Values.Where(value => EvsFormat.Channels(value.Rtpmap) > 0 && value.ID != evs.ID).ToArray())
                media.MediaFormats.Remove(other.ID);
            string rtpmap = channels == 2 ? "EVS/16000/2" : full ? "EVS/16000/1" : "EVS/16000";
            string parameters = incompatible ? "dtx=1" : profile + (channels == 2 ? ";channels=2;hf-only=1" : full ? ";hf-only=1" : "");
            media.MediaFormats[evs.ID] = new SDPAudioVideoMediaFormat(evs.Kind, evs.ID, rtpmap, parameters);
            string original = offer.ToString();
            Check(second.SetRemoteDescription(SdpType.offer, offer) == SetDescriptionResultEnum.OK, "EVS or same-call PCMA fallback negotiated");
            Check(offer.ToString() == original, "negotiation never edits the original remote description");
            var answer = second.CreateAnswer(IPAddress.Loopback);
            var answerEvs = answer.Media.Single(value => value.Media == SDPMediaTypesEnum.audio).MediaFormats.Values
                .SingleOrDefault(value => value.Name().Equals("EVS", StringComparison.OrdinalIgnoreCase));
            Check(incompatible ? answerEvs.Rtpmap == null : string.Equals(answerEvs.Rtpmap, rtpmap, StringComparison.OrdinalIgnoreCase),
                "answer mirrors valid peer channels/spelling and removes incompatible EVS");
            if (!incompatible) Check(answer.ToString().Contains("a=ptime:20") && answer.ToString().Contains("a=maxptime:120"), "SDP advertises20ms sending and120ms receive bounds");
            Check(first.SetRemoteDescription(SdpType.answer, answer) == SetDescriptionResultEnum.OK, "answer completes original media negotiation");
            var payload = new TaskCompletionSource<(int Payload, int Bytes)>(TaskCreationOptions.RunContinuationsAsynchronously);
            var timestamps = new List<uint>();
            second.OnRtpPacketReceived += (_, kind, packet) => {
                if (kind != SDPMediaTypesEnum.audio) return;
                payload.TrySetResult((packet.Header.PayloadType, packet.Payload.Length));
                lock (timestamps) { if (timestamps.Count < 3) timestamps.Add(packet.Header.Timestamp); }
            };
            await first.Start(); await second.Start();
            var sent = await payload.Task.WaitAsync(TimeSpan.FromSeconds(5));
            Check(sent.Payload == (incompatible ? 8 : evs.ID), "actual RTP follows configured agreed codec priority");
            if (!incompatible) Check(sent.Bytes == channels * (EvsFormat.Sender(profile).Bitrate / 400 + (full ? 1 : 0)),
                "actual RTP payload uses the highest mutually agreed bitrate and complete coded channels");
            await Until(() => firstAudio.Received >= 9600 && secondAudio.Received >= 9600, "actual bidirectional local RTP carries negotiated synthetic audio");
            Check(!first.Failed && !second.Failed && firstAudio.Rms > .1 && secondAudio.Rms > .1, "native EVS/PCMA media remains audible and healthy");
            lock (timestamps) Check(timestamps.Count == 3 && unchecked(timestamps[1] - timestamps[0]) == (incompatible ? 160u : 320u) &&
                unchecked(timestamps[2] - timestamps[1]) == (incompatible ? 160u : 320u), "actual RTP timestamp progression matches advertised clock");
            if (incompatible) {
                // A new remote offer can reintroduce a configured codec removed by the first intersection.
                media.MediaFormats[evs.ID] = new SDPAudioVideoMediaFormat(evs.Kind, evs.ID, rtpmap, EvsFormat.Parameters(channels));
                Check(second.SetRemoteDescription(SdpType.offer, offer) == SetDescriptionResultEnum.OK && second.CreateAnswer(IPAddress.Loopback).ToString().Contains("EVS/16000"),
                    "re-INVITE can negotiate EVS after an earlier same-call PCMA fallback");
            }
        }
        foreach (string mode in new[] { "initial", "delayed" }) {
            var settings = new CodecSettings(["EVS"]); var leftAudio = new AudioPort(); var rightAudio = new AudioPort();
            await using var left = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp", OutgoingOfferMode: mode), settings, _ => leftAudio, _ => false);
            await using var right = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp"), settings, _ => rightAudio, _ => true);
            var offered = Invitation(); right.Incoming += call => offered.TrySetResult(call);
            var outgoing = left.PrepareOutgoing(Guid.NewGuid().ToString());
            var dial = outgoing.DialAsync($"sip:evs@127.0.0.1:{right.LocalEndpoint.Port}", null, null, 5);
            var incoming = await offered.Task.WaitAsync(TimeSpan.FromSeconds(5)); var answer = incoming.AnswerAsync();
            Check((await dial.WaitAsync(TimeSpan.FromSeconds(10))).State == "connected" && (await answer.WaitAsync(TimeSpan.FromSeconds(10))).State == "connected", "EVS-only original " + mode + " SIP offer/answer connects");
            await Until(() => leftAudio.Received >= 9600 && rightAudio.Received >= 9600, "EVS SIP call carries actual local RTP in both directions");
            Check(leftAudio.Rms > .1 && rightAudio.Rms > .1, "EVS SIP call delivers audible synthetic audio");
            outgoing.Hangup(); await Until(() => incoming.Observation.State == "local_ended", "original BYE closes EVS call");
            await left.ReleaseAsync(outgoing); await right.ReleaseAsync(incoming);
        }
    }
}
