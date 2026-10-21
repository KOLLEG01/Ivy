using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Ivy.PhoneBridge;

// The original DLL owns all RPC, SIP/configuration and Desktop input behavior.
// Only Micro construction/status/disposal and the closed PCM port are supplied.
public static partial class PhoneNativeLifecycleBoundariesOracle {
    const string Epoch="11111111-1111-4111-8111-111111111111";
    sealed class Port : IPcmAudioPort {
        public bool IsOpen=>false;
        public long Generation=>0;
        public int ReadCaptured(Span<float> output) { output.Clear(); return 0; }
        public void WriteReceived(ReadOnlySpan<float> input) {}
    }
    static bool Released(IPEndPoint endpoint) {
        using var socket=new Socket(endpoint.AddressFamily,SocketType.Dgram,ProtocolType.Udp);
        socket.ExclusiveAddressUse=true;
        try { socket.Bind(endpoint); return true; }
        catch(SocketException) { return false; }
    }
    static IPEndPoint Endpoint(NativeSipOperations owner) {
        var host=typeof(NativeSipOperations).GetField("host",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(owner);
        return (IPEndPoint)host.GetType().GetProperty("LocalEndpoint").GetValue(host);
    }
    sealed class Device : IMicroVoiceInput {
        public int Mode, CloseMode, Closes;
        public IPEndPoint Endpoint;
        public bool? CallerSawBindingReleased;
        public MicroAttachmentStatus Status=>Mode switch {
            6=>throw new ArgumentException("fixture status"),
            7=>throw new InvalidOperationException("fixture status"),
            _=>new("ready",1,null)
        };
        public MicroInputReceipt SendMicrophone(bool pressed,long generation,Func<bool> original)=>throw new Exception("No input authorized");
        public ValueTask DisposeAsync() {
            Closes++; CallerSawBindingReleased=Released(Endpoint);
            if(CloseMode==2) throw new InvalidOperationException("fixture dispose constructor");
            return CloseMode==1 ? new(Task.FromException(new InvalidOperationException("fixture dispose work"))) : ValueTask.CompletedTask;
        }
    }
    static IMicroVoiceInput Create(int mode,Device device)=>mode switch {
        0=>null,
        1=>throw new IOException("fixture factory"),
        2=>throw new UnauthorizedAccessException("fixture factory"),
        3=>throw new SocketException((int)SocketError.AddressAlreadyInUse),
        4=>throw new ArgumentException("fixture factory"),
        5=>throw new InvalidOperationException("fixture factory"),
        _=>device
    };
    static NativeRequest Request(string method,JsonElement args,int operation,int request)=>new(1,Epoch,request,
        method is "status" or "heartbeat" ? null : $"00000000-0000-4000-8000-{operation:x12}",method,args);
    static JsonElement Args(string json)=>JsonDocument.Parse(json).RootElement.Clone();
    static JsonElement Project(string method,NativeResponse response) {
        if(!response.Ok) return JsonSerializer.SerializeToElement(new { error=response.Error },NativeRpc.Json);
        var value=response.Result.Value;
        if(method=="configure") return JsonSerializer.SerializeToElement(new { configured=value.GetProperty("configured").GetBoolean() },NativeRpc.Json);
        if(method=="call.prepare") return JsonSerializer.SerializeToElement(new { id=value.GetProperty("id").GetString(),state=value.GetProperty("state").GetString() },NativeRpc.Json);
        if(method=="status") return JsonSerializer.SerializeToElement(new {
            configured=value.GetProperty("configured").GetBoolean(),
            endpointPresent=value.GetProperty("endpoint").ValueKind==JsonValueKind.String,
            micro=value.GetProperty("micro").Clone()
        },NativeRpc.Json);
        return value.Clone();
    }
    static object Partial(JsonElement input) {
        int mode=input.GetProperty("mode").GetInt32(), closeMode=input.GetProperty("closeMode").GetInt32();
        var args=input.GetProperty("params");
        var selected=Args($"{{\"callId\":\"{Epoch}\"}}");
        int factories=0,ports=0;
        var device=new Device { Mode=mode,CloseMode=closeMode };
        var desktop=new PhoneDesktopRuntime(microFactory:_=> { factories++; return Create(mode,device); });
        var owner=new NativeSipOperations(_=> { ports++; return new Port(); },desktop);
        var rpc=new NativeRpc(Epoch,owner.InvokeAsync,64,()=>Math.Max(4,owner.ActiveCalls*4));
        var rows=new List<JsonElement>();
        var configured=rpc.ExecuteAsync(Request("configure",args,1,1)).GetAwaiter().GetResult();
        rows.Add(Project("configure",configured));
        int factoriesAfterConfigure=factories;
        var endpoint=Endpoint(owner); device.Endpoint=endpoint;
        bool endpointBound=!Released(endpoint);
        var replay=rpc.ExecuteAsync(Request("configure",args,1,2)).GetAwaiter().GetResult();
        rows.Add(Project("configure",replay));
        bool replaySame=configured.Ok==replay.Ok && configured.Error==replay.Error && configured.Result?.GetRawText()==replay.Result?.GetRawText();
        rows.Add(Project("configure",rpc.ExecuteAsync(Request("configure",Args("{}"),3,3)).GetAwaiter().GetResult()));
        rows.Add(Project("status",rpc.ExecuteAsync(Request("status",Args("{}"),0,4)).GetAwaiter().GetResult()));
        var prepared=rpc.ExecuteAsync(Request("call.prepare",selected,2,5)).GetAwaiter().GetResult();
        if(!prepared.Ok) throw new Exception("Original fixture media must construct: "+prepared.Error);
        rows.Add(Project("call.prepare",prepared));
        var preparedReplay=rpc.ExecuteAsync(Request("call.prepare",selected,2,6)).GetAwaiter().GetResult();
        rows.Add(Project("call.prepare",preparedReplay));
        rows.Add(Project("call.release",rpc.ExecuteAsync(Request("call.release",selected,4,7)).GetAwaiter().GetResult()));
        rows.Add(Project("status",rpc.ExecuteAsync(Request("status",Args("{}"),0,8)).GetAwaiter().GetResult()));
        var disposal=owner.DisposeAsync().AsTask();
        string disposalError=null;
        try { disposal.GetAwaiter().GetResult(); } catch { disposalError="operation_outcome_unknown"; }
        var repeated=owner.DisposeAsync().AsTask();
        try { repeated.GetAwaiter().GetResult(); } catch {}
        return new { mode,closeMode,rows,endpointBound,factoriesAfterConfigure,replaySame,ports,
            callerSawBindingReleased=device.CallerSawBindingReleased,deviceDisposals=device.Closes,
            disposalError,cachedDisposal=ReferenceEquals(disposal,repeated),bindingReleased=Released(endpoint) };
    }
    static object Concurrent(JsonElement input) {
        int mode=input.GetProperty("mode").GetInt32();
        var args=input.GetProperty("params");
        var selected=Args($"{{\"callId\":\"{Epoch}\"}}");
        using var entered=new ManualResetEventSlim(); using var release=new ManualResetEventSlim();
        using var secondEntered=new ManualResetEventSlim();
        int factories=0,ports=0; bool invocationLockHeld=false;
        var device=new Device { Mode=mode };
        NativeSipOperations owner=null; NativeRpc rpc=null;
        var desktop=new PhoneDesktopRuntime(microFactory:_=> {
            factories++; device.Endpoint=Endpoint(owner);
            var sync=typeof(NativeRpc).GetField("sync",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(rpc);
            invocationLockHeld=Monitor.IsEntered(sync);
            entered.Set(); if(!release.Wait(10000)) throw new Exception("fixture release unavailable");
            return Create(mode,device);
        });
        owner=new NativeSipOperations(_=> { Interlocked.Increment(ref ports); return new Port(); },desktop);
        rpc=new NativeRpc(Epoch,owner.InvokeAsync,64,()=>Math.Max(4,owner.ActiveCalls*4));
        var configuring=rpc.ExecuteAsync(Request("configure",args,1,1));
        if(!entered.Wait(10000)) throw new Exception("fixture factory not entered");
        var second=Task.Factory.StartNew(()=> { secondEntered.Set(); return rpc.ExecuteAsync(Request("call.prepare",selected,2,2)); },
            CancellationToken.None,TaskCreationOptions.LongRunning,TaskScheduler.Default).Unwrap();
        if(!secondEntered.Wait(10000)) throw new Exception("fixture caller not entered");
        bool secondBeforeRelease=second.Wait(50), endpointBound=!Released(device.Endpoint);
        int receiptsBeforeRelease=((System.Collections.IDictionary)typeof(NativeRpc).GetField("receipts",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(rpc)).Count;
        int portsBeforeRelease=Volatile.Read(ref ports);
        release.Set();
        var configured=configuring.GetAwaiter().GetResult();
        var prepared=second.GetAwaiter().GetResult();
        if(!prepared.Ok) throw new Exception("Original fixture media must construct: "+prepared.Error);
        var replay=rpc.ExecuteAsync(Request("call.prepare",selected,2,3)).GetAwaiter().GetResult();
        bool replaySame=prepared.Ok==replay.Ok && prepared.Error==replay.Error && prepared.Result?.GetRawText()==replay.Result?.GetRawText();
        rpc.ExecuteAsync(Request("call.release",selected,4,4)).GetAwaiter().GetResult();
        owner.DisposeAsync().AsTask().GetAwaiter().GetResult();
        return new { mode,secondBeforeRelease,endpointBound,receiptsBeforeRelease,portsBeforeRelease,
            configured=Project("configure",configured),prepared=Project("call.prepare",prepared),replay=Project("call.prepare",replay),
            replaySame,factories,ports,bindingReleased=Released(device.Endpoint),invocationLockHeld };
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());
        var partial=new List<object>(); var concurrent=new List<object>();
        foreach(var item in input.RootElement.EnumerateArray()) {
            if(item.GetProperty("kind").GetString()=="partial") partial.Add(Partial(item));
            else concurrent.Add(Concurrent(item));
        }
        Console.Write(JsonSerializer.Serialize(new { partial,concurrent },NativeRpc.Json));
    }
}
