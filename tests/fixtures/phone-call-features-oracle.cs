using System;
using System.Collections.Generic;
using System.Reflection;
using System.Text.Json;
using System.Threading;
using Ivy.PhoneBridge;

// The sealed original DLL supplies policy, gate, limiter and serializer. This
// fixture replaces only its documented time input and teardown callback.
public static class PhoneCallFeaturesOracle {
    const BindingFlags Flags=BindingFlags.Instance|BindingFlags.NonPublic|BindingFlags.Public;
    sealed class Clock : TimeProvider {
        public long Ticks=638_000_000_000_000_000;
        readonly List<Timer> timers=new();
        public override DateTimeOffset GetUtcNow()=>new(Ticks,TimeSpan.Zero);
        public override ITimer CreateTimer(TimerCallback callback, object state, TimeSpan dueTime, TimeSpan period) {
            var timer=new Timer(this,callback,state,dueTime,period); lock(timers)timers.Add(timer); return timer;
        }
        public void Advance(long milliseconds) {
            Ticks+=milliseconds*10_000;
            Timer[] current; lock(timers)current=timers.ToArray();
            foreach(var timer in current)timer.Tick(Ticks);
        }
        sealed class Timer : ITimer {
            readonly Clock clock; readonly TimerCallback callback; readonly object state; long due; long period; bool disposed;
            public Timer(Clock clock,TimerCallback callback,object state,TimeSpan due,TimeSpan period) {
                this.clock=clock;this.callback=callback;this.state=state;Change(due,period);
            }
            public bool Change(TimeSpan due,TimeSpan period) { this.due=due==Timeout.InfiniteTimeSpan?long.MaxValue:clock.Ticks+due.Ticks;this.period=period.Ticks;return !disposed; }
            public void Tick(long now) { if(disposed||now<due)return;due=period>0?now+period:long.MaxValue;callback(state); }
            public void Dispose(){disposed=true;}
            public System.Threading.Tasks.ValueTask DisposeAsync(){Dispose();return default;}
        }
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());
        if(input.RootElement.ValueKind==JsonValueKind.Object){Gate(input.RootElement);return;}
        var scenarios=new List<object>();
        var digitMethod=typeof(PhoneCallFeatures).GetMethod("Digit",Flags);
        var sourceType=digitMethod.GetParameters()[1].ParameterType;
        var needs=typeof(PhoneCallFeatures).GetProperty("NeedsInBandDtmf",Flags);
        var limiterType=typeof(PhoneCallFeatures).Assembly.GetType("Ivy.PhoneBridge.DtmfAccessFailureLimiter");
        foreach(var scenario in input.RootElement.EnumerateArray()) {
            var limiter=Activator.CreateInstance(limiterType,true);var clock=new Clock();int hangups=0;
            Action hangup=()=>Interlocked.Increment(ref hangups);
            Func<PhoneCallFeatures> create=()=>(PhoneCallFeatures)Activator.CreateInstance(typeof(PhoneCallFeatures),Flags,null,new object[]{limiter,hangup,clock},null);
            var features=create();var rows=new List<object>();
            try { foreach(var step in scenario.EnumerateArray()) {
                string error=null;
                try { switch(step.GetProperty("op").GetString()) {
                    case "configure": features.Configure(step.GetProperty("challenge").Deserialize<CallAccessSettings>(NativeRpc.Json),step.GetProperty("incoming").GetBoolean());break;
                    case "connected":features.Connected();break;
                    case "digits":
                        string source=step.TryGetProperty("source",out var selected)?selected.GetString():"Rfc4733";
                        foreach(char digit in step.GetProperty("digits").GetString())digitMethod.Invoke(features,new[]{(object)digit,Enum.Parse(sourceType,source)});break;
                    case "screen":features.StartScreening();break;
                    case "bridge":features.BridgeScreening();break;
                    case "screenTimeout":features.ScreeningTimeout();break;
                    case "dispose":features.Dispose();break;
                    case "advance":clock.Advance(step.GetProperty("milliseconds").GetInt64());break;
                    case "wait":
                        int expected=step.GetProperty("hangups").GetInt32();
                        if(!SpinWait.SpinUntil(()=>Volatile.Read(ref hangups)>=expected,TimeSpan.FromSeconds(3)))throw new Exception("Original teardown did not complete");break;
                    case "new":features.Dispose();features=create();break;
                    case "status":break;
                    default:throw new Exception("Unknown fixture action");
                }} catch(Exception e) {
                    while(e is TargetInvocationException reflected)e=reflected.InnerException;
                    error=e is NativeRpcException rpc?rpc.Code:e is ArgumentException||e is JsonException?"invalid_request":e is InvalidOperationException?"operation_outcome_unknown":throw e;
                }
                string op=step.GetProperty("op").GetString();
                rows.Add(op=="advance"?(object)new{error}:new{error,features=features.Observation,needsInBand=(bool)needs.GetValue(features),authenticated=features.Authenticated,hangups=op=="wait"?(int?)Volatile.Read(ref hangups):null});
            }} finally{features.Dispose();}
            scenarios.Add(rows);
        }
        Console.Write(JsonSerializer.Serialize(scenarios,NativeRpc.Json));
    }
    static void Gate(JsonElement input) {
        var type=typeof(PhoneCallFeatures).Assembly.GetType("Ivy.PhoneBridge.DtmfAccessCodeGate");
        var rows=new List<object>();
        foreach(var step in input.GetProperty("cases").EnumerateArray()) {
            try {
                var gate=Activator.CreateInstance(type,Flags,null,new object[]{step.GetProperty("code").GetString()},null);
                var digits=new List<object>();
                foreach(char digit in step.GetProperty("digits").GetString())digits.Add(new{progress=type.GetMethod("ProcessDigit").Invoke(gate,new object[]{digit}).ToString(),accepted=(bool)type.GetProperty("IsAccepted").GetValue(gate),disable=(bool)type.GetProperty("DisableCodecRenegotiation").GetValue(gate)});
                rows.Add(new{digits});
            } catch(Exception e) { while(e is TargetInvocationException reflected)e=reflected.InnerException;if(e is not ArgumentException)throw;rows.Add(new{error="invalid_request"}); }
        }
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}
