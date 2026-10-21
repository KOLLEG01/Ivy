using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Ivy.PhoneBridge;

public static partial class PhoneNativeLifecycleBoundariesOracle {
    const string Other="21111111-1111-4111-8111-111111111111";
    static object CountedStatus(NativeResponse response) {
        if(!response.Ok) throw new Exception("Fixture status unavailable: "+response.Error);
        var value=response.Result.Value;
        return new { configured=value.GetProperty("configured").GetBoolean(),callIds=value.GetProperty("callIds").Clone() };
    }
    static int Receipts(NativeRpc rpc)=>((System.Collections.IDictionary)typeof(NativeRpc).GetField("receipts",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(rpc)).Count;
    static bool InvocationLock(NativeRpc rpc)=>Monitor.IsEntered(typeof(NativeRpc).GetField("sync",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(rpc));
    static List<Socket> Occupy(JsonElement args) {
        var result=new List<Socket>();
        try {
            var binding=args.GetProperty("binding");
            int first=binding.GetProperty("rtpPortRangeStart").GetInt32(),last=binding.GetProperty("rtpPortRangeEnd").GetInt32();
            for(int port=first;port<=last+1;port++) {
                var socket=new Socket(AddressFamily.InterNetwork,SocketType.Dgram,ProtocolType.Udp);
                try { socket.ExclusiveAddressUse=true;socket.Bind(new IPEndPoint(IPAddress.Loopback,port));result.Add(socket); }
                catch { socket.Dispose();throw; }
            }
            return result;
        }catch{foreach(var socket in result)socket.Dispose();throw;}
    }
    static object Construction(JsonElement input) {
        int mode=input.GetProperty("mode").GetInt32();var args=input.GetProperty("params");
        using var entered=new ManualResetEventSlim();using var release=new ManualResetEventSlim();using var secondEntered=new ManualResetEventSlim();
        var selected=Args($"{{\"callId\":\"{Epoch}\"}}");var other=Args($"{{\"callId\":\"{Other}\"}}");
        NativeRpc rpc=null;int factories=0;bool lockHeld=false;
        var owner=new NativeSipOperations(_=>{
            int attempt=Interlocked.Increment(ref factories);
            if(attempt==1){lockHeld=InvocationLock(rpc);entered.Set();if(!release.Wait(10000))throw new Exception("fixture release unavailable");
                if(mode==1)throw new ArgumentException("fixture before media resources");
                if(mode==2)throw new IOException("fixture before media resources");}
            return new Port();
        });
        rpc=new NativeRpc(Epoch,owner.InvokeAsync,64,()=>Math.Max(4,owner.ActiveCalls*4));
        var configured=rpc.ExecuteAsync(Request("configure",args,1,1)).GetAwaiter().GetResult();
        if(!configured.Ok)throw new Exception("Fixture configure unavailable");
        var endpoint=Endpoint(owner);var holders=mode==3?Occupy(args):new List<Socket>();int held=holders.Count;
        try {
            var first=rpc.ExecuteAsync(Request("call.prepare",selected,2,2));
            if(!entered.Wait(10000))throw new Exception("fixture constructor not entered");
            var second=Task.Factory.StartNew(()=>{secondEntered.Set();return rpc.ExecuteAsync(Request("call.hangup",other,3,3));},CancellationToken.None,TaskCreationOptions.LongRunning,TaskScheduler.Default).Unwrap();
            if(!secondEntered.Wait(10000))throw new Exception("fixture caller not entered");
            bool secondBeforeRelease=second.Wait(50);int receiptsBeforeRelease=Receipts(rpc);
            release.Set();var prepared=first.GetAwaiter().GetResult();var otherResult=second.GetAwaiter().GetResult();
            var replay=rpc.ExecuteAsync(Request("call.prepare",selected,2,4)).GetAwaiter().GetResult();
            bool same=prepared.Ok==replay.Ok&&prepared.Error==replay.Error&&prepared.Result?.GetRawText()==replay.Result?.GetRawText();
            var status=CountedStatus(rpc.ExecuteAsync(Request("status",Args("{}"),0,5)).GetAwaiter().GetResult());
            var probe=new List<JsonElement>();object resources=null;
            if(mode==3){
                using var process=Process.GetCurrentProcess();_ = process.Handle;_ = process.HandleCount;
                int before=process.HandleCount;
                for(int attempt=0;attempt<8;attempt++)probe.Add(Project("call.prepare",rpc.ExecuteAsync(Request("call.prepare",selected,20+attempt,20+attempt)).GetAwaiter().GetResult()));
                int after=process.HandleCount;
                resources=new{attempts=8,handlesBefore=before,handlesAfter=after,handleDelta=after-before};
            }
            var released=rpc.ExecuteAsync(Request("call.release",selected,4,6)).GetAwaiter().GetResult();
            foreach(var socket in holders)socket.Dispose();holders.Clear();
            var next=rpc.ExecuteAsync(Request("call.prepare",selected,5,7)).GetAwaiter().GetResult();
            if(!next.Ok)throw new Exception("Fixture replacement media must construct: "+next.Error);
            var oldPrepare=rpc.ExecuteAsync(Request("call.prepare",selected,2,8)).GetAwaiter().GetResult();
            var oldRelease=rpc.ExecuteAsync(Request("call.release",selected,4,9)).GetAwaiter().GetResult();
            var replacement=CountedStatus(rpc.ExecuteAsync(Request("status",Args("{}"),0,10)).GetAwaiter().GetResult());
            var final=rpc.ExecuteAsync(Request("call.release",selected,6,11)).GetAwaiter().GetResult();
            owner.DisposeAsync().AsTask().GetAwaiter().GetResult();
            return new { kind="construction",mode,secondBeforeRelease,receiptsBeforeRelease,invocationLockHeld=lockHeld,
                rows=new object[]{Project("call.prepare",prepared),Project("call.hangup",otherResult),Project("call.prepare",replay),status,Project("call.release",released),Project("call.prepare",next),Project("call.prepare",oldPrepare),Project("call.release",oldRelease),replacement,Project("call.release",final)},
                failedPrepareProbe=probe,replaySame=same,factories,ownedHeldPorts=held,bindingReleased=Released(endpoint),resources };
        }finally{release.Set();foreach(var socket in holders)socket.Dispose();owner.DisposeAsync().AsTask().GetAwaiter().GetResult();}
    }
    sealed class PrefixDevice:IMicroVoiceInput {
        public Func<MicroAttachmentStatus> Read;
        public int Closes;
        public MicroAttachmentStatus Status=>Read();
        public MicroInputReceipt SendMicrophone(bool pressed,long generation,Func<bool> original)=>throw new Exception("No input authorized");
        public ValueTask DisposeAsync(){Closes++;return ValueTask.CompletedTask;}
    }
    static object StatusPrefix(JsonElement input) {
        string method=input.GetProperty("method").GetString();var args=input.GetProperty("params");
        using var entered=new ManualResetEventSlim();using var release=new ManualResetEventSlim();using var secondEntered=new ManualResetEventSlim();
        NativeRpc rpc=null;int block=0,reads=0;bool held=false;
        var device=new PrefixDevice{Read=()=>{
            Interlocked.Increment(ref reads);
            if(Interlocked.Exchange(ref block,0)==1){held=InvocationLock(rpc);entered.Set();if(!release.Wait(10000))throw new Exception("fixture status release unavailable");}
            return new("ready",1,null);
        }};
        var owner=new NativeSipOperations(_=>new Port(),new PhoneDesktopRuntime(microFactory:_=>device));
        rpc=new NativeRpc(Epoch,owner.InvokeAsync,64,()=>Math.Max(4,owner.ActiveCalls*4));
        if(!rpc.ExecuteAsync(Request("configure",args,1,1)).GetAwaiter().GetResult().Ok)throw new Exception("Fixture configure unavailable");
        var endpoint=Endpoint(owner);Volatile.Write(ref block,1);
        try {
            var observation=rpc.ExecuteAsync(Request(method,Args("{}"),0,2));
            if(!entered.Wait(10000))throw new Exception("fixture status not entered");
            var second=Task.Factory.StartNew(()=>{secondEntered.Set();return rpc.ExecuteAsync(Request("configure",Args("{}"),2,3));},CancellationToken.None,TaskCreationOptions.LongRunning,TaskScheduler.Default).Unwrap();
            if(!secondEntered.Wait(10000))throw new Exception("fixture caller not entered");
            bool before=second.Wait(50);int count=Receipts(rpc);release.Set();
            var first=observation.GetAwaiter().GetResult();var conflict=second.GetAwaiter().GetResult();
            var fresh=rpc.ExecuteAsync(Request(method,Args("{}"),0,4)).GetAwaiter().GetResult();
            owner.DisposeAsync().AsTask().GetAwaiter().GetResult();
            return new {kind="status",method,secondBeforeRelease=before,receiptsBeforeRelease=count,invocationLockHeld=held,
                rows=new object[]{Project("status",first),Project("configure",conflict),Project("status",fresh)},reads,deviceDisposals=device.Closes,bindingReleased=Released(endpoint)};
        }finally{release.Set();owner.DisposeAsync().AsTask().GetAwaiter().GetResult();}
    }
    public static void RunConstructionPrefix(){
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());var rows=new List<object>();
        foreach(var item in input.RootElement.EnumerateArray())rows.Add(item.GetProperty("kind").GetString()=="construction"?Construction(item):StatusPrefix(item));
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}
