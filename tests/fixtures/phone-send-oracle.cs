using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Text.Json;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;

// Actual sealed MediaCodec/AudioStream methods with owned synthetic loopback
// peers only. The race probe pauses between the original pump's two public
// calls; it does not claim to pause the original private realtime thread.
public static class PhoneSendOracle {
    sealed class Port : IPcmAudioPort {
        public bool IsOpen {get;set;}=true;
        public long Generation {get;set;}=1;
        public bool DuringRead;
        public int Reads;
        public int ReadCaptured(Span<float> output) {
            Reads++;output.Fill(0.5f);
            if(DuringRead){Generation++;IsOpen=false;DuringRead=false;}
            return output.Length;
        }
        public void WriteReceived(ReadOnlySpan<float> input) { }
    }
    static void Set(object target,string name,object value)=>target.GetType().GetProperty(name).SetValue(target,value);
    static (UdpClient,UdpClient) Pair() {
        for(int i=0;i<32;i++){
            var rtp=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
            int port=((IPEndPoint)rtp.Client.LocalEndPoint).Port;
            try{if(port<65535)return(rtp,new UdpClient(new IPEndPoint(IPAddress.Loopback,port+1)));}
            catch(SocketException){}
            rtp.Dispose();
        }
        throw new InvalidOperationException("Owned pair unavailable");
    }
    static object RunCase(JsonElement input) {
        var pair=Pair();using var peer=pair.Item1;using var control=pair.Item2;
        peer.Client.ReceiveTimeout=1000;
        using var session=new RTPSession(false,false,false,IPAddress.Loopback);
        var formats=new CodecSettings(new[]{"PCMU"}).Formats();
        session.addTrack(new MediaStreamTrack(formats));
        var offer=session.CreateOffer(IPAddress.Loopback);
        int local=offer.Media.Single(m=>m.Media==SDPMediaTypesEnum.audio).Port;
        int remote=((IPEndPoint)peer.Client.LocalEndPoint).Port;
        string answer="v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n"+
            $"m=audio {remote} RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n";
        if(session.SetRemoteDescription(SdpType.answer,SDP.ParseSDPDescription(answer))!=SetDescriptionResultEnum.OK)
            throw new InvalidOperationException("Owned peer refused");
        var track=session.AudioLocalTrack;track.Ssrc=17;
        Set(track,"SeqNum",input.TryGetProperty("sequence",out var seq)?seq.GetUInt16():(ushort)65535);
        Set(track,"Timestamp",input.TryGetProperty("timestamp",out var ts)?ts.GetUInt32():uint.MaxValue-80);
        object Packet(byte[] bytes,IPEndPoint source)=>new{hex=Convert.ToHexString(bytes).ToLowerInvariant(),sourcePort=source.Port};
        string op=input.GetProperty("op").GetString();
        if(op=="race") {
            var port=new Port();using var codec=new MediaCodec(formats[0],port);
            for(int i=0;i<4;i++)codec.Encode();
            string mode=input.GetProperty("mode").GetString();
            if(mode=="closed")port.IsOpen=false;
            if(mode=="during")port.DuringRead=true;
            var encoded=codec.Encode();long encodedGeneration=port.Generation;bool encodedOpen=port.IsOpen;
            if(mode=="after"){port.Generation++;port.IsOpen=false;}
            session.AudioStream.SendAudioFrame(codec.RtpDuration,formats[0].FormatID,encoded);
            IPEndPoint source=null;var bytes=peer.Receive(ref source);
            return new{ok=true,mode,encodedGeneration,encodedOpen,sendGeneration=port.Generation,sendOpen=port.IsOpen,
                encodedContainsSignal=encoded.Any(b=>b!=255),received=Packet(bytes,source),localPort=local,
                sequence=track.SeqNum,timestamp=track.Timestamp,rtcpPackets=session.AudioRtcpSession.PacketsSentCount};
        }
        if(op!="frames")throw new ArgumentException("Unknown probe");
        var trace=new List<object>();
        foreach(var action in input.GetProperty("actions").EnumerateArray()) {
            byte[] payload=Convert.FromHexString(action.GetProperty("hex").GetString());
            if(payload.Length>8192)throw new ArgumentException("Bounded payload required");
            uint duration=action.GetProperty("duration").GetUInt32();int payloadId=action.GetProperty("payload").GetInt32();
            bool destination=!action.TryGetProperty("destination",out var dest)||dest.GetBoolean();
            bool receiving=action.TryGetProperty("receiveOnly",out var ro)&&ro.GetBoolean();
            Set(session.AudioStream,"DestinationEndPoint",destination?new IPEndPoint(IPAddress.Loopback,remote):null);
            Set(track,"StreamStatus",receiving?MediaStreamStatusEnum.RecvOnly:MediaStreamStatusEnum.SendRecv);
            string error=null;
            try{session.AudioStream.SendAudioFrame(duration,payloadId,payload);}
            catch(Exception failure){error=failure.GetType().Name;}
            var packets=new List<object>();
            if(destination&&!receiving&&error==null) {
                int count=(payload.Length+1199)/1200;
                for(int i=0;i<count;i++){IPEndPoint source=null;var bytes=peer.Receive(ref source);packets.Add(Packet(bytes,source));}
            }
            trace.Add(new{sequence=track.SeqNum,timestamp=track.Timestamp,rtcpPackets=session.AudioRtcpSession.PacketsSentCount,
                rtcpOctets=session.AudioRtcpSession.OctetsSentCount,error,packets});
        }
        return new{ok=true,localPort=local,trace};
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());var output=new List<object>();
        foreach(var item in input.RootElement.EnumerateArray()){
            try{output.Add(RunCase(item));}catch(Exception error){output.Add(new{ok=false,error=error.GetType().Name,detail=error.Message});}
        }
        Console.Write(JsonSerializer.Serialize(output));
    }
}
