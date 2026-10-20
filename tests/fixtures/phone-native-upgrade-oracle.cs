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

// Only owned loopback endpoints and the original closed PCM port are used.
public static class PhoneNativeUpgradeOracle {
    static JsonElement Args(object value)=>JsonSerializer.SerializeToElement(value,NativeRpc.Json);
    static SipTransportHost Host(NativeSipOperations owner)=>(SipTransportHost)typeof(NativeSipOperations).GetField("host",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(owner);
    static async Task<string> Receive(UdpClient peer,int milliseconds=4000){using var cancel=new CancellationTokenSource(milliseconds);for(;;){var p=await peer.ReceiveAsync(cancel.Token);var raw=Encoding.UTF8.GetString(p.Buffer);if(!raw.StartsWith("SIP/2.0 100"))return raw;}}
    static async Task Wait(Func<bool> ready){var end=DateTime.UtcNow.AddSeconds(4);while(!ready()){if(DateTime.UtcNow>=end)throw new Exception("Original upgrade observation unavailable");await Task.Delay(1);}}
    static async Task<object> Invoke(NativeSipOperations owner,string id)=>await PhoneNativeDialOracle.FeedbackRow(owner,"call.codec.upgrade",new{callId=id});
    public static async Task<object> Run(NativeSipOperations owner,JsonElement args){
        string scenario=args.GetProperty("scenario").GetString();bool delayed=scenario.StartsWith("delayed");
        await owner.InvokeAsync("configure",args.GetProperty("configuration"));
        using var peer=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));UdpClient rtp=null,rtcp=null;
        for(int n=0;n<32;n++){rtp=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));int p=((IPEndPoint)rtp.Client.LocalEndPoint).Port;try{if(p==65535)throw new SocketException();rtcp=new UdpClient(new IPEndPoint(IPAddress.Loopback,p+1));break;}catch(SocketException){rtp.Dispose();rtp=null;}}
        if(rtp==null)throw new Exception("Owned upgrade RTP pair unavailable");
        using(rtp)using(rtcp){
            var target=Host(owner).LocalEndpoint;string tag="",wire="owned-native-upgrade",source=peer.Client.LocalEndPoint.ToString();
            string Sdp(string codec,int payload)=>$"v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {((IPEndPoint)rtp.Client.LocalEndPoint).Port} RTP/AVP {payload} 101\r\na=rtpmap:{payload} {codec}/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n";
            async Task Send(string method,int seq,string branch,string body){string raw=$"{method} sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {source};rport;branch={branch}\r\nFrom: <sip:caller@{source}>;tag=caller-tag\r\nTo: <sip:fixture@{target}>{(tag.Length==0?"":";tag="+tag)}\r\nCall-ID: {wire}\r\nCSeq: {seq} {method}\r\nContact: <sip:caller@{source}>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {Encoding.UTF8.GetByteCount(body)}\r\n\r\n{body}";var packet=Encoding.UTF8.GetBytes(raw);await peer.SendAsync(packet,packet.Length,target);}
            await Send("INVITE",1,"z9hG4bK-owned-upgrade",delayed?"":Sdp("PCMA",8));
            if((int)SIPResponse.ParseSIPResponse(await Receive(peer)).Status!=180)throw new Exception("Original upgrade ringing unavailable");
            await Wait(()=>Host(owner).Calls.Any(c=>c.Observation.SipCallId==wire));var call=Host(owner).Calls.Single(c=>c.Observation.SipCallId==wire);var media=call.Media;string id=call.Observation.Id;
            await owner.InvokeAsync("call.claim",Args(new{callId=id}));
            object challenge=scenario.EndsWith("trusted")?null:new{code="*123#",timeoutSeconds=30,maxFailedAttempts=3,failedAttemptWindowSeconds=60};
            await owner.InvokeAsync("call.features",Args(new{callId=id,challenge}));
            var pending=owner.InvokeAsync("call.answer",Args(new{callId=id,waiting=false}));var final=SIPResponse.ParseSIPResponse(await Receive(peer));tag=final.Header.To.ToTag;
            await Send("ACK",1,"z9hG4bK-owned-upgrade-ack",delayed?Sdp("PCMA",8):"");await pending.WaitAsync(TimeSpan.FromSeconds(6));
            if(!scenario.EndsWith("unauthenticated")&&challenge!=null){var digit=typeof(PhoneCallFeatures).GetMethod("Digit",BindingFlags.Instance|BindingFlags.NonPublic);var input=Enum.Parse(digit.GetParameters()[1].ParameterType,"SipInfo");foreach(char key in scenario.EndsWith("disabled")?"**123#":"*123#")digit.Invoke(call.Features,new object[]{key,input});}
            var before=call.Features.Observation;var rows=new List<object>();var requests=new List<object>();
            var upgrade=Invoke(owner,id);
            if(!(scenario.EndsWith("trusted")||scenario.EndsWith("disabled")||scenario.EndsWith("unauthenticated"))){
                var invite=SIPRequest.ParseSIPRequest(await Receive(peer));if(invite.Method!=SIPMethodsEnum.INVITE)throw new Exception("Original upgrade INVITE unavailable");
                requests.Add(new{method="INVITE",sequence=invite.Header.CSeq,sameCallId=invite.Header.CallId==wire,sameTags=invite.Header.From.FromTag==tag&&invite.Header.To.ToTag=="caller-tag",hasPcma=invite.Body.Contains("PCMA/8000"),hasPcmu=invite.Body.Contains("PCMU/8000"),hasSendrecv=invite.Body.Contains("sendrecv")});
                var response=SIPResponse.GetResponse(invite,(SIPResponseStatusCodesEnum)(scenario.EndsWith("refused")?488:200),null);
                response.Header.Contact=[SIPContactHeader.ParseContactHeader($"<sip:caller@{source}>").Single()];
                response.Body=scenario.EndsWith("refused")?null:scenario.EndsWith("invalid")?Sdp("PCMA",8).Replace("PCMA/8000","PCMA/8000/2"):Sdp(scenario.EndsWith("fallback")?"PCMA":"PCMU",scenario.EndsWith("fallback")?8:0);
                response.Header.ContentType=response.Body==null?null:"application/sdp";response.Header.ContentLength=Encoding.UTF8.GetByteCount(response.Body??"");
                var packet=Encoding.UTF8.GetBytes(response.ToString());await peer.SendAsync(packet,packet.Length,target);
                var ack=SIPRequest.ParseSIPRequest(await Receive(peer));requests.Add(new{method=ack.Method.ToString(),sequence=ack.Header.CSeq,sameCallId=ack.Header.CallId==wire});
            }
            rows.Add(await upgrade.WaitAsync(TimeSpan.FromSeconds(7)));
            await owner.InvokeAsync("call.hangup",Args(new{callId=id}));var bye=SIPRequest.ParseSIPRequest(await Receive(peer));if(bye.Method!=SIPMethodsEnum.BYE)throw new Exception("Original upgrade BYE unavailable");
            var ended=SIPResponse.GetResponse(bye,(SIPResponseStatusCodesEnum)200,null);var bytes=Encoding.UTF8.GetBytes(ended.ToString());await peer.SendAsync(bytes,bytes.Length,target);
            rows.Add(await Invoke(owner,id));
            object release;try{await owner.InvokeAsync("call.release",Args(new{callId=id}));release=new{released=Host(owner).FindCall(id)==null};}catch(Exception e){release=new{error=e.GetType().FullName,message=e.Message};}
            return new{rows,requests,before,sameMedia=ReferenceEquals(media,call.Media),release};
        }
    }
}
