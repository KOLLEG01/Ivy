using System;
using System.Linq;
using System.Net;
using System.Reflection;
using System.Text.Json;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;

// Original managed parser/session, synthetic closed PCM port, loopback sockets.
// Never starts RTP, captures audio, creates a service or contacts a SIP account.
public static class PhoneSdpOracle {
    sealed class ClosedPort : IPcmAudioPort {
        public bool IsOpen => false;
        public long Generation => 0;
        public int ReadCaptured(Span<float> output) { output.Clear(); return output.Length; }
        public void WriteReceived(ReadOnlySpan<float> input) { }
    }
    static readonly Type Call = typeof(SipMediaSession).Assembly.GetType("Ivy.PhoneBridge.LateOfferSipCall", true);
    static object Invoke(string method, params object[] args) => Call.GetMethod(method, BindingFlags.Static | BindingFlags.NonPublic).Invoke(null, args);
    static object Shape(SDP value) => value == null ? null : new {
        username = value.Username, sessionId = value.SessionId, version = value.AnnouncementVersion,
        network = value.NetworkType, addressType = value.AddressType, address = value.AddressOrHost,
        name = value.SessionName, timing = value.Timing, connection = value.Connection?.ToString().Trim(),
        direction = value.SessionMediaStreamStatus?.ToString(), extra = value.ExtraSessionAttributes,
        media = value.Media.Select((track, index) => new {
            kind = track.Media.ToString(), port = track.Port, portCount = track.PortCount,
            transport = track.Transport, connection = track.Connection?.ToString().Trim(),
            direction = track.MediaStreamStatus.ToString(), effectiveDirection = value.GetMediaStreamStatus(track.Media, 0).ToString(),
            mid = track.MediaID, extra = track.ExtraMediaAttributes,
            formats = track.MediaFormats?.Values.Select(format => new {
                payload = format.ID, name = format.Name(), rtpmap = format.Rtpmap, fmtp = format.Fmtp
            }).ToArray()
        }).ToArray()
    };
    static void Reply(object value) { Console.WriteLine(JsonSerializer.Serialize(value)); Console.Out.Flush(); }
    public static void Run() {
        while (Console.ReadLine() is string line) {
            try {
                using var json = JsonDocument.Parse(line);
                var input = json.RootElement;
                switch (input.GetProperty("op").GetString()) {
                    case "parse": {
                        string body = input.GetProperty("sdp").GetString();
                        var parsed = SDP.ParseSDPDescription(body);
                        string rejected = (string)Invoke("RejectedAnswer", body);
                        Reply(new { ok = true, shape = Shape(parsed), canonical = parsed?.ToString(), rejected });
                        break;
                    }
                    case "valid": Reply(new { valid = Invoke("ValidSdp", input.GetProperty("mime").GetString(), input.GetProperty("sdp").GetString()) }); break;
                    case "session": {
                        string[] preferences = input.GetProperty("preferences").EnumerateArray().Select(x => x.GetString()).ToArray();
                        if (preferences.Any(x => x.Equals("EVS", StringComparison.OrdinalIgnoreCase))) throw new ArgumentException("This fixture has no native EVS runtime.");
                        var session = new SipMediaSession(new CodecSettings(preferences), new ClosedPort(), IPAddress.Loopback);
                        try {
                            var offer = session.CreateOffer(IPAddress.Loopback);
                            var steps = input.GetProperty("steps").EnumerateArray().Select(step => {
                                if (step.TryGetProperty("challenge", out var challenge)) typeof(SipMediaSession).GetMethod("ChallengeCodecs", BindingFlags.Instance | BindingFlags.NonPublic).Invoke(session, new object[] { challenge.GetBoolean() });
                                var type = step.GetProperty("type").GetString() == "offer" ? SdpType.offer : SdpType.answer;
                                var result = session.SetRemoteDescription(type, SDP.ParseSDPDescription(step.GetProperty("sdp").GetString()));
                                var answer = result == SetDescriptionResultEnum.OK && type == SdpType.offer ? session.CreateAnswer(IPAddress.Loopback) : null;
                                return new { result = result.ToString(), failed = session.Failed, sendCodec = session.SendCodec,
                                    sendPayload = session.SendPayload, answer = Shape(answer), diagnostics = session.Diagnostics,
                                    localFormats = session.AudioLocalTrack.Capabilities.Select(f => new { payload = f.ID, name = f.Name(), rtpmap = f.Rtpmap, fmtp = f.Fmtp }).ToArray() };
                            }).ToArray();
                            Reply(new { ok = true, offer = Shape(offer), steps });
                        } finally { session.DisposeAsync().AsTask().GetAwaiter().GetResult(); }
                        break;
                    }
                    default: throw new ArgumentException("Unknown synthetic operation.");
                }
            } catch (Exception error) {
                while (error is TargetInvocationException && error.InnerException != null) error = error.InnerException;
                Reply(new { ok = false, error = error.GetType().Name });
            }
        }
    }
}
