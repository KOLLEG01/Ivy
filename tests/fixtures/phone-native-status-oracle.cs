using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Text;
using System.Text.Json;
using System.Threading;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP;

// Original native status over owned loopback and unprepared call audio only.
public static class PhoneNativeStatusOracle {
    const BindingFlags Flags=BindingFlags.Instance|BindingFlags.NonPublic;
    static SipTransportHost Host(NativeSipOperations owner)=>(SipTransportHost)typeof(NativeSipOperations).GetField("host",Flags).GetValue(owner);
    static object SignallingDto(JsonElement args) {
        var transport=new SIPTransport();using var diagnostic=new SipSignallingDiagnostics(transport,"fixture");
        try {
            long value=long.Parse(args.GetProperty("value").GetString(),System.Globalization.CultureInfo.InvariantCulture);
            typeof(SipSignallingDiagnostics).GetField("sent",Flags).SetValue(diagnostic,value);
            typeof(SipSignallingDiagnostics).GetField("received",Flags).SetValue(diagnostic,value);
            var events=(Queue<object>)typeof(SipSignallingDiagnostics).GetField("events",Flags).GetValue(diagnostic);
            events.Enqueue(new{at=new DateTimeOffset(638000000000000001,TimeSpan.Zero),direction="sent",method="INVITE",status=(int?)null});
            events.Enqueue(new{at=new DateTimeOffset(638000000000000001,TimeSpan.Zero),direction="received",method="INVITE",status=(int?)200});
            return new{json=JsonSerializer.Serialize(diagnostic.Observation,NativeRpc.Json)};
        }finally{transport.Shutdown();}
    }
    static object Signal(NativeSipOperations owner,UdpClient peer,JsonElement args) {
        var host=Host(owner);var call=host.FindCall(args.GetProperty("callId").GetString());
        long before=JsonSerializer.SerializeToElement(call.Signalling.Observation,NativeRpc.Json).GetProperty("received").GetInt64();
        int count=args.GetProperty("count").GetInt32();
        for(int i=0;i<count;i++) {
            string body="protected-fixture-body-never-status";
            string raw=$"SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP {peer.Client.LocalEndPoint};branch=z9hG4bK-status-{i}\r\nFrom: <sip:fixture@localhost>;tag=foreign\r\nTo: <sip:target@localhost>;tag=unselected\r\nCall-ID: {call.Observation.SipCallId}\r\nCSeq: {100+i} MESSAGE\r\nContent-Type: application/octet-stream\r\nContent-Length: {Encoding.UTF8.GetByteCount(body)}\r\n\r\n{body}";
            byte[] bytes=Encoding.UTF8.GetBytes(raw);peer.Send(bytes,bytes.Length,host.LocalEndpoint);
            if(!SpinWait.SpinUntil(()=>JsonSerializer.SerializeToElement(call.Signalling.Observation,NativeRpc.Json).GetProperty("received").GetInt64()>=before+i+1,TimeSpan.FromSeconds(3)))throw new Exception("Original signalling observation unavailable");
        }
        return new{observed=true};
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());
        var owner=new NativeSipOperations(id=>new CallAudioPort(id,new AudioPermit()));
        using var peer=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
        var rows=new List<object>();
        try{foreach(var step in input.RootElement.EnumerateArray()){
            try{
                string method=step.GetProperty("method").GetString();var args=step.GetProperty("params");
                object result;
                if(method=="fixture.offer"){Host(owner).FindCall(args.GetProperty("callId").GetString()).Media.CreateOffer(IPAddress.Loopback);result=new{offered=true};}
                else if(method=="fixture.signal")result=Signal(owner,peer,args);
                else if(method=="fixture.signallingDto")result=SignallingDto(args);
                else if(method=="fixture.dispose"){owner.DisposeAsync().AsTask().GetAwaiter().GetResult();result=new{disposed=true};}
                else result=owner.InvokeAsync(method,args).GetAwaiter().GetResult();
                rows.Add(new{json=JsonSerializer.Serialize(result,NativeRpc.Json)});
            }catch(Exception e){rows.Add(new{error=e is NativeRpcException rpc?rpc.Code:e is ArgumentException||e is JsonException?"invalid_request":"operation_outcome_unknown"});}
        }}finally{owner.DisposeAsync().AsTask().GetAwaiter().GetResult();}
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}
