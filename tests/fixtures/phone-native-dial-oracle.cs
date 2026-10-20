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
    static async Task<object> FeedbackRow(NativeSipOperations owner,string method,object value) {
        var args=Args(value);object output;
        try { output=new{json=JsonSerializer.Serialize(await owner.InvokeAsync(method,args),NativeRpc.Json)}; }
        catch(Exception e) { output=new{error=e is NativeRpcException rpc?rpc.Code:e is ArgumentException||e is JsonException?"invalid_request":"operation_outcome_unknown"}; }
        return new{method,@params=args,output};
    }
    static async Task<List<object>> Feedback(NativeSipOperations owner,string id,bool capacity) {
        var rows=new List<object>();
        rows.Add(await FeedbackRow(owner,"call.waiting.end",new{callId=id}));
        rows.Add(await FeedbackRow(owner,"call.waiting.end",new{callId=id}));
        rows.Add(await FeedbackRow(owner,"call.command.feedback",new{callId=id,commandSequence=1L,success=true}));
        var features=Host(owner).FindCall(id).Features;
        var digit=typeof(PhoneCallFeatures).GetMethod("Digit",BindingFlags.Instance|BindingFlags.NonPublic);
        var source=Enum.Parse(digit.GetParameters()[1].ParameterType,"SipInfo");
        for(int sequence=1;sequence<=3;sequence++) {
            foreach(char key in "*0#")digit.Invoke(features,new object[]{key,source});
            if(sequence==3)break;
            rows.Add(await FeedbackRow(owner,"call.command.feedback",new{callId=id,commandSequence=(long)sequence,success=sequence==2}));
            rows.Add(await FeedbackRow(owner,"call.command.feedback",new{callId=id,commandSequence=(long)sequence,success=sequence!=2}));
        }
        if(capacity) {
            for(long sequence=3;sequence<=34;sequence++) {
                if(sequence>3)foreach(char key in "*0#")digit.Invoke(features,new object[]{key,source});
                rows.Add(await FeedbackRow(owner,"call.command.feedback",new{callId=id,commandSequence=sequence,success=true}));
                if(sequence>=33)rows.Add(await FeedbackRow(owner,"call.command.feedback",new{callId=id,commandSequence=sequence,success=false}));
            }
        }
        return rows;
    }
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
            var failureCallbacks=new List<object>();var originalCall=Host(owner).FindCall(id);originalCall.Media.MediaFailed+=()=>{failureCallbacks.Add(originalCall.Observation);throw new InvalidOperationException("Synthetic external failure listener");};
            var pending=owner.InvokeAsync("call.dial",Args(new{callId=id,destination,username=telephone?null:"fixture-user",password=telephone?null:"fixture-only",ringSeconds=5,waiting=scenario=="feedback_waiting"}));
            var (invite,remote)=await Receive(peer);
            object hungup=null;
            if(scenario=="hangup_before_answer")hungup=await owner.InvokeAsync("call.hangup",Args(new{callId=id}));
            string sdp=$"v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {((IPEndPoint)rtp.Client.LocalEndPoint).Port} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
            Reply(peer,invite,remote,scenario=="refused"?486:200,scenario=="refused"?"":scenario=="media_failure_answer"?sdp.Replace("PCMA/8000","PCMA/8000/2"):sdp);
            var (ack,ackRemote)=await ReceiveDialog(peer);
            if(ack.Method!=SIPMethodsEnum.ACK)throw new Exception("Original ACK unavailable");
            object call=await pending.WaitAsync(TimeSpan.FromSeconds(5));
            if(scenario=="feedback_capacity")originalCall.Media.Close("owned_capacity_fixture");
            List<object> feedbackActions=scenario.StartsWith("feedback_")?await Feedback(owner,id,scenario=="feedback_capacity"):null;
            if(scenario=="media_failure_connected"){typeof(SipMediaSession).GetMethod("Fail",BindingFlags.Instance|BindingFlags.NonPublic).Invoke(originalCall.Media,Array.Empty<object>());if(!originalCall.Media.Failed)throw new Exception("Original media failure unavailable");}
            if(scenario=="connected") {
                var digitMethod=typeof(PhoneCallFeatures).GetMethod("Digit",BindingFlags.Instance|BindingFlags.NonPublic);
                var source=Enum.Parse(digitMethod.GetParameters()[1].ParameterType,"SipInfo");
                foreach(char digit in "*0#")digitMethod.Invoke(Host(owner).FindCall(id).Features,new object[]{digit,source});
            }
            var policy=Host(owner).FindCall(id).Features.Observation;
            object ended=null;
            if(scenario=="remote_bye") {
                string raw=$"BYE sip:fixture@{remote} SIP/2.0\r\nVia: SIP/2.0/UDP {peer.Client.LocalEndPoint};branch=z9hG4bK-owned-bye\r\nFrom: <sip:peer@{peer.Client.LocalEndPoint}>;tag={ack.Header.To.ToTag}\r\nTo: <{invite.Header.From.FromURI}>;tag={invite.Header.From.FromTag}\r\nCall-ID: {id}\r\nCSeq: 2 BYE\r\nContent-Length: 0\r\n\r\n";
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
                if(!scenario.StartsWith("media_failure"))await owner.InvokeAsync("call.hangup",Args(new{callId=id}));
                var (bye,byeRemote)=await ReceiveDialog(peer);
                if(bye.Method!=SIPMethodsEnum.BYE)throw new Exception("Original BYE unavailable");
                Reply(peer,bye,byeRemote,200,"");
                if(scenario.StartsWith("media_failure"))ended=originalCall.Observation;
            }
            if(feedbackActions!=null) {
                feedbackActions.Add(await FeedbackRow(owner,"call.waiting.end",new{callId=id}));
                feedbackActions.Add(await FeedbackRow(owner,"call.command.feedback",new{callId=id,commandSequence=3L,success=true}));
            }
            await owner.InvokeAsync("call.release",Args(new{callId=id}));
            if(feedbackActions!=null)return new{call,hungup,policy,inviteHasSdp=!string.IsNullOrEmpty(invite.Body),ackHasSdp=!string.IsNullOrEmpty(ack.Body),
                requestUri=invite.URI.ToString(),fromUri=invite.Header.From.FromURI.ToString(),sameCallId=invite.Header.CallId==id&&ack.Header.CallId==id,released=Host(owner).FindCall(id)==null,failureCallbacks,feedbackActions};
            if(scenario=="remote_bye"||scenario.StartsWith("media_failure"))return new{call,hungup,policy,inviteHasSdp=!string.IsNullOrEmpty(invite.Body),ackHasSdp=!string.IsNullOrEmpty(ack.Body),
                requestUri=invite.URI.ToString(),fromUri=invite.Header.From.FromURI.ToString(),sameCallId=invite.Header.CallId==id&&ack.Header.CallId==id,released=Host(owner).FindCall(id)==null,failureCallbacks,ended};
            return new{call,hungup,policy,inviteHasSdp=!string.IsNullOrEmpty(invite.Body),ackHasSdp=!string.IsNullOrEmpty(ack.Body),
                requestUri=invite.URI.ToString(),fromUri=invite.Header.From.FromURI.ToString(),sameCallId=invite.Header.CallId==id&&ack.Header.CallId==id,released=Host(owner).FindCall(id)==null,failureCallbacks};
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
                object result=method=="fixture.answer"?PhoneNativeAnswerOracle.Run(owner,args).GetAwaiter().GetResult():method=="fixture.incoming"?PhoneNativeIncomingOracle.Run(owner,args).GetAwaiter().GetResult():method=="fixture.outgoing"?Outgoing(owner,args).GetAwaiter().GetResult():method=="fixture.proxyDial"?ProxyDial(owner,args).GetAwaiter().GetResult():method=="fixture.requests"?Requests(owner,args).GetAwaiter().GetResult():owner.InvokeAsync(method,args).GetAwaiter().GetResult();
                rows.Add(new{json=JsonSerializer.Serialize(result,NativeRpc.Json)});
            } catch(Exception e){rows.Add(new{error=e is NativeRpcException rpc?rpc.Code:e is ArgumentException||e is JsonException?"invalid_request":"operation_outcome_unknown"});}
        }} finally {owner.DisposeAsync().AsTask().GetAwaiter().GetResult();}
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}

public static class PhoneNativeIncomingOracle {
    static SipTransportHost Host(NativeSipOperations owner)=>(SipTransportHost)typeof(NativeSipOperations).GetField("host",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(owner);
    static JsonElement Args(object value)=>JsonSerializer.SerializeToElement(value,NativeRpc.Json);
    static async Task<SIPResponse> Receive(UdpClient peer,int milliseconds=4000) {
        using var cancel=new CancellationTokenSource(milliseconds);
        for(;;) {var packet=await peer.ReceiveAsync(cancel.Token);var response=SIPResponse.ParseSIPResponse(Encoding.UTF8.GetString(packet.Buffer));if((int)response.Status!=100)return response;}
    }
    static async Task Wait(Func<bool> ready) {var until=DateTime.UtcNow.AddSeconds(4);while(!ready()){if(DateTime.UtcNow>=until)throw new Exception("Original incoming state evidence unavailable");await Task.Delay(1);}}
    static async Task<object> Invoke(NativeSipOperations owner,string method,object args) {
        try{return new{json=JsonSerializer.Serialize(await owner.InvokeAsync(method,Args(args)),NativeRpc.Json)};}
        catch(Exception e){return new{error=e is NativeRpcException rpc?rpc.Code:e is ArgumentException||e is JsonException?"invalid_request":"operation_outcome_unknown"};}
    }
    public static async Task<object> Run(NativeSipOperations owner,JsonElement args) {
        string scenario=args.GetProperty("scenario").GetString();bool claimed=scenario.Contains("claimed")&&!scenario.Contains("unclaimed");
        string wire=args.GetProperty("wireId").GetString(),asserted=args.GetProperty("asserted").GetString();
        await owner.InvokeAsync("configure",args.GetProperty("configuration"));
        string outgoing="11111111-1111-4111-8111-111111111111";
        if(scenario=="busy")await owner.InvokeAsync("call.prepare",Args(new{callId=outgoing}));
        using var peer=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));var target=Host(owner).LocalEndpoint;
        var source=(IPEndPoint)peer.Client.LocalEndPoint;
        string via=scenario=="spoofed"?$"127.0.0.2:{source.Port}":source.ToString();
        string Raw(string method,string branch)=>$"{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {via};rport;branch={branch}\r\nFrom: <sip:caller@{source}>;tag=caller-tag\r\nTo: <sip:fixture@{target}>\r\nCall-ID: {wire}\r\nCSeq: 1 {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\nP-Asserted-Identity: {asserted}\r\nContent-Length: 0\r\n\r\n";
        async Task Send(string method,string branch="z9hG4bK-owned-incoming") {var bytes=Encoding.UTF8.GetBytes(Raw(method,branch));await peer.SendAsync(bytes,bytes.Length,target);}
        await Send("INVITE");var first=await Receive(peer);int code=(int)first.Status;
        if(code!=180) {
            int count=Host(owner).Calls.Count();object existing=scenario=="busy"?Host(owner).FindCall(outgoing).Observation:null;
            if(scenario=="busy")await owner.InvokeAsync("call.release",Args(new{callId=outgoing}));
            await owner.DisposeAsync();return new{code,call=(object)null,existing,retained=count,closed=true};
        }
        await Wait(()=>Host(owner).Calls.Any(c=>c.Observation.SipCallId==wire));var call=Host(owner).Calls.Single(c=>c.Observation.SipCallId==wire);string id=call.Observation.Id;
        object admitted=call.Observation,claim=null,features=null,hungup=null,expired=null;bool duplicateIgnored=false;
        if(scenario=="duplicate") {
            await Send("INVITE");if((int)(await Receive(peer)).Status!=180)throw new Exception("Original ringing retransmission unavailable");
            await Send("INVITE","z9hG4bK-duplicate-wire");try{await Receive(peer,150);}catch(OperationCanceledException){duplicateIgnored=true;}
            if(Host(owner).Calls.Count()!=1)throw new Exception("Duplicate original call");
            claimed=true;
        }
        if(claimed) {
            claim=await Invoke(owner,"call.claim",new{callId=id});
            features=await Invoke(owner,"call.features",new{callId=id,challenge=(object)null});
        }
        var statuses=new List<int>();
        if(scenario.StartsWith("cancel")) {await Send("CANCEL");statuses.Add((int)(await Receive(peer)).Status);statuses.Add((int)(await Receive(peer)).Status);statuses.Sort();}
        else if(scenario.StartsWith("expire"))statuses.Add((int)(await Receive(peer)).Status);
        else {hungup=await Invoke(owner,"call.hangup",new{callId=id});statuses.Add((int)(await Receive(peer)).Status);}
        await Wait(()=>call.Observation.State=="local_ended");object ended=call.Observation;
        if(!claimed)await Wait(()=>Host(owner).FindCall(id)==null);
        int retained=Host(owner).Calls.Count();expired=await Invoke(owner,"call.claim",new{callId=id});
        if(claimed)await owner.InvokeAsync("call.release",Args(new{callId=id}));
        bool released=Host(owner).FindCall(id)==null;await owner.DisposeAsync();
        return new{code,call=admitted,claim,features,hungup,ended,statuses,retained,expired,released,duplicateIgnored,closed=true};
    }
}

public static class PhoneNativeAnswerOracle {
    static SipTransportHost Host(NativeSipOperations owner)=>(SipTransportHost)typeof(NativeSipOperations).GetField("host",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(owner);
    static JsonElement Args(object value)=>JsonSerializer.SerializeToElement(value,NativeRpc.Json);
    static async Task<string> Receive(UdpClient peer,int milliseconds=4000) {using var cancel=new CancellationTokenSource(milliseconds);for(;;){var p=await peer.ReceiveAsync(cancel.Token);string raw=Encoding.UTF8.GetString(p.Buffer);if(!raw.StartsWith("SIP/2.0 100"))return raw;}}
    static async Task Wait(Func<bool> ready){var until=DateTime.UtcNow.AddSeconds(4);while(!ready()){if(DateTime.UtcNow>=until)throw new Exception("Original answer observation unavailable");await Task.Delay(1);}}
    static async Task<object> Invoke(NativeSipOperations owner,string method,object args){try{return new{json=JsonSerializer.Serialize(await owner.InvokeAsync(method,Args(args)),NativeRpc.Json)};}catch(Exception e){return new{error=e is NativeRpcException rpc?rpc.Code:e is ArgumentException||e is JsonException?"invalid_request":"operation_outcome_unknown"};}}
    public static async Task<object> Run(NativeSipOperations owner,JsonElement args){
        string scenario=args.GetProperty("scenario").GetString();bool delayed=scenario.StartsWith("delayed"),waiting=scenario.EndsWith("waiting");
        await owner.InvokeAsync("configure",args.GetProperty("configuration"));
        using var peer=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));UdpClient rtp=null,rtcp=null;
        for(int n=0;n<32;n++){rtp=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));int port=((IPEndPoint)rtp.Client.LocalEndPoint).Port;try{if(port==65535)throw new SocketException();rtcp=new UdpClient(new IPEndPoint(IPAddress.Loopback,port+1));break;}catch(SocketException){rtp.Dispose();rtp=null;}}
        if(rtp==null)throw new Exception("Owned answer RTP pair unavailable");
        using(rtp)using(rtcp){
            string wire="owned-native-answer",tag="",source=peer.Client.LocalEndPoint.ToString();var target=Host(owner).LocalEndpoint;
            string sdp=$"v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {((IPEndPoint)rtp.Client.LocalEndPoint).Port} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
            string Raw(string method,int sequence,string branch,string body)=>$"{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {source};rport;branch={branch}\r\nFrom: <sip:caller@{source}>;tag=caller-tag\r\nTo: <sip:fixture@{target}>{(tag.Length==0?"":";tag="+tag)}\r\nCall-ID: {wire}\r\nCSeq: {sequence} {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {Encoding.UTF8.GetByteCount(body)}\r\n\r\n{body}";
            async Task Send(string method,int seq,string branch,string body){byte[] packet=Encoding.UTF8.GetBytes(Raw(method,seq,branch,body));await peer.SendAsync(packet,packet.Length,target);}
            await Send("INVITE",1,"z9hG4bK-owned-answer",scenario.EndsWith("invalid_offer")?"invalid-sdp":delayed?"":sdp);
            var ring=SIPResponse.ParseSIPResponse(await Receive(peer));if((int)ring.Status!=180)throw new Exception("Original answer ringing unavailable");
            await Wait(()=>Host(owner).Calls.Any(c=>c.Observation.SipCallId==wire));var call=Host(owner).Calls.Single(c=>c.Observation.SipCallId==wire);var media=call.Media;string id=call.Observation.Id;
            var failureCallbacks=new List<object>();media.MediaFailed+=()=>{failureCallbacks.Add(call.Observation);throw new InvalidOperationException("Synthetic external failure listener");};
            void FailMedia(){typeof(SipMediaSession).GetMethod("Fail",BindingFlags.Instance|BindingFlags.NonPublic).Invoke(media,Array.Empty<object>());if(!media.Failed)throw new Exception("Original media failure unavailable");}
            await owner.InvokeAsync("call.claim",Args(new{callId=id}));await owner.InvokeAsync("call.features",Args(new{callId=id,challenge=(object)null}));
            var pending=Invoke(owner,"call.answer",new{callId=id,waiting});
            var final=SIPResponse.ParseSIPResponse(await Receive(peer));int status=(int)final.Status;tag=final.Header.To.ToTag;
            object hungup=null;string endRequest=null;var controls=new List<int>();
            bool answerCompletedBeforeAck=false,mediaStartedBeforeAck=false;if(status==200){answerCompletedBeforeAck=await Task.WhenAny(pending,Task.Delay(150))==pending;mediaStartedBeforeAck=(bool)typeof(SipMediaSession).GetField("started",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(media);}
            if(status==200){
                if(scenario.EndsWith("hangup_before_ack"))hungup=await Invoke(owner,"call.hangup",new{callId=id});
                if(scenario.EndsWith("media_failure_before_ack"))FailMedia();
                if(scenario.EndsWith("slow_ack")){await Task.Delay(1250);try{for(;;)if((int)SIPResponse.ParseSIPResponse(await Receive(peer,50)).Status!=200)throw new Exception("Original ring timer was not retired by answer");}catch(OperationCanceledException){}}
                await Send("ACK",1,"z9hG4bK-owned-ack",delayed?(scenario.EndsWith("invalid_ack")?"invalid-sdp":sdp):"");
            }else{await Send("ACK",1,"z9hG4bK-owned-answer","");}
            object answered=await pending.WaitAsync(TimeSpan.FromSeconds(6));
            if(scenario.EndsWith("media_failure_after_start"))FailMedia();
            if(status==200&&scenario.EndsWith("reinvite")){
                await Send("INVITE",2,"z9hG4bK-owned-reinvite",delayed?"":sdp);var re=SIPResponse.ParseSIPResponse(await Receive(peer));controls.Add((int)re.Status);
                await Send("INVITE",3,"z9hG4bK-owned-overlap",sdp);controls.Add((int)SIPResponse.ParseSIPResponse(await Receive(peer)).Status);
                await Send("INVITE",1,"z9hG4bK-owned-stale",sdp);controls.Add((int)SIPResponse.ParseSIPResponse(await Receive(peer)).Status);
                await Send("ACK",2,(int)re.Status==200?"z9hG4bK-owned-reack":"z9hG4bK-owned-reinvite","");await Send("ACK",3,"z9hG4bK-owned-overlap-ack","");await Send("ACK",1,"z9hG4bK-owned-stale-ack","");
                await Send("OPTIONS",4,"z9hG4bK-owned-options","");controls.Add((int)SIPResponse.ParseSIPResponse(await Receive(peer)).Status);
            }
            if(status==200&&scenario.EndsWith("remote_bye")){
                for(int n=0;n<2;n++){await Send("BYE",5,"z9hG4bK-owned-bye","");controls.Add((int)SIPResponse.ParseSIPResponse(await Receive(peer)).Status);}
                await Wait(()=>(call.Observation.State=="local_ended"||call.Observation.State=="outcome_unknown"));
            }else if(status==200){
                if(hungup==null&&!scenario.EndsWith("invalid_ack")&&!scenario.Contains("media_failure"))hungup=await Invoke(owner,"call.hangup",new{callId=id});
                try{var bye=SIPRequest.ParseSIPRequest(await Receive(peer,scenario.EndsWith("invalid_ack")||scenario.EndsWith("hangup_before_ack")?150:4000));if(bye.Method!=SIPMethodsEnum.BYE)throw new Exception("Original incoming BYE unavailable");endRequest="BYE";
                var response=SIPResponse.GetResponse(bye,(SIPResponseStatusCodesEnum)200,null);byte[] packet=Encoding.UTF8.GetBytes(response.ToString());await peer.SendAsync(packet,packet.Length,target);}catch(OperationCanceledException)when(scenario.EndsWith("invalid_ack")||scenario.EndsWith("hangup_before_ack")){}
            }
            await Wait(()=>(call.Observation.State=="local_ended"||call.Observation.State=="outcome_unknown"));object ended=call.Observation;bool sameMedia=ReferenceEquals(media,call.Media);
            await owner.InvokeAsync("call.release",Args(new{callId=id}));bool released=Host(owner).FindCall(id)==null;await owner.DisposeAsync();
            return new{status,answered,hungup,ended,controls,endRequest,noByeObservationMs=scenario.EndsWith("invalid_ack")||scenario.EndsWith("hangup_before_ack")?150:0,finalHasSdp=!string.IsNullOrEmpty(final.Body),ackHasSdp=status==200&&delayed,sameMedia,released,closed=true,failureCallbacks,answerCompletedBeforeAck,mediaStartedBeforeAck};
        }
    }
}
