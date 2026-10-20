using System;
using System.Collections.Generic;
using System.Net.Sockets;
using System.Reflection;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Ivy.PhoneBridge;

// Native call composition on an injected PCM route, without Windows devices.
public static class PhoneNativeScreeningOracle {
    sealed class Route : ICallAudioRoute {
        public Func<bool> Allowed;
        volatile string mode="suspended";
        long generation;
        public bool IsOpen=>mode=="open"&&Allowed();
        public long Generation=>Interlocked.Read(ref generation);
        public bool Failed=>false;
        public MediaStatus Status=>new(mode,"microphone",48000,1,0,0,0,0,10);
        public void Resume(){if(!Allowed()||mode=="closed")throw new InvalidOperationException();mode="open";Interlocked.Increment(ref generation);}
        public void Suspend(){if(mode=="open")mode="suspended";}
        public int ReadCaptured(Span<float> output){output.Fill(IsOpen ? .25f : 0);return IsOpen?output.Length:0;}
        public void WriteReceived(ReadOnlySpan<float> input){}
        public ValueTask DisposeAsync(){mode="closed";return ValueTask.CompletedTask;}
    }
    sealed class Gate {
        public readonly TaskCompletionSource Ready=new(TaskCreationOptions.RunContinuationsAsynchronously);
        public readonly TaskCompletionSource Released=new(TaskCreationOptions.RunContinuationsAsynchronously);
    }
    static readonly Dictionary<string,Gate> gates=new();
    public static CallAudioPort Audio(string id)=>new(id,new AudioPermit(),async (_,_,allowed,_)=>{
        if(gates.TryGetValue(id,out var gate)){gate.Ready.TrySetResult();await gate.Released.Task;}
        return new Route{Allowed=allowed};
    });
    public static void Begin(string id,string scenario) {gates.Remove(id);if(scenario=="screening_end_during_bridge")gates[id]=new Gate();}
    public static bool EndsItself(string scenario)=>scenario is "screening_timeout" or "screening_decline" or "screening_end_during_bridge";
    static SipTransportHost Host(NativeSipOperations owner)=>(SipTransportHost)typeof(NativeSipOperations).GetField("host",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(owner);
    static object Settings()=>new{captureEndpointId="capture-fixture",renderEndpointId="render-fixture",sourceMode="microphone",captureBufferMs=20,renderLatencyMs=10,queueMs=60};
    static void Digit(PhoneCallFeatures features,char key) {
        var digit=typeof(PhoneCallFeatures).GetMethod("Digit",BindingFlags.Instance|BindingFlags.NonPublic);
        digit.Invoke(features,new object[]{key,Enum.Parse(digit.GetParameters()[1].ParameterType,"SipInfo")});
    }
    static async Task Wait(Func<bool> ready){var until=DateTime.UtcNow.AddSeconds(4);while(!ready()){if(DateTime.UtcNow>=until)throw new Exception("Original screening evidence unavailable");await Task.Delay(1);}}
    public static async Task Connected(NativeSipOperations owner,string id,string scenario,UdpClient rtp,List<object> rows,JsonElement args) {
        var call=Host(owner).FindCall(id);
        if(call.Features.Observation.Screening!="waiting")throw new Exception("Original connected screening state unavailable");
        var packet=await rtp.ReceiveAsync().WaitAsync(TimeSpan.FromSeconds(1));
        if(packet.Buffer.Length<13||(packet.Buffer[1]&127)!=8)throw new Exception("Original screening RTP unavailable");
        rows.Add(await PhoneNativeDialOracle.FeedbackRow(owner,"call.screening.bridge",new{callId=id,settings=(object)null}));
        if(scenario=="screening_retired")return;
        if(scenario=="screening_decline"){Digit(call.Features,'2');await Wait(()=>call.Observation.State=="local_ended");return;}
        if(scenario=="screening_timeout"){await Wait(()=>call.Features.Observation.Screening=="timeout");return;}
        Digit(call.Features,'1');
        if(call.Features.Observation.Screening!="accepted")throw new Exception("Original screening acceptance unavailable");
        // Accepted screening closes the announcement while caller PCM is closed.
        using(var cancel=new CancellationTokenSource(150)){
            for(;;){try{await rtp.ReceiveAsync(cancel.Token);}catch(OperationCanceledException){break;}}
        }
        using(var cancel=new CancellationTokenSource(150)){
            try{await rtp.ReceiveAsync(cancel.Token);throw new Exception("Original accepted announcement still sending");}catch(OperationCanceledException){}
        }
        if(scenario=="screening_accept_late_bridge")await Task.Delay(1200);
        if(args.TryGetProperty("audioSettingsCases",out var cases))foreach(var settings in cases.EnumerateArray())rows.Add(await PhoneNativeDialOracle.FeedbackRow(owner,"call.screening.bridge",new{callId=id,settings}));
        rows.Add(await PhoneNativeDialOracle.FeedbackRow(owner,"call.screening.bridge",new{callId=id,settings=new{}}));
        if(scenario=="screening_end_during_bridge"){
            var pending=PhoneNativeDialOracle.FeedbackRow(owner,"call.screening.bridge",new{callId=id,settings=Settings()});
            var gate=gates[id];await gate.Ready.Task.WaitAsync(TimeSpan.FromSeconds(4));
            var ended=await PhoneNativeDialOracle.FeedbackRow(owner,"call.hangup",new{callId=id});
            gate.Released.TrySetResult();rows.Add(await pending.WaitAsync(TimeSpan.FromSeconds(4)));rows.Add(ended);
            return;
        }
        rows.Add(await PhoneNativeDialOracle.FeedbackRow(owner,"call.screening.bridge",new{callId=id,settings=Settings()}));
        rows.Add(await PhoneNativeDialOracle.FeedbackRow(owner,"call.screening.bridge",new{callId=id,settings=Settings()}));
        rows.Add(await PhoneNativeDialOracle.FeedbackRow(owner,"call.windows.connect",new{callId=id,settings=Settings()}));
        if(scenario=="screening_bridge")await Task.Delay(1200);
        foreach(char key in "*0#")Digit(call.Features,key);
        if(call.Features.Observation.Screening!="bridged"||call.Features.Observation.CommandSequence!=0||call.Media.AudioStatus.State!="open")throw new Exception("Original screening bridge authority unavailable");
    }
}
