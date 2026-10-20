using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Ivy.PhoneBridge;

// Sealed original controller/progress methods; no device or account is opened.
public static class PhoneCallAudioOracle {
    sealed class Time : TimeProvider {
        public long Ticks;
        public override long TimestampFrequency => 1000;
        public override long GetTimestamp() => Ticks;
    }
    sealed class Route : ICallAudioRoute {
        public Func<bool> Allowed=()=>false;
        public string Mode="suspended";
        public long Generation {get;private set;}
        public int Resumes,Suspends,Disposals,Reads,Writes;
        public bool BadClose,BadSuspend;
        public bool IsOpen=>Mode=="open"&&Allowed();
        public bool Failed=>Mode=="failed";
        public MediaStatus Status=>new(Mode,"microphone",48000,1,0,0,0,0,10);
        public void Resume(){if(!Allowed()||Mode=="closed"||Mode=="failed")throw new InvalidOperationException();Mode="open";Generation++;Resumes++;}
        public void Suspend(){Suspends++;if(BadSuspend)throw new InvalidOperationException();if(Mode=="open")Mode="suspended";}
        public int ReadCaptured(Span<float> output){Reads++;output.Fill(IsOpen ? .25f : 0);return IsOpen?output.Length:0;}
        public void WriteReceived(ReadOnlySpan<float> input){if(IsOpen)Writes++;}
        public ValueTask DisposeAsync(){Disposals++;Mode="closed";if(BadClose)throw new InvalidOperationException();return ValueTask.CompletedTask;}
    }
    static object Invoke(object target,string name,params object[] args)=>target.GetType().GetMethod(name,BindingFlags.Public|BindingFlags.NonPublic|BindingFlags.Instance).Invoke(target,args);
    delegate float[] Parse(ReadOnlySpan<byte> bytes);
    static string Floats(float[] value)=>Convert.ToHexString(MemoryMarshal.AsBytes(value.AsSpan())).ToLowerInvariant();
    static AudioSettings Settings()=>new("capture-fixture","render-fixture","microphone",20,10,60);
    static object Snapshot(CallAudioPort port,Route route)=>new{state=port.Status.State,open=port.IsOpen,failed=port.Failed,route=route.Mode,
        resumes=route.Resumes,suspends=route.Suspends,disposals=route.Disposals,reads=route.Reads,writes=route.Writes};
    static async Task<object> Lifetime(JsonElement input){
        var time=new Time();var permit=new AudioPermit(time);var route=new Route();
        var port=new CallAudioPort("11111111-1111-4111-8111-111111111111",permit,(_,_,allowed,_)=>{route.Allowed=allowed;return Task.FromResult<ICallAudioRoute>(route);});
        var rows=new List<object>();
        foreach(var action in input.GetProperty("actions").EnumerateArray()){
            string name=action.GetString();bool ok=true;
            try{switch(name){
                case "prepare":await port.PrepareAsync(Settings(),null);break;
                case "bind":Invoke(port,"BindAuthority","capture-fixture");break;
                case "grant":Invoke(port,"GrantAuthority");break;
                case "renew":Invoke(port,"RefreshAuthority",time.Ticks);break;
                case "expire":time.Ticks+=500;break;
                case "activate":port.Activate();break;
                case "suspend":port.Suspend();break;
                case "clear":Invoke(port,"ClearAuthority");break;
                case "release":Invoke(port,"ReleaseVoiceAuthority");break;
                case "arm":Invoke(port,"ArmVoiceRecovery");break;
                case "fail":route.Mode="failed";break;
                case "provider-open":route.Mode="open";break;
                case "capture":port.ReadCaptured(new float[32]);break;
                case "write":port.WriteReceived(new float[32]);break;
                case "bad-close":route.BadClose=true;break;
                case "bad-suspend":route.BadSuspend=true;break;
                case "revoke":port.Revoke();break;
                case "close":await port.DisposeAsync();break;
                default:throw new ArgumentException();
            }}catch{ok=false;}
            rows.Add(new{action=name,ok,snapshot=Snapshot(port,route)});
        }
        try{await port.DisposeAsync();}catch{}
        return new{ok=true,trace=rows};
    }
    static async Task<object> Waiting(JsonElement input){
        var permit=new AudioPermit();var route=new Route();
        var normal=new CallAudioPort("11111111-1111-4111-8111-111111111111",permit,(_,_,allowed,_)=>{route.Allowed=allowed;return Task.FromResult<ICallAudioRoute>(route);});
        await normal.PrepareAsync(Settings(),null);Invoke(normal,"GrantAuthority");
        var type=typeof(CallAudioPort).Assembly.GetType("Ivy.PhoneBridge.CallWaitingAudio");
        var waiting=Activator.CreateInstance(type,BindingFlags.Instance|BindingFlags.Public|BindingFlags.NonPublic,null,new object[]{normal},null);
        var audio=(IPcmAudioPort)waiting;var rows=new List<object>();
        foreach(var action in input.GetProperty("actions").EnumerateArray()){
            string name=action.GetProperty("op").GetString();bool ok=true;float[] output=Array.Empty<float>();
            try{switch(name){case "begin":Invoke(waiting,"Begin");break;case "end":Invoke(waiting,"End");break;
                case "success":Invoke(waiting,"Acknowledge",true);break;case "failure":Invoke(waiting,"Acknowledge",false);break;
                case "suspend":normal.Suspend();break;case "activate":normal.Activate();break;case "revoke":normal.Revoke();break;
                case "read":output=new float[action.GetProperty("count").GetInt32()];audio.ReadCaptured(output);break;
                case "write":audio.WriteReceived(new float[32]);break;default:throw new ArgumentException();}
            }catch{ok=false;}
            rows.Add(new{action=name,ok,open=audio.IsOpen,reads=route.Reads,writes=route.Writes,pcm=Floats(output)});
        }
        await normal.DisposeAsync();return new{ok=true,trace=rows};
    }
    static object Wave(JsonElement input){
        var type=typeof(CallAudioPort).Assembly.GetType("Ivy.PhoneBridge.ScreeningAudio");
        var method=type.GetMethod("Parse",BindingFlags.Static|BindingFlags.NonPublic|BindingFlags.Public);
        var parse=(Parse)method.CreateDelegate(typeof(Parse));
        var samples=parse(Convert.FromHexString(input.GetProperty("hex").GetString()));
        return new{ok=true,pcm=Floats(samples)};
    }
    static async Task RunAsync(){using var input=JsonDocument.Parse(Console.In.ReadToEnd());var rows=new List<object>();
        foreach(var item in input.RootElement.EnumerateArray())try{string op=item.GetProperty("op").GetString();rows.Add(op=="wave"?Wave(item):op=="waiting"?await Waiting(item):await Lifetime(item));}
        catch(Exception error){rows.Add(new{ok=false,error=error.GetType().Name});}
        Console.Write(JsonSerializer.Serialize(rows));
    }
    public static void Run()=>RunAsync().GetAwaiter().GetResult();
}
