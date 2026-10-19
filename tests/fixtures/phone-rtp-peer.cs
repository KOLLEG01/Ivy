using System;
using System.Collections.Concurrent;
using System.Linq;
using System.Net;
using System.Text.Json;
using System.Threading;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;

// Owned synthetic peer only. Uses the original managed media stack without a
// device, account, service, SIP provider, native codec library or live audio.
public static class PhoneRtpOracle {
    static void Reply(object value) { Console.WriteLine(JsonSerializer.Serialize(value)); Console.Out.Flush(); }
    public static void Run() {
        using var session = new RTPSession(false, false, false, IPAddress.Loopback);
        var formats = new CodecSettings(new[] { "PCMU" }).Formats();
        session.addTrack(new MediaStreamTrack(formats));
        var received = new ConcurrentQueue<object>();
        int reports = 0;
        session.OnRtpPacketReceived += (source, media, packet) => {
            if (media == SDPMediaTypesEnum.audio) received.Enqueue(new {
                peer = source.ToString(), sequence = packet.Header.SequenceNumber,
                timestamp = packet.Header.Timestamp, ssrc = packet.Header.SyncSource,
                payloadType = packet.Header.PayloadType, marker = packet.Header.MarkerBit,
                payload = Convert.ToHexString(packet.GetPayloadBytes()).ToLowerInvariant()
            });
        };
        session.OnReceiveReport += (source, media, report) => { if (media == SDPMediaTypesEnum.audio) Interlocked.Increment(ref reports); };
        var offer = session.CreateOffer(IPAddress.Loopback);
        int local = offer.Media.Single(track => track.Media == SDPMediaTypesEnum.audio).Port;
        Reply(new { rtp = local, rtcp = local + 1, offer = offer.ToString() });
        using var encoder = new SIPSorcery.Media.AudioEncoder(includeOpus: false);
        while (Console.ReadLine() is string line) {
            using var document = JsonDocument.Parse(line);
            var input = document.RootElement;
            switch (input.GetProperty("op").GetString()) {
                case "peer": {
                    int rtp = input.GetProperty("rtp").GetInt32(), rtcp = input.GetProperty("rtcp").GetInt32();
                    if (rtp < 1024 || rtp > 65534 || rtcp != rtp + 1) throw new ArgumentException("Owned peer pair required.");
                    string answer = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n" +
                        $"m=audio {rtp} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-16\r\na=rtcp:{rtcp}\r\na=sendrecv\r\n";
                    var result = session.SetRemoteDescription(SdpType.answer, SDP.ParseSDPDescription(answer));
                    if (result != SetDescriptionResultEnum.OK) throw new InvalidOperationException("Synthetic peer answer refused.");
                    session.Start().GetAwaiter().GetResult();
                    Reply(new { ok = true }); break;
                }
                case "send": {
                    ushort sequence = input.GetProperty("sequence").GetUInt16();
                    uint timestamp = input.GetProperty("timestamp").GetUInt32();
                    short level = input.GetProperty("positive").GetBoolean() ? (short)8000 : (short)-8000;
                    byte[] payload = encoder.EncodeAudio(Enumerable.Repeat(level, 160).ToArray(), formats[0]);
                    session.SendRtpRaw(SDPMediaTypesEnum.audio, payload, timestamp, 0, formats[0].FormatID, sequence);
                    Reply(new { sent = sequence }); break;
                }
                case "status": Reply(new { packets = received.ToArray(), reports = Volatile.Read(ref reports) }); break;
                case "stop": session.Close("fixture_complete"); Reply(new { stopped = true }); return;
                default: throw new ArgumentException("Unknown fixture operation.");
            }
        }
        session.Close("fixture_complete");
    }
}
