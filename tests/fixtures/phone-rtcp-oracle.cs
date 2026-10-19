using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using System.Threading;
using System.Reflection;
using System.Diagnostics;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;

// Public APIs of the sealed original assembly; synthetic byte arrays only.
public static class PhoneRtcpOracle {
    static (UdpClient, UdpClient) Pair() {
        for (int i = 0; i < 32; i++) {
            var rtp = new UdpClient(new IPEndPoint(IPAddress.Loopback, 0));
            int port = ((IPEndPoint)rtp.Client.LocalEndPoint).Port;
            try { if (port < 65535) return (rtp, new UdpClient(new IPEndPoint(IPAddress.Loopback, port + 1))); }
            catch (SocketException) { }
            rtp.Dispose();
        }
        throw new InvalidOperationException("Owned loopback pair unavailable");
    }
    sealed class ClosedAudio : IPcmAudioPort {
        public bool IsOpen => false;
        public long Generation => 1;
        public int ReadCaptured(Span<float> output) { output.Clear(); return 0; }
        public void WriteReceived(ReadOnlySpan<float> input) { }
    }
    static object Block(ReceptionReportSample value) => new {
        source=value.SSRC, fraction=value.FractionLost, lost=value.PacketsLost,
        highest=value.ExtendedHighestSequenceNumber, jitter=value.Jitter,
        last=value.LastSenderReportTimestamp, delay=value.DelaySinceLastSenderReport
    };
    static List<ReceptionReportSample> Blocks(JsonElement input) {
        var list = new List<ReceptionReportSample>();
        foreach (var value in input.GetProperty("blocks").EnumerateArray()) list.Add(new(
            value[0].GetUInt32(), value[1].GetByte(), value[2].GetInt32(),
            value[3].GetUInt32(), value[4].GetUInt32(), value[5].GetUInt32(), value[6].GetUInt32()));
        return list;
    }
    static object RunCase(JsonElement input) {
        switch (input.GetProperty("op").GetString()) {
            case "statistics": {
                var report = new ReceptionReport(input.GetProperty("source").GetUInt32());
                var record = typeof(ReceptionReport).GetMethod("RtpPacketReceived", BindingFlags.NonPublic | BindingFlags.Instance);
                var receiveSender = typeof(ReceptionReport).GetMethod("RtcpSenderReportReceived", BindingFlags.NonPublic | BindingFlags.Instance);
                if (record == null || receiveSender == null) throw new InvalidOperationException("Pinned original statistics entry unavailable");
                var trace = new List<object>();
                foreach (var action in input.GetProperty("actions").EnumerateArray()) {
                    switch(action[0].GetString()) {
                        case "rx": record.Invoke(report, new object[] { action[1].GetUInt16(), action[2].GetUInt32(), action[3].GetUInt32() }); break;
                        case "sr": {
                            receiveSender.Invoke(report, new object[] { ulong.Parse(action[1].GetString(), NumberStyles.HexNumber) });
                            var timestamp = typeof(ReceptionReport).GetField("m_receivedLSRTimestamp", BindingFlags.NonPublic | BindingFlags.Instance).GetValue(report);
                            var receivedAt = (DateTime)timestamp.GetType().GetField("ReceivedAt").GetValue(timestamp);
                            trace.Add(new { kind = "sr", receivedNtp32 = RTCPSession.DateTimeToNtpTimestamp32(receivedAt) }); break;
                        }
                        case "sample": {
                            var sample = report.GetSample(action[1].GetUInt32());
                            trace.Add(new { kind = "sample", block = Block(sample), hex = Convert.ToHexString(sample.GetBytes()).ToLowerInvariant() }); break;
                        }
                        default: throw new ArgumentException("Unknown statistics action");
                    }
                }
                return new { ok = true, trace };
            }
            case "timer": {
                var session = new RTCPSession(SDPMediaTypesEnum.audio, 9);
                int defaultTimeout = session.NoActivityTimeoutMilliseconds;
                session.NoActivityTimeoutMilliseconds = 0;
                using var completed = new CountdownEvent(2);
                var trace = new List<object>(); var sync = new object();
                int timeouts = 0;
                var clock = Stopwatch.StartNew();
                session.OnTimeout += _ => Interlocked.Increment(ref timeouts);
                session.OnReportReadyToSend += (_, report) => {
                    lock(sync) {
                        trace.Add(new { ms = clock.Elapsed.TotalMilliseconds, sender = report.SenderReport != null,
                            bye = report.Bye != null, reason = report.Bye?.Reason, timeouts = Volatile.Read(ref timeouts) });
                        if(report.Bye == null && completed.CurrentCount > 0) completed.Signal();
                    }
                };
                try {
                    var data = new byte[172]; data[0] = 128; data[3] = 1; data[11] = 9;
                    session.RecordRtpPacketSend(new RTPPacket(data));
                    bool firstRead = session.GetRtcpReport().SenderReport != null;
                    bool secondRead = session.GetRtcpReport().SenderReport != null;
                    if (!completed.Wait(30000)) throw new InvalidOperationException("Original report callbacks unavailable");
                    bool afterTimer = session.GetRtcpReport().SenderReport != null;
                    bool timedOut = session.IsTimedOut;
                    session.RecordRtpPacketReceived(new RTPPacket(data));
                    bool activityRecovered = !session.IsTimedOut;
                    var choose = typeof(RTCPSession).GetMethod("GetNextRtcpInterval", BindingFlags.Instance | BindingFlags.NonPublic);
                    int[] intervals = Enumerable.Range(0,256).Select(_ => (int)choose.Invoke(session,new object[] {5000})).ToArray();
                    session.Close("phone_media_closed");
                    lock(sync) return new { ok=true, defaultTimeout, firstRead, secondRead, afterTimer, timedOut, activityRecovered, intervals, trace=trace.ToArray() };
                } finally { session.Close("fixture_complete"); }
            }
            case "receive": {
                // Actual original application callback/projection, not a
                // reimplementation. Only owned loopback sockets and zero PCM;
                // no device, account, route, real call or service is used.
                var pair = Pair();
                using var rtp = pair.Item1;
                using var control = pair.Item2;
                using var rawControl = new AutoResetEvent(false);
                using var rawRtp = new AutoResetEvent(false);
                using var seeded = new ManualResetEventSlim(false);
                var session = new SipMediaSession(new CodecSettings(new[] { "PCMU" }), new ClosedAudio(), IPAddress.Loopback);
                int events = 0;
                var snapshots = new List<object>();
                try {
                    session.AudioLocalTrack.Ssrc = 17;
                    session.AudioRtcpSession.Ssrc = 17;
                    session.OnReceiveReport += (_, media, _) => { if (media == SDPMediaTypesEnum.audio) Interlocked.Increment(ref events); };
                    session.OnRtpPacketReceived += (_, media, packet) => { if (media == SDPMediaTypesEnum.audio && packet.Header.SyncSource == 9) seeded.Set(); };
                    var offer = session.CreateOffer(IPAddress.Loopback);
                    int local = offer.Media.Single(value => value.Media == SDPMediaTypesEnum.audio).Port;
                    int peer = ((IPEndPoint)rtp.Client.LocalEndPoint).Port;
                    int peerControl = ((IPEndPoint)control.Client.LocalEndPoint).Port;
                    string answer = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n" +
                        $"m=audio {peer} RTP/AVP 0 101 110\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=rtpmap:110 telephone-event/48000\r\na=rtcp:{peerControl}\r\na=sendrecv\r\n";
                    string WithSources(JsonElement value) => answer + (value.TryGetProperty("sources", out var sources)
                        ? string.Concat(sources.EnumerateArray().Select(source => $"a=ssrc:{source.GetUInt32()} cname:fixture\r\n")) : "");
                    if (session.SetRemoteDescription(SdpType.answer, SDP.ParseSDPDescription(WithSources(input))) != SetDescriptionResultEnum.OK)
                        throw new InvalidOperationException("Synthetic answer refused");
                    session.Start().GetAwaiter().GetResult();
                    session.AudioStream.GetRTPChannel().OnControlDataReceived += (_, _, _) => rawControl.Set();
                    session.AudioStream.GetRTPChannel().OnRTPDataReceived += (_, _, _) => rawRtp.Set();
                    if (!input.TryGetProperty("seed", out var seed) || seed.GetBoolean()) {
                        byte[] first = new byte[172]; first[0] = 128; first[3] = 1; first[11] = 9;
                        rtp.Send(first, new IPEndPoint(IPAddress.Loopback, local));
                        if (!seeded.Wait(1000)) throw new InvalidOperationException("Original media did not receive the owned RTP seed");
                        if (!rawRtp.WaitOne(1000)) throw new InvalidOperationException("Original seed callback did not complete");
                    }
                    foreach (var value in input.GetProperty("datagrams").EnumerateArray()) {
                        string kind = value.ValueKind == JsonValueKind.String ? "rtcp" : value.GetProperty("kind").GetString();
                        string hex = kind == "renegotiate" ? "" : value.ValueKind == JsonValueKind.String ? value.GetString() : value.GetProperty("hex").GetString();
                        if (kind == "renegotiate") {
                            if (session.SetRemoteDescription(SdpType.answer, SDP.ParseSDPDescription(WithSources(value))) != SetDescriptionResultEnum.OK)
                                throw new InvalidOperationException("Synthetic replacement answer refused");
                        } else {
                            byte[] bytes = Convert.FromHexString(hex);
                            if (bytes.Length > 8192) throw new ArgumentException("Bounded fixture datagram required");
                            if (kind == "rtp") {
                                rtp.Send(bytes, new IPEndPoint(IPAddress.Loopback, local));
                                if (!rawRtp.WaitOne(1000)) throw new InvalidOperationException("Original RTP callback did not complete");
                            } else if (kind == "rtcp") {
                                control.Send(bytes, new IPEndPoint(IPAddress.Loopback, local + 1));
                                if (!rawControl.WaitOne(1000)) throw new InvalidOperationException("Original control channel did not receive the owned datagram");
                            } else throw new ArgumentException("Unknown receive step");
                        }
                        var diagnostics = JsonSerializer.SerializeToElement(session.Diagnostics);
                        snapshots.Add(new { kind, hex, events = Volatile.Read(ref events), rtcp = diagnostics.GetProperty("rtcp").Clone(), failed = session.Failed,
                            learnedSource = session.AudioRemoteTrack.Ssrc, declaredSources = session.AudioRemoteTrack.SdpSsrc.Keys.ToArray() });
                    }
                } finally { session.DisposeAsync().AsTask().GetAwaiter().GetResult(); }
                return new { ok = true, started = true, seeded = seeded.IsSet, snapshots };
            }
            case "parse": {
                byte[] bytes=Convert.FromHexString(input.GetProperty("hex").GetString());
                bool ok=RTCPCompoundPacket.TryParse(bytes.AsSpan(),out var value,out int consumed);
                if (!ok) return new {ok=false,consumed};
                var sr=value.SenderReport; var rr=value.ReceiverReport;
                return new {ok=true,consumed,sender=sr==null?null:(object)new {
                    source=sr.SSRC,ntp=sr.NtpTimestamp.ToString("x16"),rtp=sr.RtpTimestamp,
                    packets=sr.PacketCount,octets=sr.OctetCount,blocks=sr.ReceptionReports?.Select(Block).ToArray()??[]
                },receiver=rr==null?null:(object)new {source=rr.SSRC,blocks=rr.ReceptionReports?.Select(Block).ToArray()??[]},
                    cname=value.SDesReport?.CNAME,bye=value.Bye?.Reason};
            }
            case "write": {
                uint source=input.GetProperty("source").GetUInt32();
                string kind=input.GetProperty("kind").GetString();
                byte[] bytes=kind switch {
                    "sr"=>new RTCPSenderReport(source,ulong.Parse(input.GetProperty("ntp").GetString(),NumberStyles.HexNumber),
                        input.GetProperty("rtp").GetUInt32(),input.GetProperty("packets").GetUInt32(),input.GetProperty("octets").GetUInt32(),Blocks(input)).GetBytes(),
                    "rr"=>new RTCPReceiverReport(source,Blocks(input)).GetBytes(),
                    "sdes"=>new RTCPSDesReport(source,input.GetProperty("text").GetString()).GetBytes(),
                    "bye"=>new RTCPBye(source,input.GetProperty("text").GetString()).GetBytes(),
                    _=>throw new ArgumentException("Unknown synthetic report kind")
                };
                return new {ok=true,hex=Convert.ToHexString(bytes).ToLowerInvariant()};
            }
            case "state": {
                var session=new RTCPSession(SDPMediaTypesEnum.audio,9);
                session.PayloadSampleRateHz=input.GetProperty("rate").GetInt32();
                var snapshots=new List<object>();
                try {
                    foreach(var action in input.GetProperty("actions").EnumerateArray()) {
                        var data=new byte[12+action[4].GetInt32()];data[0]=128;
                        ushort sequence=action[1].GetUInt16();uint stamp=action[2].GetUInt32(),source=action[3].GetUInt32();
                        data[2]=(byte)(sequence>>8);data[3]=(byte)sequence;
                        for(int i=0;i<4;i++){data[4+i]=(byte)(stamp>>(24-i*8));data[8+i]=(byte)(source>>(24-i*8));}
                        var packet=new RTPPacket(data);
                        if(action[0].GetString()=="tx")session.RecordRtpPacketSend(packet);else session.RecordRtpPacketReceived(packet);
                        var report=session.GetRtcpReport();
                        snapshots.Add(new {sent=session.PacketsSentCount,received=session.PacketsReceivedCount,
                            sentOctets=session.OctetsSentCount,receivedOctets=session.OctetsReceivedCount,
                            sender=report.SenderReport!=null,
                            blocks=(report.SenderReport?.ReceptionReports??report.ReceiverReport?.ReceptionReports)?.Select(Block).ToArray()??[]});
                    }
                } finally {session.Close("fixture_complete");}
                return new {ok=true,snapshots};
            }
            default: throw new ArgumentException("Unknown fixture operation");
        }
    }
    public static void Run() {
        using var document=JsonDocument.Parse(Console.In.ReadToEnd());
        var output=new List<object>();
        foreach(var input in document.RootElement.EnumerateArray()) {
            try { output.Add(RunCase(input)); }
            catch(Exception error) { output.Add(new {ok=false,error=error.GetType().Name}); }
        }
        Console.Write(JsonSerializer.Serialize(output));
    }
}
