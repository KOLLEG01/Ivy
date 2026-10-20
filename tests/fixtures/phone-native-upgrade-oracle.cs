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
        bool downgrade=scenario.EndsWith("_repeat_downgrade"),reverse=scenario.EndsWith("_concurrent_reverse");
        bool concurrentLate=scenario.EndsWith("_concurrent_late"),mixed=scenario.Contains("_concurrent_")&&!reverse&&!concurrentLate,refusedFirst=scenario.EndsWith("_refused_first"),refusedSecond=scenario.EndsWith("_refused_second");
        bool late=scenario.EndsWith("_late"),repeated=scenario.EndsWith("_repeated")||downgrade,retransmitted=scenario.EndsWith("_retransmitted"),pendingHangup=scenario.EndsWith("_pending_hangup");
        bool concurrent=scenario.EndsWith("_concurrent")||reverse||mixed||concurrentLate,pendingRelease=scenario.EndsWith("_pending_release"),hangupSuccess=scenario.EndsWith("_hangup_success");
        bool lifecycle=late||repeated||retransmitted||pendingHangup||concurrent||pendingRelease||hangupSuccess;
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
            var before=call.Features.Observation;var rows=new List<object>();var requests=new List<object>();var finals=new List<object>();
            void FinalRow(SIPResponse response){var parsed=SIPResponse.ParseSIPResponse(response.ToString());finals.Add(new{status=(int)parsed.Status,sequence=parsed.Header.CSeq,contentType=parsed.Header.ContentType,body=(parsed.Body??"").Replace("m=audio "+((IPEndPoint)rtp.Client.LocalEndPoint).Port,"m=audio owned-rtp")});}
            async Task<SIPRequest> Next(SIPMethodsEnum expected){for(;;){var next=SIPRequest.ParseSIPRequest(await Receive(peer));if(next.Method==expected)return next;if(next.Method!=SIPMethodsEnum.INVITE||expected==SIPMethodsEnum.INVITE)throw new Exception("Unexpected original upgrade control request");}}
            async Task<SIPRequest> Additional(int sequence){for(;;){var next=await Next(SIPMethodsEnum.INVITE);if(next.Header.CSeq!=sequence)return next;}}
            void InviteRow(SIPRequest invite){requests.Add(new{method="INVITE",sequence=invite.Header.CSeq,sameCallId=invite.Header.CallId==wire,sameTags=invite.Header.From.FromTag==tag&&invite.Header.To.ToTag=="caller-tag",hasPcma=invite.Body.Contains("PCMA/8000"),hasPcmu=invite.Body.Contains("PCMU/8000"),hasSendrecv=invite.Body.Contains("sendrecv")});}
            void AckRow(SIPRequest ack){requests.Add(new{method=ack.Method.ToString(),sequence=ack.Header.CSeq,sameCallId=ack.Header.CallId==wire});}
            int byeSequence=0;bool endedBeforeFinal=false,firstSaved=false,secondSaved=false,releaseBeforeFinal=false,acked=true;Task releasePending=null;Task<object> concurrentUpgrade=null;SIPRequest concurrentInvite=null;
            async Task CompleteConcurrent(){
                var accepted=SIPResponse.GetResponse(concurrentInvite,(SIPResponseStatusCodesEnum)(refusedSecond?488:200),null);accepted.Header.Contact=[SIPContactHeader.ParseContactHeader($"<sip:caller@{source}>").Single()];
                bool pcma=!mixed||scenario.EndsWith("missing_type")||scenario.EndsWith("other_type");
                accepted.Body=refusedSecond?null:Sdp(pcma?"PCMA":"PCMU",pcma?8:0);accepted.Header.ContentType=refusedSecond?null:"application/sdp";accepted.Header.ContentLength=Encoding.UTF8.GetByteCount(accepted.Body??"");
                if(mixed)FinalRow(accepted);
                var bytes=Encoding.UTF8.GetBytes(accepted.ToString());await peer.SendAsync(bytes,bytes.Length,target);AckRow(await Next(SIPMethodsEnum.ACK));
            }
            async Task End(){await owner.InvokeAsync("call.hangup",Args(new{callId=id}));var bye=await Next(SIPMethodsEnum.BYE);byeSequence=bye.Header.CSeq;var ended=SIPResponse.GetResponse(bye,(SIPResponseStatusCodesEnum)200,null);var bytes=Encoding.UTF8.GetBytes(ended.ToString());await peer.SendAsync(bytes,bytes.Length,target);}
            var upgrade=Invoke(owner,id);
            if(!(scenario.EndsWith("trusted")||scenario.EndsWith("disabled")||scenario.EndsWith("unauthenticated"))){
                var invite=await Next(SIPMethodsEnum.INVITE);InviteRow(invite);
                if(concurrent){concurrentUpgrade=Invoke(owner,id);concurrentInvite=await Additional(invite.Header.CSeq);if(concurrentInvite!=null)InviteRow(concurrentInvite);}
                if(reverse&&concurrentInvite!=null){var first=invite;invite=concurrentInvite;concurrentInvite=first;}
                if(late){rows.Add(await upgrade.WaitAsync(TimeSpan.FromSeconds(7)));firstSaved=true;}
                if(concurrentLate){rows.Add(await concurrentUpgrade.WaitAsync(TimeSpan.FromSeconds(7)));secondSaved=true;}
                if(scenario.Contains("_shutdown_")){
                    bool preReleased=scenario.EndsWith("released"),replacementDistinct=false;
                    if(preReleased){await End();await owner.InvokeAsync("call.release",Args(new{callId=id}));await owner.InvokeAsync("call.prepare",Args(new{callId=id}));var replacement=Host(owner).FindCall(id);replacementDistinct=replacement!=null&&!ReferenceEquals(replacement,call);}
                    var shutdown=owner.DisposeAsync().AsTask();string byeError=null;bool byeObserved=preReleased;
                    if(!preReleased){try{var bye=await Next(SIPMethodsEnum.BYE);byeObserved=true;byeSequence=bye.Header.CSeq;var ended=SIPResponse.GetResponse(bye,(SIPResponseStatusCodesEnum)200,null);var bytes=Encoding.UTF8.GetBytes(ended.ToString());await peer.SendAsync(bytes,bytes.Length,target);}catch(Exception e){byeError=e.GetType().FullName+": "+e.Message;}}
                    await Task.WhenAny(shutdown,Task.Delay(750));bool shutdownBeforeFinal=shutdown.IsCompletedSuccessfully;
                    bool bindingReleased=false;string rebindError=null;if(shutdownBeforeFinal){try{using var rebound=new UdpClient(target);bindingReleased=true;}catch(Exception e){rebindError=e.GetType().FullName+": "+e.Message;}}
                    var accepted=SIPResponse.GetResponse(invite,(SIPResponseStatusCodesEnum)200,null);accepted.Header.Contact=[SIPContactHeader.ParseContactHeader($"<sip:caller@{source}>").Single()];accepted.Header.ContentType="application/sdp";accepted.Body=Sdp("PCMU",0);accepted.Header.ContentLength=Encoding.UTF8.GetByteCount(accepted.Body);FinalRow(accepted);
                    string finalSendError=null,ackError=null;var bytesFinal=Encoding.UTF8.GetBytes(accepted.ToString());try{await peer.SendAsync(bytesFinal,bytesFinal.Length,target);}catch(Exception e){finalSendError=e.GetType().FullName+": "+e.Message;}
                    try{AckRow(await Next(SIPMethodsEnum.ACK));}catch(Exception e){acked=false;ackError=e.GetType().FullName+": "+e.Message;}
                    rows.Add(await upgrade.WaitAsync(TimeSpan.FromSeconds(7)));
                    object closed;try{await shutdown.WaitAsync(TimeSpan.FromSeconds(7));closed=new{closed=true};}catch(Exception e){closed=new{error=e.GetType().FullName,message=e.Message};}
                    var shared=new{rows,before,sameMedia=ReferenceEquals(media,call.Media),preReleased,replacementDistinct,sendCodecAfterShutdown=(string)media.GetType().GetProperty("SendCodec",BindingFlags.Instance|BindingFlags.Public|BindingFlags.NonPublic).GetValue(media),byeSequence};
                    return new{shared,requests,finals,shutdownBeforeFinal,bindingReleased,rebindError,byeObserved,byeError,finalSendError,ackObserved=acked,ackError,shutdown=closed};
                }
                if(pendingHangup||pendingRelease||hangupSuccess){await End();endedBeforeFinal=true;}
                if(pendingRelease){releasePending=owner.InvokeAsync("call.release",Args(new{callId=id}));await Task.WhenAny(releasePending,Task.Delay(1000));releaseBeforeFinal=releasePending.IsCompletedSuccessfully;}
                var response=SIPResponse.GetResponse(invite,(SIPResponseStatusCodesEnum)(scenario.EndsWith("refused")||pendingHangup||refusedFirst?488:200),null);
                response.Header.Contact=[SIPContactHeader.ParseContactHeader($"<sip:caller@{source}>").Single()];
                response.Body=scenario.EndsWith("refused")||pendingHangup||refusedFirst?null:scenario.EndsWith("malformed")?"invalid-sdp":scenario.EndsWith("invalid")?Sdp("PCMA",8).Replace("PCMA/8000","PCMA/8000/2"):Sdp(scenario.EndsWith("fallback")?"PCMA":"PCMU",scenario.EndsWith("fallback")?8:0);
                response.Header.ContentType=response.Body==null||scenario.EndsWith("missing_type")?null:scenario.EndsWith("other_type")?"application/octet-stream":"application/sdp";response.Header.ContentLength=Encoding.UTF8.GetByteCount(response.Body??"");
                if(mixed)FinalRow(response);
                var packet=Encoding.UTF8.GetBytes(response.ToString());await peer.SendAsync(packet,packet.Length,target);
                try{AckRow(await Next(SIPMethodsEnum.ACK));}catch(OperationCanceledException)when(pendingRelease||hangupSuccess){acked=false;}
                if(mixed)await CompleteConcurrent();
                if(retransmitted){await peer.SendAsync(packet,packet.Length,target);AckRow(await Next(SIPMethodsEnum.ACK));}
                if(!firstSaved){rows.Add(await upgrade.WaitAsync(TimeSpan.FromSeconds(7)));firstSaved=true;}
                if(concurrent){
                    if(!secondSaved)rows.Add(await concurrentUpgrade.WaitAsync(TimeSpan.FromSeconds(7)));
                    if(concurrentInvite!=null&&!mixed)await CompleteConcurrent();
                }
                if(repeated){
                    var again=Invoke(owner,id);var next=await Next(SIPMethodsEnum.INVITE);InviteRow(next);
                    var accepted=SIPResponse.GetResponse(next,(SIPResponseStatusCodesEnum)200,null);accepted.Header.Contact=[SIPContactHeader.ParseContactHeader($"<sip:caller@{source}>").Single()];accepted.Body=downgrade?Sdp("PCMA",8):Sdp("PCMU",0);accepted.Header.ContentType="application/sdp";accepted.Header.ContentLength=Encoding.UTF8.GetByteCount(accepted.Body);
                    var bytes=Encoding.UTF8.GetBytes(accepted.ToString());await peer.SendAsync(bytes,bytes.Length,target);AckRow(await Next(SIPMethodsEnum.ACK));rows.Add(await again.WaitAsync(TimeSpan.FromSeconds(7)));
                }
            }
            if(!firstSaved)rows.Add(await upgrade.WaitAsync(TimeSpan.FromSeconds(7)));
            string SendCodec()=>(string)media.GetType().GetProperty("SendCodec",BindingFlags.Instance|BindingFlags.Public|BindingFlags.NonPublic).GetValue(media);
            if(late)await Wait(()=>SendCodec()=="PCMU");string sendCodecBeforeEnd=SendCodec();
            if(!endedBeforeFinal)await End();
            rows.Add(await Invoke(owner,id));
            object release;try{if(releasePending!=null)await releasePending.WaitAsync(TimeSpan.FromSeconds(7));else await owner.InvokeAsync("call.release",Args(new{callId=id}));release=new{released=Host(owner).FindCall(id)==null};}catch(Exception e){release=new{error=e.GetType().FullName,message=e.Message};}
            if(mixed)return new{rows,requests,before,sameMedia=ReferenceEquals(media,call.Media),release,dialog=new{firstBeforeFinal=false,hangupBeforeFinal=false,sendCodecBeforeEnd,byeSequence},finals};
            if(concurrentLate)return new{rows,requests,before,sameMedia=ReferenceEquals(media,call.Media),release,dialog=new{firstBeforeFinal=true,secondBeforeFinal=true,hangupBeforeFinal=false,sendCodecBeforeEnd,byeSequence}};
            if(pendingRelease)return new{rows,requests,before,sameMedia=ReferenceEquals(media,call.Media),release,dialog=new{firstBeforeFinal=false,hangupBeforeFinal=endedBeforeFinal,sendCodecBeforeEnd,byeSequence},releaseBeforeFinal,ackAfterFinal=acked};
            if(hangupSuccess)return new{rows,requests,before,sameMedia=ReferenceEquals(media,call.Media),release,dialog=new{firstBeforeFinal=false,hangupBeforeFinal=endedBeforeFinal,sendCodecBeforeEnd,byeSequence},ackAfterFinal=acked};
            if(lifecycle)return new{rows,requests,before,sameMedia=ReferenceEquals(media,call.Media),release,dialog=new{firstBeforeFinal=late,hangupBeforeFinal=endedBeforeFinal,sendCodecBeforeEnd,byeSequence}};
            return new{rows,requests,before,sameMedia=ReferenceEquals(media,call.Media),release};
        }
    }
}
