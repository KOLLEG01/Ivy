using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Linq.Expressions;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Text.Json;
using System.Threading;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP.App;

// Sealed original diagnostics over closed call audio and owned loopback only.
// No sender pump, physical device, registration or account is started.
public static class PhoneMediaDiagnosticsOracle {
    const string Call="11111111-1111-4111-8111-111111111111";
    const BindingFlags Flags=BindingFlags.Instance|BindingFlags.NonPublic;
    static void Set(object target,string name,object value)=>target.GetType().GetField(name,Flags).SetValue(target,value);
    static long Raw(SipMediaSession session)=>(long)typeof(SipMediaSession).GetField("rawRtpPackets",Flags).GetValue(session);
    static (UdpClient,UdpClient) Pair(){for(int i=0;i<32;i++){var a=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));int p=((IPEndPoint)a.Client.LocalEndPoint).Port;try{if(p<65535)return(a,new UdpClient(new IPEndPoint(IPAddress.Loopback,p+1)));}catch(SocketException){}a.Dispose();}throw new Exception("Owned pair unavailable");}
    static void Digit(char digit,object source){}
    static object RunCase(JsonElement input) {
        var pair=Pair();using var peer=pair.Item1;using var control=pair.Item2;using var foreign=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
        int port=((IPEndPoint)peer.Client.LocalEndPoint).Port;
        var audio=new CallAudioPort(Call,new AudioPermit());
        var session=new SipMediaSession(new CodecSettings(input.GetProperty("preferences").Deserialize<string[]>()),audio,IPAddress.Loopback,Call);
        var method=typeof(SipMediaSession).GetMethod("EnableDtmf",Flags);
        var type=method.GetParameters()[0].ParameterType;var arg=type.GetGenericArguments();
        var first=Expression.Parameter(arg[0]);var second=Expression.Parameter(arg[1]);
        var callback=Expression.Lambda(type,Expression.Call(typeof(PhoneMediaDiagnosticsOracle).GetMethod("Digit",BindingFlags.Static|BindingFlags.NonPublic),first,Expression.Convert(second,typeof(object))),first,second).Compile();
        method.Invoke(session,new object[]{callback,(Func<bool>)(()=>false)});
        var local=session.AudioStream.GetRTPChannel().RTPLocalEndPoint;
        var rows=new List<object>();
        try { foreach(var step in input.GetProperty("steps").EnumerateArray()) {
            string result=null;var op=step.GetProperty("op").GetString();
            switch(op) {
                case "snapshot":break;
                case "offer":session.CreateOffer(IPAddress.Loopback);break;
                case "answer":session.CreateAnswer(IPAddress.Loopback);break;
                case "set":result=session.SetRemoteDescription(step.TryGetProperty("answer",out var answer)&&answer.GetBoolean()?SdpType.answer:SdpType.offer,SDP.ParseSDPDescription(step.GetProperty("sdp").GetString().Replace("$PORT",port.ToString()))).ToString();break;
                case "status":session.SetMediaStreamStatus(SDPMediaTypesEnum.audio,Enum.Parse<MediaStreamStatusEnum>(step.GetProperty("value").GetString()));break;
                case "challenge":typeof(SipMediaSession).GetMethod("ChallengeCodecs",Flags).Invoke(session,new object[]{step.GetProperty("enabled").GetBoolean()});break;
                case "close":session.Close("fixture");break;
                case "dispose":session.DisposeAsync().AsTask().GetAwaiter().GetResult();break;
                case "send":{
                    long before=Raw(session);var bytes=Convert.FromHexString(step.GetProperty("hex").GetString());
                    (step.TryGetProperty("foreign",out var f)&&f.GetBoolean()?foreign:peer).Send(bytes,bytes.Length,local);
                    if(!SpinWait.SpinUntil(()=>Raw(session)>before,TimeSpan.FromSeconds(3)))throw new Exception("Original datagram observation unavailable");break;
                }
                case "counters":{
                    long Number(string key)=>long.Parse(step.GetProperty(key).GetString(),CultureInfo.InvariantCulture);
                    Set(session,"sentPackets",Number("sent"));Set(session,"receivedPackets",Number("received"));
                    Set(session,"reportsReceived",Number("reports"));Set(session,"reportsSent",Number("reports"));
                    Set(session,"lastReportAt",new DateTimeOffset(Number("ticks"),TimeSpan.Zero));
                    Set(session,"peerReceptionReports",new object[]{new{ssrc=uint.MaxValue,fractionLost256=(byte)255,packetsLost=0xFFFFFFu,extendedHighestSequence=uint.MaxValue,jitterTimestampUnits=uint.MaxValue}});
                    var counts=(long[])typeof(SipMediaSession).GetField("rawPayloadCounts",Flags).GetValue(session);counts[8]=Number("received");
                    foreach(string field in new[]{"rawRtpPackets","rawOfferedEventPackets","parsedEventPackets","libraryEventPackets","unsupportedPayloadPackets","inBandAnalyzedPackets","inBandDetectedDigits","rfc4733DetectedDigits"})Set(session,field,Number("reports"));break;
                }
                default:throw new Exception("Unknown diagnostic fixture action");
            }
            string diagnostics=JsonSerializer.Serialize(session.Diagnostics,NativeRpc.Json).Replace(local.ToString(),"local").Replace(new IPEndPoint(IPAddress.Loopback,port).ToString(),"peer");
            rows.Add(new{result,diagnostics});
        }}finally{session.DisposeAsync().AsTask().GetAwaiter().GetResult();}
        return rows;
    }
    public static void Run(){using var input=JsonDocument.Parse(Console.In.ReadToEnd());Console.Write(JsonSerializer.Serialize(input.RootElement.EnumerateArray().Select(RunCase).ToArray(),NativeRpc.Json));}
}
