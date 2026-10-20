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

// Real original requests on owned loopback with closed, unprepared PCM only.
public static class PhoneNativeInfoOracle {
    static JsonElement Args(object value)=>JsonSerializer.SerializeToElement(value,NativeRpc.Json);
    static SipTransportHost Host(NativeSipOperations owner)=>(SipTransportHost)typeof(NativeSipOperations).GetField("host",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(owner);
    static async Task<string> Receive(UdpClient peer,int milliseconds=4000){using var cancel=new CancellationTokenSource(milliseconds);for(;;){var p=await peer.ReceiveAsync(cancel.Token);var raw=Encoding.UTF8.GetString(p.Buffer);if(!raw.StartsWith("SIP/2.0 100"))return raw;}}
    static async Task Wait(Func<bool> ready){var end=DateTime.UtcNow.AddSeconds(4);while(!ready()){if(DateTime.UtcNow>=end)throw new Exception("Original INFO observation unavailable");await Task.Delay(1);}}
    public static async Task<object> Run(NativeSipOperations owner,JsonElement args){
        bool incoming=args.GetProperty("incoming").GetBoolean();string fixedId=args.GetProperty("callId").GetString();
        await owner.InvokeAsync("configure",args.GetProperty("configuration"));
        using var peer=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));UdpClient rtp=null,rtcp=null;
        for(int n=0;n<32;n++){rtp=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));int p=((IPEndPoint)rtp.Client.LocalEndPoint).Port;try{if(p==65535)throw new SocketException();rtcp=new UdpClient(new IPEndPoint(IPAddress.Loopback,p+1));break;}catch(SocketException){rtp.Dispose();rtp=null;}}
        if(rtp==null)throw new Exception("Owned INFO RTP pair unavailable");
        using(rtp)using(rtcp){
            var target=Host(owner).LocalEndpoint;string source=peer.Client.LocalEndPoint.ToString(),wire=incoming?"owned-native-info":fixedId,local="",remote="caller-tag";int branch=0;
            string sdp=$"v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {((IPEndPoint)rtp.Client.LocalEndPoint).Port} RTP/AVP 8 101\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
            string Raw(string method,int seq,string body,string contentType,string selectedWire,string selectedLocal,string selectedRemote,string via)=>$"{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {source};rport;branch={via}\r\nFrom: <sip:caller@{source}>{(selectedRemote==null?"":";tag="+selectedRemote)}\r\nTo: <sip:fixture@{target}>{(selectedLocal==null?"":";tag="+selectedLocal)}\r\nCall-ID: {selectedWire}\r\nCSeq: {seq} {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\n{(contentType==null?"":"Content-Type: "+contentType+"\r\n")}Content-Length: {Encoding.UTF8.GetByteCount(body)}\r\n\r\n{body}";
            async Task SendRaw(string raw){var packet=Encoding.UTF8.GetBytes(raw);await peer.SendAsync(packet,packet.Length,target);}
            var rows=new List<object>();SipCall call=null;
            async Task Cases(string phase,string selectedWire,string selectedLocal,string selectedRemote,SipCall observed){foreach(var step in args.GetProperty("cases").EnumerateArray().Where(c=>c.GetProperty("phase").GetString()==phase)){
                int seq=step.GetProperty("sequence").GetInt32();string label=step.GetProperty("label").GetString(),mutation=step.TryGetProperty("mutation",out var m)?m.GetString():"";
                string ct=step.GetProperty("contentType").ValueKind==JsonValueKind.Null?null:step.GetProperty("contentType").GetString(),body=step.GetProperty("body").GetString();
                string selectedVia=step.TryGetProperty("branch",out var b)?b.GetString():"z9hG4bK-owned-info-"+(++branch);
                await SendRaw(Raw("INFO",seq,body,ct,mutation=="wire"?"foreign-wire":selectedWire,mutation=="local"?"foreign-local":mutation=="missingLocal"?null:selectedLocal,mutation=="remote"?"foreign-remote":mutation=="missingRemote"?null:selectedRemote,selectedVia));
                int? status=null,responseSequence=null;using var cancel=new CancellationTokenSource(step.GetProperty("reply").GetBoolean()?4000:150);
                try{for(;;){var packet=await peer.ReceiveAsync(cancel.Token);string raw=Encoding.UTF8.GetString(packet.Buffer);if(!raw.StartsWith("SIP/2.0"))throw new Exception("Unexpected original INFO dialog request");var response=SIPResponse.ParseSIPResponse(raw);if(response.Header.CSeqMethod!=SIPMethodsEnum.INFO||response.Header.Vias.TopViaHeader.Branch!=selectedVia)continue;status=(int)response.Status;responseSequence=response.Header.CSeq;break;}}
                catch(OperationCanceledException){}
                rows.Add(new{label,status,responseSequence,features=observed.Features.Observation});
            }}
            async Task<(SipCall,string,string)> Outgoing(string id,bool first){
                await owner.InvokeAsync("call.prepare",Args(new{callId=id}));var current=Host(owner).FindCall(id);await owner.InvokeAsync("call.features",Args(new{callId=id,challenge=(object)null}));
                var pending=owner.InvokeAsync("call.dial",Args(new{callId=id,destination=$"sip:peer@{source}",username="fixture-user",password="fixture-only",ringSeconds=5,waiting=false}));
                var invite=SIPRequest.ParseSIPRequest(await Receive(peer));string tag=invite.Header.From.FromTag;
                if(first)await Cases("beforeAnswer",id,tag,"owned-peer",current);
                var response=SIPResponse.GetResponse(invite,(SIPResponseStatusCodesEnum)200,null);response.Header.To.ToTag="owned-peer";response.Header.Contact=[SIPContactHeader.ParseContactHeader($"<sip:peer@{source}>").Single()];response.Body=sdp;response.Header.ContentType="application/sdp";response.Header.ContentLength=Encoding.UTF8.GetByteCount(sdp);await SendRaw(response.ToString());
                if(SIPRequest.ParseSIPRequest(await Receive(peer)).Method!=SIPMethodsEnum.ACK)throw new Exception("Original INFO ACK unavailable");await pending.WaitAsync(TimeSpan.FromSeconds(6));return(current,tag,"owned-peer");
            }
            if(incoming){
                bool delayed=args.GetProperty("delayed").GetBoolean();await SendRaw(Raw("INVITE",1,delayed?"":sdp,"application/sdp",wire,null,remote,"z9hG4bK-owned-info-invite"));
                var ring=SIPResponse.ParseSIPResponse(await Receive(peer));if((int)ring.Status!=180)throw new Exception("Original INFO ringing unavailable");local=ring.Header.To.ToTag;
                await Wait(()=>Host(owner).Calls.Any(c=>c.Observation.SipCallId==wire));call=Host(owner).Calls.Single(c=>c.Observation.SipCallId==wire);string id=call.Observation.Id;
                await owner.InvokeAsync("call.claim",Args(new{callId=id}));await owner.InvokeAsync("call.features",Args(new{callId=id,challenge=args.TryGetProperty("challenge",out var challenge)?(object)challenge:null}));
                await Cases("beforeAnswer",wire,local,remote,call);
                var pending=owner.InvokeAsync("call.answer",Args(new{callId=id,waiting=false}));var final=SIPResponse.ParseSIPResponse(await Receive(peer));local=final.Header.To.ToTag;
                if(!delayed)await pending.WaitAsync(TimeSpan.FromSeconds(6));await Cases("beforeAck",wire,local,remote,call);
                await SendRaw(Raw("ACK",1,delayed?sdp:"",delayed?"application/sdp":null,wire,local,remote,"z9hG4bK-owned-info-ack"));await pending.WaitAsync(TimeSpan.FromSeconds(6));
            } else {var opened=await Outgoing(fixedId,true);call=opened.Item1;local=opened.Item2;remote=opened.Item3;}
            var originalMedia=call.Media;await Cases("connected",wire,local,remote,call);
            async Task End(SipCall ended){await owner.InvokeAsync("call.hangup",Args(new{callId=ended.Observation.Id}));var bye=SIPRequest.ParseSIPRequest(await Receive(peer));if(bye.Method!=SIPMethodsEnum.BYE)throw new Exception("Original INFO BYE unavailable");await SendRaw(SIPResponse.GetResponse(bye,(SIPResponseStatusCodesEnum)200,null).ToString());}
            await End(call);await Cases("ended",wire,local,remote,call);string originalId=call.Observation.Id;await owner.InvokeAsync("call.release",Args(new{callId=originalId}));
            await Cases("released",wire,local,remote,call);
            var replacement=await Outgoing(originalId,false);await Cases("replacementOld",wire,local,remote,replacement.Item1);await Cases("replacementNew",originalId,replacement.Item2,replacement.Item3,replacement.Item1);
            await End(replacement.Item1);await owner.InvokeAsync("call.release",Args(new{callId=originalId}));
            return new{rows,released=Host(owner).FindCall(originalId)==null,originalMediaStable=ReferenceEquals(originalMedia,call.Media),physicalAudioOpens=0};
        }
    }
}
