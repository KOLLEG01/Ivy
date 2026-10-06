using System.Net;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;
using System.Security.Cryptography;

static partial class Program {
    static async Task PhoneParity() {
        var meter = new LoopbackProbeMeter();
        meter.Add([float.NaN, float.PositiveInfinity], true);
        Check(meter.Result("completed").Samples == 2 && !meter.Result("completed").Detected, "silent probe buffers are counted without inspecting invalid silence contents");
        meter.Add([.2f, -.4f], false);
        Check(meter.Result("completed").Detected && meter.Result("completed").Peak == .4f, "probe retains only bounded peak and sample counts");
        Check(!meter.Result("interrupted").Detected, "interrupted probe cannot report successful detection");
        Reject(() => meter.Add([float.NaN], false), "nonfinite observed probe samples are refused");
        var codecTest = PhoneCodecDiagnostics.Run();
        Check(codecTest.Codecs.Length == 5 && codecTest.Codecs.Where(value => value.Name != "EVS").All(value => value.Passed),
            "installed managed codecs pass actual production encoder/decoder/resampler diagnostics");
        var evs = codecTest.Codecs.Single(value => value.Name == "EVS");
        Check(evs.Passed || evs.Error == "codec_library_missing", "EVS diagnostic passes a packaged library or explicitly reports absent development library");
        Check(codecTest.Passed == codecTest.Codecs.All(value => value.Passed), "aggregate codec diagnostic never hides a missing codec");
        var limiter = new DtmfAccessFailureLimiter(); int ended = 0;
        using (var features = new PhoneCallFeatures(limiter, () => Interlocked.Increment(ref ended))) {
            features.Configure(new("*123#"), true);
            foreach (char digit in "9*12") features.Digit(digit, DtmfInputSource.SipInfo);
            Check(features.Observation.Access == "awaiting_code" && ended == 0, "partial code never reveals a mismatch or grants access");
            foreach (char digit in "3#") features.Digit(digit, DtmfInputSource.SipInfo);
            Check(features.Authenticated && !features.Observation.DisableCodecUpgrade, "complete code authenticates");
            features.Connected();
            foreach (char digit in "*0#") features.Digit(digit, DtmfInputSource.SipInfo);
            Check(features.Observation.Command == "new_voice" && features.Observation.CommandSequence == 1, "authorized semantic command contains no digits");
            Check(features.Observation.CommandState == "pending", "authorized Voice transition is exposed for the Phone owner");
        }
        using (var features = new PhoneCallFeatures(limiter, () => Interlocked.Increment(ref ended))) {
            features.Configure(null, false); features.Connected();
            foreach (char digit in "*0#") features.Digit(digit, DtmfInputSource.Rfc4733);
            Check(features.Observation.Command == "new_voice" && features.Observation.CommandSequence == 1,
                "outgoing authenticated calls expose the same Voice recycle command");
            foreach (char digit in "*123#") features.Digit(digit, DtmfInputSource.Rfc4733);
            Check(features.Observation.Command == "select_voice" && features.Observation.Model == "sol" &&
                features.Observation.ReasoningEffort == "high" && features.Observation.CommandSequence == 2,
                "function, model and reasoning keys select a semantic Voice configuration without recycling");
            foreach (char digit in "*116#") features.Digit(digit, DtmfInputSource.Rfc4733);
            Check(features.Observation.CommandSequence == 2, "unsupported model and reasoning pairs are ignored");
            foreach (char digit in "*0#") features.Digit(digit, DtmfInputSource.Rfc4733);
            Check(features.Observation.Command == "new_voice" && features.Observation.Model == "sol" &&
                features.Observation.ReasoningEffort == "high" && features.Observation.CommandSequence == 3,
                "restart retains the current selection even when PhoneBridge misses an intermediate poll");
            Check(features.Observation.Commands.Select(item => (item.Command, item.Sequence)).SequenceEqual(
                new[] { ("new_voice", 1L), ("select_voice", 2L), ("new_voice", 3L) }),
                "all completed keypad commands remain ordered between observations");
            Check(features.NeedsInBandDtmf, "connected Voice calls keep in-band command detection available");
        }
        using (var features = new PhoneCallFeatures(limiter, () => Interlocked.Increment(ref ended))) {
            features.Configure(new("*123#"), true);
            foreach (char digit in "**123#") features.Digit(digit, DtmfInputSource.Rfc4733);
            Check(features.Authenticated && features.Observation.DisableCodecUpgrade, "double star preserves same authentication and disables upgrade");
        }
        using (var features = new PhoneCallFeatures(limiter, () => Interlocked.Increment(ref ended))) {
            features.Configure(new("*123#", MaxFailedAttempts: 1, FailureResponseDelayMilliseconds: 0), true);
            foreach (char digit in "*999#") features.Digit(digit, DtmfInputSource.SipInfo);
            Check(!features.Authenticated && features.Observation.Access == "invalid_code", "wrong complete code fails closed");
            await Until(() => ended == 1, "failed code ends original call");
        }
        using (var features = new PhoneCallFeatures(limiter, () => { })) {
            bool limited = false;
            try { features.Configure(new("*123#", MaxFailedAttempts: 1), true); }
            catch (NativeRpcException error) { limited = error.Code == "phone_access_limited"; }
            Check(limited, "failed challenges rate limit subsequent untrusted calls");
            Check(!features.Authenticated, "rate-limited challenges cannot retain trusted audio authority");
            features.Connected();
            foreach (char digit in "*123#*0#") features.Digit(digit, DtmfInputSource.SipInfo);
            Check(!features.Authenticated && features.Observation.CommandSequence == 0,
                "rate-limited calls cannot authenticate or issue Voice commands");
        }
        Check(SipInfoDtmfParser.TryParse("application/dtmf-relay", "Signal=1\r\nDuration=100", out char parsed) && parsed == '1', "SIP INFO relay parser");
        Check(!SipInfoDtmfParser.TryParse("application/dtmf", "12", out _), "multi-digit INFO rejected");
        OutgoingRegistrationIdentity();
        var binding = new SipBinding("127.0.0.1", 0, "udp", OutgoingOfferMode: "auto", RtpPortRangeStart: 32000, RtpPortRangeEnd: 32020);
        Reject(() => (binding with { RtpPortRangeStart = 32001 }).Endpoint(), "odd RTP range rejected");
        Reject(() => (binding with { RtpPortRangeEnd = 31000 }).Endpoint(), "reversed RTP range rejected");
        await using (var host = new SipTransportHost(binding, new(["PCMA"]), _ => new AudioPort(), _ => false)) {
            var call = host.PrepareOutgoing(Guid.NewGuid().ToString());
            int port = call.Media.CreateOffer(IPAddress.Loopback).Media.Single(value => value.Media == SDPMediaTypesEnum.audio).Port;
            Check(port is >= 32000 and <= 32020 && port % 2 == 0, "actual SDP advertises allocated configured RTP port");
            await host.ReleaseAsync(call);
        }
        await RegistrationLoopback();
        await ScreeningLoopback();
        await ChallengeLoopback();
        await TelephoneEventsLoopback();
        await ConcurrentCallsLoopback();
        await ExplicitProxyLoopback();
        await using (var multi = new SipTransportHost(new("127.0.0.1", 0, "udp", MaxConcurrentCalls: 2), new(["PCMA"]), _ => new AudioPort(), _ => false)) {
            var first = multi.PrepareOutgoing(Guid.NewGuid().ToString());
            var second = multi.PrepareOutgoing(Guid.NewGuid().ToString());
            Reject(() => multi.PrepareOutgoing(Guid.NewGuid().ToString()), "configured native concurrency is bounded");
            await multi.ReleaseAsync(first);
            Check(multi.FindCall(second.Observation.Id) == second && !second.Media.IsClosed, "releasing one call leaves other media and ownership intact");
            await multi.ReleaseAsync(second);
        }
        Console.WriteLine("phone_parity_passed: access challenge, limiter, semantic commands, INFO parsing, RTP port allocation and registration keepalive; no Desktop or carrier");
    }
    static void OutgoingRegistrationIdentity() {
        var registeredDescriptor = SipCall.BuildCallDescriptor("sip:+491701234567@tel.t-online.de", "anonymous@t-online.de", "", new(
            "sip:+4998611234567@tel.t-online.de", "sip:tel.t-online.de", "+4998611234567", "anonymous@t-online.de", "tel.t-online.de", 120, 30));
        Check(registeredDescriptor.Username == "+4998611234567" && registeredDescriptor.From == "sip:+4998611234567@tel.t-online.de" &&
            registeredDescriptor.AuthUsername == "anonymous@t-online.de" && !string.IsNullOrWhiteSpace(registeredDescriptor.Password),
            "registered outgoing descriptor keeps public caller identity separate from access-line digest credentials");
    }
    static async Task TelephoneEventsLoopback() {
        // Two advertised event payloads exercise both SIPSorcery's selected event callback and
        // the secondary negotiated payload path. Packets travel through the actual UDP socket.
        var settings = new CodecSettings(["PCMA"]);
        using var source = new RTPSession(false, false, false, IPAddress.Loopback);
        source.addTrack(new MediaStreamTrack(settings.Formats().Concat([
            new SIPSorceryMedia.Abstractions.AudioFormat(110, "telephone-event", 8000, 1, "0-16"),
            new SIPSorceryMedia.Abstractions.AudioFormat(113, "telephone-event", 8000, 1, "0-16")]).ToList()));
        var protectedAudio = new AudioPort { IsOpen = false };
        await using var target = new SipMediaSession(settings, protectedAudio, IPAddress.Loopback);
        using var features = new PhoneCallFeatures(new DtmfAccessFailureLimiter(), () => { });
        features.Configure(new("*123#"), true); target.EnableDtmf(features.Digit, () => features.NeedsInBandDtmf);
        Check(target.SetRemoteDescription(SdpType.offer, source.CreateOffer(IPAddress.Loopback)) == SetDescriptionResultEnum.OK,
            "multiple telephone-event payloads negotiate on the actual media session");
        Check(source.SetRemoteDescription(SdpType.answer, target.CreateAnswer(IPAddress.Loopback)) == SetDescriptionResultEnum.OK,
            "telephone-event sender accepts the answer");
        await source.Start(); await target.Start();
        ushort sequence = 1; uint timestamp = 1000;
        void Send(byte digit, int payload) {
            // End-event retransmissions with distinct RTP sequence numbers are still one digit.
            for (int duplicate = 0; duplicate < 3; duplicate++)
                source.SendRtpRaw(SDPMediaTypesEnum.audio, [digit, 0x8a, 0x03, 0x20], timestamp, 0, payload, sequence++);
            timestamp += 1600;
        }
        foreach (byte digit in new byte[] { 10, 1, 2, 3, 11 }) Send(digit, 114);
        using (var foreign = new System.Net.Sockets.UdpClient(new IPEndPoint(IPAddress.Loopback, 0))) {
            foreach (byte digit in new byte[] { 10, 1, 2, 3, 11 }) {
                var bytes = new byte[16]; bytes[0] = 0x80; bytes[1] = 113; bytes[12] = digit; bytes[13] = 0x8a;
                System.Buffers.Binary.BinaryPrimitives.WriteUInt16BigEndian(bytes.AsSpan(2), sequence++);
                System.Buffers.Binary.BinaryPrimitives.WriteUInt32BigEndian(bytes.AsSpan(4), timestamp);
                timestamp += 1600;
                await foreign.SendAsync(bytes, target.AudioStream.GetRTPChannel().RTPLocalEndPoint);
            }
        }
        await Task.Delay(100);
        Check(!features.Authenticated && !target.Failed, "unadvertised payloads and a foreign UDP sender cannot authenticate");
        foreach (byte digit in new byte[] { 10, 1, 2, 3, 11 }) Send(digit, 113);
        await Until(() => features.Authenticated, "secondary advertised telephone-event payload authenticates over real RTP");
        features.Connected();
        foreach (byte digit in new byte[] { 10, 0, 11 }) Send(digit, 110);
        await Until(() => features.Observation.CommandSequence == 1, "primary event payload invokes one semantic command despite end retransmissions");
        Check(protectedAudio.Received == 0 && !target.Failed, "telephone events never open or enter protected audio");
        target.Close("fixture_complete"); source.Close("fixture_complete");
    }
    static async Task ExplicitProxyLoopback() {
        foreach (string mode in new[] { "initial", "delayed" }) {
            await using var receiver = new SipTransportHost(new("127.0.0.1", 0, "udp"), new(["PCMA"]), _ => new AudioPort(), _ => true);
            await using var caller = new SipTransportHost(new("127.0.0.1", 0, "udp", OutgoingOfferMode: mode,
                OutboundProxy: $"udp:127.0.0.1:{receiver.LocalEndpoint.Port}"), new(["PCMA"]), _ => new AudioPort(), _ => false);
            var invitation = Invitation(); receiver.Incoming += value => invitation.TrySetResult(value);
            var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString()); outgoing.ConfigureFeatures(null);
            var dial = outgoing.DialAsync("sip:fixture@127.0.0.2:9", null, null, 5);
            var incoming = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5)); incoming.ConfigureFeatures(null);
            await incoming.AnswerAsync(); await dial;
            Check(outgoing.Observation.State == "connected", "configured proxy routes actual " + mode + " offer independently of Request-URI endpoint");
            outgoing.Hangup(); await caller.ReleaseAsync(outgoing); await receiver.ReleaseAsync(incoming);
        }
    }
    static async Task ConcurrentCallsLoopback() {
        await using var caller = new SipTransportHost(new("127.0.0.1", 0, "udp", MaxConcurrentCalls: 2), new(["PCMA"]), _ => new AudioPort(), _ => false);
        await using var receiver = new SipTransportHost(new("127.0.0.1", 0, "udp", MaxConcurrentCalls: 2), new(["PCMA"]), _ => new AudioPort(), _ => true);
        var offers = System.Threading.Channels.Channel.CreateUnbounded<SipCall>();
        receiver.Incoming += value => offers.Writer.TryWrite(value);
        async Task<(SipCall Outgoing, SipCall Incoming)> Connect() {
            var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString()); outgoing.ConfigureFeatures(null);
            var dial = outgoing.DialAsync($"sip:fixture@127.0.0.1:{receiver.LocalEndpoint.Port}", null, null, 5);
            var incoming = await offers.Reader.ReadAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(5)); incoming.ConfigureFeatures(null);
            await incoming.AnswerAsync(); await dial; return (outgoing, incoming);
        }
        var first = await Connect(); var second = await Connect();
        Check(caller.Calls.Length == 2 && receiver.Calls.Length == 2, "two real SIP dialogs coexist");
        var signalling = System.Text.Json.JsonSerializer.SerializeToElement(second.Outgoing.Signalling.Observation, NativeRpc.Json);
        Check(signalling.GetProperty("events").EnumerateArray().Any(value => value.GetProperty("status").ValueKind == System.Text.Json.JsonValueKind.Number && value.GetProperty("status").GetInt32() == 200), "SIP diagnostics retain actual response status");
        Check(!signalling.GetRawText().Contains("fixture@"), "signalling diagnostics omit addresses and raw headers");
        bool HasReport() => System.Text.Json.JsonSerializer.SerializeToElement(second.Outgoing.Media.Diagnostics, NativeRpc.Json)
            .GetProperty("rtcp").GetProperty("reportsReceived").GetInt64() > 0;
        long reportDeadline = Environment.TickCount64 + 12000;
        while (!HasReport() && Environment.TickCount64 < reportDeadline) await Task.Delay(50);
        Check(HasReport(), "RTCP diagnostics observe reports from an actual loopback peer");
        long packets = second.Outgoing.Media.ReceivedPackets;
        first.Outgoing.Hangup(); await caller.ReleaseAsync(first.Outgoing); await receiver.ReleaseAsync(first.Incoming);
        await Until(() => second.Outgoing.Media.ReceivedPackets > packets, "remaining dialog still receives RTP after independent release");
        Check(second.Outgoing.Observation.State == "connected" && second.Incoming.Observation.State == "connected", "hangup is scoped to one dialog");
        second.Outgoing.Hangup(); await caller.ReleaseAsync(second.Outgoing); await receiver.ReleaseAsync(second.Incoming);
    }
    static async Task ChallengeLoopback() {
        var keypad = new KeypadPort { Digits = " *123#" };
        var protectedAudio = new AudioPort { IsOpen = false };
        await using var caller = new SipTransportHost(new("127.0.0.1", 0, "udp"), new(["G722", "PCMA", "PCMU"]), _ => keypad, _ => false);
        await using var receiver = new SipTransportHost(new("127.0.0.1", 0, "udp"), new(["G722", "PCMA", "PCMU"]), _ => protectedAudio, _ => true);
        var invitation = Invitation(); receiver.Incoming += value => invitation.TrySetResult(value);
        var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString()); outgoing.ConfigureFeatures(null);
        var dial = outgoing.DialAsync($"sip:fixture@127.0.0.1:{receiver.LocalEndpoint.Port}", null, null, 5);
        var incoming = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5)); incoming.ConfigureFeatures(new("*123#"));
        await incoming.AnswerAsync(); await dial;
        Check(incoming.Media.SendCodec is "PCMA" or "PCMU", "access challenge initially negotiates G711 only");
        await Until(() => incoming.Features.Authenticated, "real G711 RTP in-band code authenticates");
        Check(protectedAudio.Received == 0, "challenge PCM never opens the protected Windows audio sink");
        await incoming.UpgradeCodecAsync();
        Check(incoming.Media.SendCodec == "G722" && incoming.Observation.State == "connected", "authenticated reINVITE upgrades codec in the same dialog");
        outgoing.Hangup(); await caller.ReleaseAsync(outgoing); await receiver.ReleaseAsync(incoming);
    }
    sealed class KeypadPort : IPcmAudioPort {
        public bool IsOpen => true;
        public long Generation => 1;
        public string Digits = "";
        private long sample;
        public long Received;
        public double Energy;
        public int ReadCaptured(Span<float> output) {
            const string keys = "123A456B789C*0#D";
            double[] lows = [697, 770, 852, 941], highs = [1209, 1336, 1477, 1633];
            for (int i = 0; i < output.Length; i++, sample++) {
                long slot = sample / 19200, position = sample % 19200;
                int key = slot < Digits.Length ? keys.IndexOf(Digits[(int)slot]) : -1;
                output[i] = key < 0 || position >= 9600 ? 0 : (float)(.25 * (Math.Sin(2 * Math.PI * lows[key / 4] * sample / 48000) + Math.Sin(2 * Math.PI * highs[key % 4] * sample / 48000)));
            }
            return output.Length;
        }
        public void WriteReceived(ReadOnlySpan<float> input) { foreach (float value in input) Energy += value * value; Received += input.Length; }
    }
    static async Task ScreeningLoopback() {
        string path = Path.Combine(Path.GetTempPath(), "ivy-screening-" + Guid.NewGuid() + ".wav");
        byte[] wav;
        using (var stream = new MemoryStream()) {
            using var writer = new BinaryWriter(stream);
            writer.Write("RIFF"u8); writer.Write(36 + 48000); writer.Write("WAVEfmt "u8); writer.Write(16);
            writer.Write((short)1); writer.Write((short)1); writer.Write(24000); writer.Write(48000); writer.Write((short)2); writer.Write((short)16);
            writer.Write("data"u8); writer.Write(48000);
            for (int i = 0; i < 24000; i++) writer.Write((short)(8000 * Math.Sin(2 * Math.PI * 440 * i / 24000)));
            wav = stream.ToArray();
        }
        await File.WriteAllBytesAsync(path, wav);
        try {
            string hash = "sha256:" + Convert.ToHexStringLower(SHA256.HashData(wav));
            foreach (int repeats in new[] { 0, 1, 3 }) {
                using var screening = new ScreeningAudio(new AudioPort { IsOpen = false }, () => true);
                screening.Load(new(path, hash, 8, repeats));
                var output = new float[48000 * 9]; screening.ReadCaptured(output);
                int interval = 48000 * 8 / (repeats + 1);
                for (int play = 0; play <= repeats; play++) {
                    Check(output.AsSpan(play * interval, 48000).ToArray().Any(sample => Math.Abs(sample) > .1), "screening plays first announcement plus configured repeats");
                    Check(output.AsSpan(play * interval + 48000, interval - 48000).ToArray().All(sample => sample == 0), "screening leaves a quiet answer pause after each announcement");
                }
                Check(output.AsSpan(48000 * 8).ToArray().All(sample => sample == 0), "completed screening never repeats beyond its configured count");
            }
            var codecs = File.Exists(Path.Combine(AppContext.BaseDirectory, EvsNativeCodec.FileName))
                ? new[] { "PCMA", "G722", "EVS" } : new[] { "PCMA", "G722" };
            foreach (string codec in codecs) {
                var sink = new KeypadPort { Digits = " 1" };
                await using var first = new SipTransportHost(new("127.0.0.1", 0, "udp"), new([codec]), _ => new AudioPort { IsOpen = false }, _ => false);
                await using var second = new SipTransportHost(new("127.0.0.1", 0, "udp"), new([codec]), _ => sink, _ => true);
                var invitation = Invitation(); second.Incoming += value => invitation.TrySetResult(value);
                var call = first.PrepareOutgoing(Guid.NewGuid().ToString()); call.ConfigureFeatures(null);
                call.PrepareScreening(new(path, "sha256:" + Convert.ToHexStringLower(SHA256.HashData(wav)), 5, 2));
                var dial = call.DialAsync($"sip:fixture@127.0.0.1:{second.LocalEndpoint.Port}", null, null, 5);
                var peer = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5)); peer.ConfigureFeatures(null);
                await peer.AnswerAsync(); await dial;
                await Until(() => call.Features.Observation.Screening == "accepted", $"in-band DTMF over {codec} RTP accepts screening while Windows audio stays closed");
                Check(sink.Received > 0 && sink.Energy > 1 && call.Observation.State == "connected", $"WAV announcement over {codec} RTP stays audible and connected");
                call.Hangup(); await first.ReleaseAsync(call); await second.ReleaseAsync(peer);
            }
            if (codecs.Contains("EVS")) {
                using (var source = new RTPSession(false, false, false, IPAddress.Loopback)) {
                    source.addTrack(new MediaStreamTrack(new CodecSettings(["EVS", "G722", "PCMA", "PCMU"]).Formats().Concat([
                        new SIPSorceryMedia.Abstractions.AudioFormat(97, "telephone-event", 8000, 1, "0-15"),
                        new SIPSorceryMedia.Abstractions.AudioFormat(96, "telephone-event", 16000, 1, "0-15")]).ToList()));
                    await using var target = new SipMediaSession(new(["EVS", "G722", "PCMA", "PCMU"]), new AudioPort(), IPAddress.Loopback);
                    Check(target.SetRemoteDescription(SdpType.offer, source.CreateOffer(IPAddress.Loopback)) == SetDescriptionResultEnum.OK,
                        "EVS plus dual-rate telephone events negotiate");
                    var answer = target.CreateAnswer(IPAddress.Loopback);
                    Check(answer.Media.Where(value => value.Media == SDPMediaTypesEnum.audio)
                        .SelectMany(value => value.MediaFormats.Values)
                        .Any(value => value.Name().Equals("telephone-event", StringComparison.OrdinalIgnoreCase) &&
                            value.ID == 96 && value.Rtpmap?.Contains("/16000", StringComparison.Ordinal) == true) &&
                        target.AudioStream.NegotiatedRtpEventPayloadID == 96,
                        "EVS selects the matching 16 kHz telephone event payload");
                }
                var keypad = new KeypadPort { Digits = " *115#" };
                await using var caller = new SipTransportHost(new("127.0.0.1", 0, "udp"), new(["EVS"]), _ => keypad, _ => false);
                await using var receiver = new SipTransportHost(new("127.0.0.1", 0, "udp"), new(["EVS"]), _ => new AudioPort(), _ => true);
                var invitation = Invitation(); receiver.Incoming += value => invitation.TrySetResult(value);
                var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString()); outgoing.ConfigureFeatures(null);
                var dial = outgoing.DialAsync($"sip:fixture@127.0.0.1:{receiver.LocalEndpoint.Port}", null, null, 5);
                var incoming = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5)); incoming.ConfigureFeatures(null);
                await incoming.AnswerAsync(); await dial;
                await Until(() => incoming.Features.Observation.Command == "select_voice", "EVS RTP keypad sequence selects Voice without a task restart");
                Check(incoming.Media.SendCodec == "EVS" && incoming.Features.Observation.Model == "luna" &&
                    incoming.Features.Observation.ReasoningEffort == "max" && incoming.Features.Observation.CommandSequence == 1,
                    "EVS in-band *115# selects Luna/max");
                outgoing.Hangup(); await caller.ReleaseAsync(outgoing); await receiver.ReleaseAsync(incoming);
            }
        } finally { File.Delete(path); }
    }
}
