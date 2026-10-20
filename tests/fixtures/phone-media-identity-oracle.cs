using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;

// Original managed SDK, closed PCM and owned loopback peers. No capture, send
// pump, account access, DNS or non-loopback destinations are used.
public static class PhoneMediaIdentityOracle {
    sealed class Port : IPcmAudioPort {
        public bool IsOpen => false;
        public long Generation => 0;
        public int ReadCaptured(Span<float> output) { output.Clear(); return 0; }
        public void WriteReceived(ReadOnlySpan<float> input) {}
    }
    static (UdpClient,UdpClient) Pair() {
        for (int i=0;i<32;i++) {
            var rtp=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
            int port=((IPEndPoint)rtp.Client.LocalEndPoint).Port;
            try { if(port<65535) return (rtp,new UdpClient(new IPEndPoint(IPAddress.Loopback,port+1))); }
            catch(SocketException) {}
            rtp.Dispose();
        }
        throw new InvalidOperationException("Owned pair unavailable");
    }
    static string Text(JsonElement input,string key) => input.TryGetProperty(key,out var value)&&value.ValueKind!=JsonValueKind.Null?value.GetString():null;
    static object Description(SDP sdp) {
        string raw=sdp.ToString();
        var origin=raw.Split('\n').Single(line=>line.StartsWith("o=")).Trim().Split(' ');
        var connection=raw.Split('\n').First(line=>line.StartsWith("c=")).Trim().Split(' ');
        return new {originType=origin[4],originAddress=origin[5],connectionType=connection[1],
            connectionAddress=sdp.Connection.ConnectionAddress,sessionId=sdp.SessionId,version=sdp.AnnouncementVersion,raw};
    }
    static object RunCase(JsonElement input) {
        var pair=Pair(); using var peer=pair.Item1; using var control=pair.Item2;
        int port=((IPEndPoint)peer.Client.LocalEndPoint).Port;
        var session=new SipMediaSession(new CodecSettings(new[]{"PCMA","PCMU"}),new Port(),IPAddress.Parse(Text(input,"binding")));
        try {
            var offer=session.CreateOffer(Text(input,"offerAddress") is string a?IPAddress.Parse(a):null);
            var identity=new {sessionId=offer.SessionId,source=session.AudioLocalTrack.Ssrc,sequence=session.AudioLocalTrack.SeqNum,
                timestamp=session.AudioLocalTrack.Timestamp,cname=session.AudioRtcpSession.Cname};
            object answer=null; string applied=null;
            if(input.TryGetProperty("remote",out var remote)) {
                string global=Text(remote,"global"),media=Text(remote,"media");
                string Connection(string address)=>address==null?"":"c=IN "+(address.Contains(':')?"IP6":"IP4")+" "+address+"\r\n";
                string raw="v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\n"+Connection(global)+"t=0 0\r\nm=audio "+port+" RTP/AVP 8\r\n"+Connection(media);
                applied=session.SetRemoteDescription(SdpType.offer,SDP.ParseSDPDescription(raw)).ToString();
                answer=Description(session.CreateAnswer(null));
            }
            return new {label=Text(input,"label"),ok=true,identity,offer=Description(offer),applied,answer};
        } finally { session.DisposeAsync().AsTask().GetAwaiter().GetResult(); }
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd()); var rows=new List<object>();
        foreach(var item in input.RootElement.EnumerateArray()) {
            try { rows.Add(RunCase(item)); }
            catch(Exception failure) { rows.Add(new{label=Text(item,"label"),ok=false,error=failure.GetType().Name,message=failure.Message}); }
        }
        Console.Write(JsonSerializer.Serialize(rows));
    }
}
