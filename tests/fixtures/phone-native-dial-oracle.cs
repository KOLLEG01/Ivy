using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Text;
using System.Text.Json;
using System.Threading;
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
    static async Task<(SIPRequest,IPEndPoint)> ReceiveDialog(UdpClient peer) {
        for(int n=0;n<4;n++){var received=await Receive(peer);if(received.Item1.Method!=SIPMethodsEnum.CANCEL)return received;Reply(peer,received.Item1,received.Item2,200,"");}
        throw new Exception("Original dialog evidence unavailable after CANCEL");
    }
    static async Task<object> Outgoing(NativeSipOperations owner,JsonElement args,UdpClient suppliedPeer=null) {
        string id=args.GetProperty("callId").GetString(),scenario=args.GetProperty("scenario").GetString();
        using var peer=suppliedPeer??new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
        UdpClient rtp=null,rtcp=null;
        for(int n=0;n<32;n++) {rtp=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));int p=((IPEndPoint)rtp.Client.LocalEndPoint).Port;
            try{if(p==65535)throw new SocketException();rtcp=new UdpClient(new IPEndPoint(IPAddress.Loopback,p+1));break;}catch(SocketException){rtp.Dispose();rtp=null;}}
        if(rtp==null)throw new Exception("Owned RTP pair unavailable");
        using(rtp)using(rtcp) {
            bool telephone=args.TryGetProperty("destination",out var target);
            string destination=telephone?target.GetString():$"sip:peer@{peer.Client.LocalEndPoint}";
            await owner.InvokeAsync("call.features",Args(new{callId=id,challenge=(object)null}));
            var pending=owner.InvokeAsync("call.dial",Args(new{callId=id,destination,username=telephone?null:"fixture-user",password=telephone?null:"fixture-only",ringSeconds=5,waiting=false}));
            var (invite,remote)=await Receive(peer);
            object hungup=null;
            if(scenario=="hangup_before_answer")hungup=await owner.InvokeAsync("call.hangup",Args(new{callId=id}));
            string sdp=$"v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {((IPEndPoint)rtp.Client.LocalEndPoint).Port} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
            Reply(peer,invite,remote,scenario=="refused"?486:200,scenario=="refused"?"":sdp);
            var (ack,ackRemote)=await ReceiveDialog(peer);
            if(ack.Method!=SIPMethodsEnum.ACK)throw new Exception("Original ACK unavailable");
            object call=await pending.WaitAsync(TimeSpan.FromSeconds(5));
            if(scenario=="connected") {
                var digitMethod=typeof(PhoneCallFeatures).GetMethod("Digit",BindingFlags.Instance|BindingFlags.NonPublic);
                var source=Enum.Parse(digitMethod.GetParameters()[1].ParameterType,"SipInfo");
                foreach(char digit in "*0#")digitMethod.Invoke(Host(owner).FindCall(id).Features,new object[]{digit,source});
            }
            var policy=Host(owner).FindCall(id).Features.Observation;
            object ended=null;
            if(scenario=="remote_bye") {
                string raw=$"BYE sip:fixture@{remote} SIP/2.0\r\nVia: SIP/2.0/UDP {peer.Client.LocalEndPoint};branch=z9hG4bK-owned-bye\r\nFrom: <sip:peer@{peer.Client.LocalEndPoint}>;tag=owned-peer\r\nTo: <{invite.Header.From.FromURI}>;tag={invite.Header.From.FromTag}\r\nCall-ID: {id}\r\nCSeq: 2 BYE\r\nContent-Length: 0\r\n\r\n";
                byte[] packet=Encoding.UTF8.GetBytes(raw);
                for(int n=0;n<2;n++) {
                    await peer.SendAsync(packet,packet.Length,remote);
                    var reply=await peer.ReceiveAsync().WaitAsync(TimeSpan.FromSeconds(4));
                    if((int)SIPResponse.ParseSIPResponse(Encoding.UTF8.GetString(reply.Buffer)).Status!=200)throw new Exception("Original BYE response unavailable");
                }
                var until=DateTime.UtcNow.AddSeconds(4);
                while(Host(owner).FindCall(id).Observation.State=="connected"&&DateTime.UtcNow<until)await Task.Delay(1);
                ended=Host(owner).FindCall(id).Observation;
            } else if(scenario!="refused") {
                await owner.InvokeAsync("call.hangup",Args(new{callId=id}));
                var (bye,byeRemote)=await ReceiveDialog(peer);
                if(bye.Method!=SIPMethodsEnum.BYE)throw new Exception("Original BYE unavailable");
                Reply(peer,bye,byeRemote,200,"");
            }
            await owner.InvokeAsync("call.release",Args(new{callId=id}));
            if(scenario=="remote_bye")return new{call,hungup,ended,policy,inviteHasSdp=!string.IsNullOrEmpty(invite.Body),ackHasSdp=!string.IsNullOrEmpty(ack.Body),
                requestUri=invite.URI.ToString(),fromUri=invite.Header.From.FromURI.ToString(),sameCallId=invite.Header.CallId==id&&ack.Header.CallId==id,released=Host(owner).FindCall(id)==null};
            return new{call,hungup,policy,inviteHasSdp=!string.IsNullOrEmpty(invite.Body),ackHasSdp=!string.IsNullOrEmpty(ack.Body),
                requestUri=invite.URI.ToString(),fromUri=invite.Header.From.FromURI.ToString(),sameCallId=invite.Header.CallId==id&&ack.Header.CallId==id,released=Host(owner).FindCall(id)==null};
        }
    }
    static async Task<object> ProxyDial(NativeSipOperations owner,JsonElement args) {
        using var peer=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
        using var config=JsonDocument.Parse(args.GetProperty("configuration").GetRawText().Replace("$PROXY",$"udp:{peer.Client.LocalEndPoint}"));
        await owner.InvokeAsync("configure",config.RootElement);
        await owner.InvokeAsync("call.prepare",Args(new{callId=args.GetProperty("callId").GetString()}));
        return await Outgoing(owner,args,peer);
    }
    static async Task<object> Requests(NativeSipOperations owner,JsonElement args) {
        string id=args.GetProperty("callId").GetString();
        using var peer=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
        var target=Host(owner).LocalEndpoint;
        var rows=new List<object>();int sequence=100;
        foreach(var input in args.GetProperty("requests").EnumerateArray()) {
            string method=input.GetProperty("method").GetString();bool tagged=input.GetProperty("tagged").GetBoolean();
            string raw=$"{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {peer.Client.LocalEndPoint};branch=z9hG4bK-owned-request-{sequence}\r\nFrom: <sip:peer@{peer.Client.LocalEndPoint}>;tag=foreign\r\nTo: <sip:fixture@{target}>{(tagged?";tag=foreign":"")}\r\nCall-ID: {id}\r\nCSeq: {sequence++} {method}\r\nContent-Type: application/octet-stream\r\nContent-Length: 19\r\n\r\nprivate-fixture-raw";
            byte[] bytes=Encoding.UTF8.GetBytes(raw);await peer.SendAsync(bytes,bytes.Length,target);
            int? status=null;
            using var cancel=new CancellationTokenSource(input.GetProperty("response").ValueKind==JsonValueKind.Null?150:4000);
            try {
                var reply=await peer.ReceiveAsync(cancel.Token);
                status=(int)SIPResponse.ParseSIPResponse(Encoding.UTF8.GetString(reply.Buffer)).Status;
            } catch(OperationCanceledException) { }
            rows.Add(new{method,tagged,status});
        }
        await owner.InvokeAsync("call.release",Args(new{callId=id}));
        await owner.DisposeAsync();
        return new{requests=rows,closed=true};
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());var owner=new NativeSipOperations(id=>new CallAudioPort(id,new AudioPermit()));
        var rows=new List<object>();
        try {foreach(var step in input.RootElement.EnumerateArray()) {
            try {string method=step.GetProperty("method").GetString();var args=step.GetProperty("params");
                object result=method=="fixture.outgoing"?Outgoing(owner,args).GetAwaiter().GetResult():method=="fixture.proxyDial"?ProxyDial(owner,args).GetAwaiter().GetResult():method=="fixture.requests"?Requests(owner,args).GetAwaiter().GetResult():owner.InvokeAsync(method,args).GetAwaiter().GetResult();
                rows.Add(new{json=JsonSerializer.Serialize(result,NativeRpc.Json)});
            } catch(Exception e){rows.Add(new{error=e is NativeRpcException rpc?rpc.Code:e is ArgumentException||e is JsonException?"invalid_request":"operation_outcome_unknown"});}
        }} finally {owner.DisposeAsync().AsTask().GetAwaiter().GetResult();}
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}
