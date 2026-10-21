$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
function Slice([string]$text,[string]$start,[string]$end) {
 $a=$text.IndexOf($start,[StringComparison]::Ordinal);$b=$text.IndexOf($end,$a,[StringComparison]::Ordinal)
 if($a -lt 0 -or $b -lt 0){throw 'Original input owner boundary unavailable'}
 return $text.Substring($a,$b-$a)
}
$native=Join-Path $PSScriptRoot '../../services/phone-bridge/native/phone-runtime'
$definition=@'
using System;using System.IO;using System.Linq;using System.Text;using System.Text.Json;using System.Collections.Generic;using System.Threading;using System.Threading.Tasks;using System.Diagnostics;using System.Text.RegularExpressions;
'@ + "`nnamespace Ivy.PhoneBridge {`n"
$source=[IO.File]::ReadAllText((Join-Path $native 'DesktopVoiceInput.cs'))
# Only record storage is needed by these original owner methods. Read/Snapshot,
# hotkey planning and dispatch are covered by the separate unchanged oracle.
$header=Slice $source 'public sealed record NativeVoiceInput' '    internal static NativeVoiceInput Read('
$definition+=$header.Substring(0,$header.LastIndexOf('{'))+";`n"
$definition+="public sealed record NativeHotkey(string[] Modifiers,int KeyCode,bool FocusBeforeHotkey);`n"
$definition+=(Slice $source 'internal sealed record VoiceInputPlan' 'public sealed partial class PhoneDesktopRuntime')
$manager=(Slice $source 'public sealed partial class PhoneDesktopRuntime' '    internal static VoiceInputPlan FallbackInput()')
$manager=$manager.Replace('Environment.TickCount64','VoiceInputOwnerOracle.TickCount64').Replace('Stopwatch.StartNew()','VoiceInputOwnerOracle.Start()').Replace('Thread.Sleep(50)','VoiceInputOwnerOracle.Sleep(50)')
$tail=$source.Substring($source.IndexOf('    public ValueTask DisposeAsync()',[StringComparison]::Ordinal))
$definition+=$manager+"    public PhoneDesktopRuntime(Func<MicroSettings,IMicroVoiceInput> factory){microFactory=factory;}`n"+$tail+"`n"
foreach($pair in @(@('MicroUsbClient.cs','public sealed record MicroSettings','public sealed record MicroClientResult'),@('MicroAttachment.cs','public sealed record MicroAttachmentStatus','//'),@('MicroUsbSession.cs','public sealed record MicroInputReceipt','//'))) {
 $source=[IO.File]::ReadAllText((Join-Path $native $pair[0]));$definition+=(Slice $source $pair[1] $pair[2])+"`n"
}
$source=[IO.File]::ReadAllText((Join-Path $native 'MicroVoiceGesture.cs'))
$definition+=(Slice $source 'public interface IMicroVoiceInput' 'public sealed record MicroGestureResult')+"`n"
$definition+=@'
public static class VoiceInputOwnerOracle {
 static readonly JsonSerializerOptions Json=new(){PropertyNamingPolicy=JsonNamingPolicy.CamelCase};
 static JsonElement Row;static List<string> Trace;static PhoneDesktopRuntime Owner;static int factories,statuses,originals,sleeps,closes;static long tick,now;static Task disposal;static TaskCompletionSource<bool> pending;
 static T[] Values<T>(string name)=>JsonSerializer.Deserialize<T[]>(Row.GetProperty(name).GetRawText(),Json);
 static T Pick<T>(T[] values,int index)=>values[Math.Min(index,values.Length-1)];
 public static long TickCount64{get{Trace.Add("tick:"+tick);return tick;}}
 public sealed class Timer(long began){public TimeSpan Elapsed{get{Trace.Add("elapsed:"+(now-began));return TimeSpan.FromMilliseconds(now-began);}}}
 public static Timer Start(){Trace.Add("start:"+now);return new(now);}
 public static void Sleep(int ms){Trace.Add("sleep:"+ms);int i=sleeps++;if(Row.GetProperty("sleepFault").GetInt32()==i)throw new IOException("fixture");tick+=ms;now+=ms;}
 static bool Original(){int i=originals++;Trace.Add("original:"+i);if(Row.GetProperty("originalFault").GetInt32()==i)throw new IOException("fixture");return Pick(Values<bool>("originals"),i);}
 static NativeVoiceInput Settings(int which,bool fallback){var m=which==0?null:new MicroSettings("C:/fixture/usbip.exe","sha256:"+new string('a',64),1023+which);return new(m,fallback?new(["control"],32,false):null);}
 static void Reenter(string where,int index){if(Row.GetProperty(where+"ReenterAt").GetInt32()!=index)return;string action=Row.GetProperty(where+"Reenter").GetString();if(action=="none")return;Trace.Add(where+".reenter:"+action);if(action=="status")Owner.MicroStatus.ToString();else if(action=="close")Owner.DisposeAsync().AsTask().GetAwaiter().GetResult();else Owner.SelectVoiceInput(Settings(1,false),false,Original);}
 sealed class Device(int id):IMicroVoiceInput {
  public MicroAttachmentStatus Status{get{int i=statuses++;Trace.Add("status:"+id+":"+i);var item=Pick(Values<JsonElement>("statuses"),i);long delta=item.GetProperty("delta").GetInt64();tick+=delta;now+=delta;Reenter("status",i);if(item.GetProperty("fault").GetBoolean())throw new IOException("fixture");return new(item.GetProperty("state").GetString(),item.GetProperty("generation").ValueKind==JsonValueKind.Null?null:item.GetProperty("generation").GetInt64(),null);}}
  public MicroInputReceipt SendMicrophone(bool down,long generation,Func<bool> original)=>throw new Exception("unexpected input");
  public ValueTask DisposeAsync(){int i=closes++;Trace.Add("close:"+id+":"+i);Reenter("close",i);string mode=Pick(Values<string>("closeReturns"),i);if(mode=="throw")throw new IOException("fixture");if(mode=="failed")return new(Task.FromException(new IOException("fixture")));if(mode=="pending"){pending=new(TaskCreationOptions.RunContinuationsAsynchronously);return new(pending.Task);}return ValueTask.CompletedTask;}
 }
 static IMicroVoiceInput Factory(MicroSettings settings){int i=factories++;Trace.Add("factory:"+i+":"+settings.Port);Reenter("factory",i);tick+=Row.GetProperty("factoryDelta").GetInt64();now+=Row.GetProperty("factoryDelta").GetInt64();return Pick(Values<string>("factories"),i) switch{"device"=>new Device(i+1),"null"=>null,"io"=>throw new IOException("fixture"),"access"=>throw new UnauthorizedAccessException("fixture"),"socket"=>throw new System.Net.Sockets.SocketException(10048),"argument"=>throw new ArgumentException("fixture"),_=>throw new Exception("fixture")};}
 static object Closed(Task task)=>new{same=disposal!=null&&ReferenceEquals(disposal,task),finished=task.IsCompleted,error=task.IsFaulted||task.IsCanceled?"fixture":null};
 static object Capture(Func<object> work){try{return new{ok=true,value=work()};}catch(ArgumentException){return new{ok=false,error="invalid_request"};}catch(InvalidOperationException e){return new{ok=false,error=e.Message};}catch{return new{ok=false,error="fixture"};}}
 static object Script(JsonElement row){Row=row;Trace=new();factories=statuses=originals=sleeps=closes=0;tick=10000;now=0;disposal=null;pending=null;Owner=new(Factory);var results=new List<object>();foreach(var step in row.GetProperty("steps").EnumerateArray()){string action=step.GetProperty("action").GetString();results.Add(Capture(()=>{switch(action){case "select":var p=Owner.SelectVoiceInput(Settings(step.GetProperty("micro").GetInt32(),step.GetProperty("fallback").GetBoolean()),step.GetProperty("wait").GetBoolean(),Original);return new{method=p.Method,generation=p.Generation,device=p.Device!=null};case "prepare":Owner.PrepareMicro(Settings(1,false).Micro);return true;case "status":return Owner.MicroStatus;case "time":tick=step.GetProperty("tick").GetInt64();now=step.GetProperty("now").GetInt64();return true;case "complete":pending.TrySetResult(true);disposal.GetAwaiter().GetResult();return true;default:var close=Owner.DisposeAsync().AsTask();var observed=Closed(close);disposal=close;return observed;}}));}return new{results,trace=Trace};}
 sealed class ConcurrentDevice:IMicroVoiceInput {public int Closes;public MicroAttachmentStatus Status=>new("ready",1,null);public MicroInputReceipt SendMicrophone(bool down,long gen,Func<bool> original)=>throw new Exception("unexpected input");public ValueTask DisposeAsync(){Interlocked.Increment(ref Closes);return ValueTask.CompletedTask;}}
 static object Concurrent(){var previous=Trace;Trace=new();using var entered=new ManualResetEventSlim();using var release=new ManualResetEventSlim();using var closeEntered=new ManualResetEventSlim();int calls=0;var device=new ConcurrentDevice();var runtime=new PhoneDesktopRuntime(_=>{Interlocked.Increment(ref calls);entered.Set();if(!release.Wait(2000))throw new Exception("factory fixture bound");return device;});var selecting=Task.Run(()=>runtime.SelectVoiceInput(Settings(1,false),false,()=>true));if(!entered.Wait(2000))throw new Exception("factory did not enter");var closing=Task.Factory.StartNew(()=>{closeEntered.Set();return runtime.DisposeAsync().AsTask();},CancellationToken.None,TaskCreationOptions.DenyChildAttach,TaskScheduler.Default);if(!closeEntered.Wait(2000))throw new Exception("close did not enter");bool early=closing.Wait(50);release.Set();if(!Task.WaitAll([selecting,closing],2000))throw new Exception("owner did not drain");var first=closing.GetAwaiter().GetResult();var last=runtime.DisposeAsync().AsTask();Trace=previous;return new{factoryCalls=calls,closeCalls=device.Closes,closeBeforeFactoryReleased=early,selectReady=selecting.Result.Generation==1,closeFinished=first.IsCompleted,sameWork=ReferenceEquals(first,last)};}
 public static string Run(string text){using var doc=JsonDocument.Parse(text);return JsonSerializer.Serialize(new{inputSha256=Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(Encoding.UTF8.GetBytes(text))),scripts=doc.RootElement.EnumerateArray().Select(Script).ToArray(),serialization=Concurrent(),actualTickCount64=Environment.TickCount64},Json);}
}
}
'@
Add-Type -TypeDefinition $definition
try{[Console]::Out.Write([Ivy.PhoneBridge.VoiceInputOwnerOracle]::Run([Console]::In.ReadToEnd()))}
catch{[Console]::Error.WriteLine($_.Exception.ToString());exit 1}
