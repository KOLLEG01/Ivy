using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using Ivy.PhoneBridge;
using SIPSorcery.SIP;

// Real original calls on owned loopback, with closed/unprepared call audio.
public static class PhoneNativeDialOracle {
    static SipTransportHost Host(NativeSipOperations owner)=>(SipTransportHost)typeof(NativeSipOperations).GetField("host",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(owner);
    static JsonElement Args(object value)=>JsonSerializer.SerializeToElement(value,NativeRpc.Json);
    static async Task<(SIPRequest,IPEndPoint)> Receive(UdpClient peer) {
        var packet=await peer.ReceiveAsync().WaitAsync(TimeSpan.FromSeconds(4));
        return (SIPRequest.ParseSIPRequest(Encoding.UTF8.GetString(packet.Buffer)),packet.RemoteEndPoint);
    }
    static void Reply(UdpClient peer,SIPRequest request,IPEndPoint remote,int code,string body) {
        var response=SIPResponse.GetResponse(request,(SIPResponseStatusCodesEnum)code,null);
        response.Header.To.ToTag="owned-peer";
        response.Header.Contact=[SIPContactHeader.ParseContactHeader($"<sip:peer@{peer.Client.LocalEndPoint}>").Single()];
        response.Body=body;response.Header.ContentType=string.IsNullOrEmpty(body)?null:"application/sdp";
        response.Header.ContentLength=Encoding.UTF8.GetByteCount(body??"");
        byte[] bytes=Encoding.UTF8.GetBytes(response.ToString());peer.Send(bytes,bytes.Length,remote);
    }
    static async Task<object> Outgoing(NativeSipOperations owner,JsonElement args) {
        string id=args.GetProperty("callId").GetString(),scenario=args.GetProperty("scenario").GetString();
        using var peer=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
        UdpClient rtp=null,rtcp=null;
        for(int n=0;n<32;n++) {rtp=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));int p=((IPEndPoint)rtp.Client.LocalEndPoint).Port;
            try{if(p==65535)throw new SocketException();rtcp=new UdpClient(new IPEndPoint(IPAddress.Loopback,p+1));break;}catch(SocketException){rtp.Dispose();rtp=null;}}
        if(rtp==null)throw new Exception("Owned RTP pair unavailable");
        using(rtp)using(rtcp) {
            string destination=$"sip:peer@{peer.Client.LocalEndPoint}";
            var pending=owner.InvokeAsync("call.dial",Args(new{callId=id,destination,username="fixture-user",password="fixture-only",ringSeconds=5,waiting=false}));
            var (invite,remote)=await Receive(peer);
            object hungup=null;
            if(scenario=="hangup_before_answer")hungup=await owner.InvokeAsync("call.hangup",Args(new{callId=id}));
            string sdp=$"v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {((IPEndPoint)rtp.Client.LocalEndPoint).Port} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
            Reply(peer,invite,remote,scenario=="refused"?486:200,scenario=="refused"?"":sdp);
            var (ack,ackRemote)=await Receive(peer);
            if(ack.Method!=SIPMethodsEnum.ACK)throw new Exception("Original ACK unavailable");
            object call=await pending.WaitAsync(TimeSpan.FromSeconds(5));
            if(scenario!="refused") {
                await owner.InvokeAsync("call.hangup",Args(new{callId=id}));
                var (bye,byeRemote)=await Receive(peer);
                if(bye.Method!=SIPMethodsEnum.BYE)throw new Exception("Original BYE unavailable");
                Reply(peer,bye,byeRemote,200,"");
            }
            await owner.InvokeAsync("call.release",Args(new{callId=id}));
            return new{call,hungup,inviteHasSdp=!string.IsNullOrEmpty(invite.Body),ackHasSdp=!string.IsNullOrEmpty(ack.Body),
                requestUriUser=invite.URI.User,fromUri=invite.Header.From.FromURI.ToString(),sameCallId=invite.Header.CallId==id&&ack.Header.CallId==id,released=Host(owner).FindCall(id)==null};
        }
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());var owner=new NativeSipOperations(id=>new CallAudioPort(id,new AudioPermit()));
        var rows=new List<object>();
        try {foreach(var step in input.RootElement.EnumerateArray()) {
            try {string method=step.GetProperty("method").GetString();var args=step.GetProperty("params");
                object result=method=="fixture.outgoing"?Outgoing(owner,args).GetAwaiter().GetResult():owner.InvokeAsync(method,args).GetAwaiter().GetResult();
                rows.Add(new{json=JsonSerializer.Serialize(result,NativeRpc.Json)});
            } catch(Exception e){rows.Add(new{error=e is NativeRpcException rpc?rpc.Code:e is ArgumentException||e is JsonException?"invalid_request":"operation_outcome_unknown"});}
        }} finally {owner.DisposeAsync().AsTask().GetAwaiter().GetResult();}
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}
