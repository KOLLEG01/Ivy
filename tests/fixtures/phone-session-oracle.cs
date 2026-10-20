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

// Sealed original session methods. Only owned loopback datagrams and closed PCM;
// never starts the private original pump or opens devices/accounts.
public static class PhoneSessionOracle {
    sealed class Port : IPcmAudioPort {
        public bool IsOpen => false;
        public long Generation => 0;
        public int ReadCaptured(Span<float> output){output.Clear();return output.Length;}
        public void WriteReceived(ReadOnlySpan<float> input){}
    }
    static (UdpClient,UdpClient) Pair(){for(int i=0;i<32;i++){
        var rtp=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));int p=((IPEndPoint)rtp.Client.LocalEndPoint).Port;
        try{if(p<65535)return(rtp,new UdpClient(new IPEndPoint(IPAddress.Loopback,p+1)));}catch(SocketException){}
        rtp.Dispose();}throw new InvalidOperationException("Owned pair unavailable");}
    static void Set(object target,string name,object value)=>target.GetType().GetProperty(name).SetValue(target,value);
    static object RunCase(JsonElement input){
        var pair=Pair();using var peer=pair.Item1;using var control=pair.Item2;
        int port=((IPEndPoint)peer.Client.LocalEndPoint).Port;
        var session=new SipMediaSession(new CodecSettings(new[]{"PCMU","PCMA"}),new Port(),IPAddress.Loopback);
        try{
            var initial=session.CreateOffer(IPAddress.Loopback);int local=initial.Media[0].Port;
            session.AudioLocalTrack.Ssrc=17;session.AudioRtcpSession.Ssrc=17;
            Set(session.AudioLocalTrack,"SeqNum",(ushort)100);Set(session.AudioLocalTrack,"Timestamp",(uint)200);
            string Endpoint(IPEndPoint ep)=>ep==null?null:ep.Address+":"+(ep.Port==port?"peer":ep.Port==port+1?"control":ep.Port.ToString());
            object State()=>new{rtp=Endpoint(session.AudioDestinationEndPoint),rtcp=Endpoint(session.AudioControlDestinationEndPoint),
                local=session.AudioLocalTrack.StreamStatus.ToString(),remote=session.AudioStream.RemoteTrack?.StreamStatus.ToString(),
                controlClosed=session.AudioRtcpSession.IsClosed,controlStarted=session.AudioRtcpSession.StartedAt!=DateTime.MinValue,
                sequence=session.AudioLocalTrack.SeqNum,timestamp=session.AudioLocalTrack.Timestamp,
                packets=session.AudioRtcpSession.PacketsSentCount,sendCodec=session.SendCodec,failed=session.Failed};
            var rows=new List<object>();
            foreach(var step in input.GetProperty("steps").EnumerateArray()){
                string op=step.GetProperty("op").GetString(),result=null,error=null;object description=null;
                try{switch(op){
                    case "set":result=session.SetRemoteDescription(step.TryGetProperty("answer",out var answer)&&answer.GetBoolean()?SdpType.answer:SdpType.offer,
                        SDP.ParseSDPDescription(step.GetProperty("sdp").GetString().Replace("$PORT",port.ToString()))).ToString();break;
                    case "status":session.SetMediaStreamStatus(SDPMediaTypesEnum.audio,Enum.Parse<MediaStreamStatusEnum>(step.GetProperty("value").GetString()));break;
                    case "offer":case "answer":{
                        var sdp=op=="offer"?session.CreateOffer(IPAddress.Loopback):session.CreateAnswer(IPAddress.Loopback);
                        description=sdp==null?null:new{version=sdp.AnnouncementVersion,port=sdp.Media[0].Port==local?"local":sdp.Media[0].Port.ToString(),direction=sdp.Media[0].MediaStreamStatus.ToString()};break;}
                    case "send":session.AudioStream.SendAudioFrame(160,0,new byte[]{1,2,3,4});break;
                    default:throw new ArgumentException();
                }}catch(Exception failure){error=failure.GetType().Name;}
                rows.Add(new{op,result,error,description,state=State()});
            }
            return new{ok=true,trace=rows};
        }finally{
            var destination=session.AudioControlDestinationEndPoint;
            if(destination!=null&&(destination.Address!=IPAddress.Loopback||destination.Port!=port+1))Set(session.AudioStream,"ControlDestinationEndPoint",null);
            session.DisposeAsync().AsTask().GetAwaiter().GetResult();
        }
    }
    public static void Run(){using var input=JsonDocument.Parse(Console.In.ReadToEnd());var rows=new List<object>();
        foreach(var item in input.RootElement.EnumerateArray())try{rows.Add(RunCase(item));}catch(Exception error){rows.Add(new{ok=false,error=error.GetType().Name});}
        Console.Write(JsonSerializer.Serialize(rows));
    }
}
