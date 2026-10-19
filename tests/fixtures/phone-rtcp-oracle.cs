using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text.Json;
using SIPSorcery.Net;

// Public APIs of the sealed original assembly; synthetic byte arrays only.
public static class PhoneRtcpOracle {
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
