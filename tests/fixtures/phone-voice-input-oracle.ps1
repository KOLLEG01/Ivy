$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$definition=@'
using System;using System.IO;using System.Linq;using System.Text;using System.Text.Json;using System.Text.Json.Serialization;using System.Collections.Generic;using System.Threading;using System.Threading.Tasks;using System.Diagnostics;using System.Globalization;using System.Runtime.InteropServices;using System.Text.RegularExpressions;
'@ + "`n"
function Slice([string]$text,[string]$start,[string]$end) {
 $a=$text.IndexOf($start,[StringComparison]::Ordinal)
 $b=$text.IndexOf($end,$a,[StringComparison]::Ordinal)
 if($a -lt 0 -or $b -lt 0){throw 'Original policy boundary unavailable'}
 return $text.Substring($a,$b-$a)
}
$native=Join-Path $PSScriptRoot '../../services/phone-bridge/native'
foreach($name in @('DesktopLaunch.cs','VoiceHotkey.cs')) {
 $source=[IO.File]::ReadAllText((Join-Path $native "input/$name"))
 $at=$source.IndexOf('namespace Ivy.PhoneBridge {',[StringComparison]::Ordinal)
 if($at -lt 0){throw 'Original input namespace unavailable'}
 $definition+=$source.Substring($at)+"`n"
}
$source=[IO.File]::ReadAllText((Join-Path $native 'phone-runtime/MicroVoiceGesture.cs'))
$source=$source.Replace('pause ??= Thread.Sleep;','pause ??= VoiceInputOracle.Pause;')
$marker='namespace Ivy.PhoneBridge;'
$definition+="namespace Ivy.PhoneBridge {`n"+$source.Substring($source.IndexOf($marker)+$marker.Length)+"`n}`n"
$source=[IO.File]::ReadAllText((Join-Path $native 'phone-runtime/MicroUsbClient.cs'))
$definition+="namespace Ivy.PhoneBridge {`n"+(Slice $source 'public sealed record MicroSettings' 'public sealed record MicroClientResult')+"`n}`n"
$source=[IO.File]::ReadAllText((Join-Path $native 'phone-runtime/DesktopVoiceInput.cs'))
$definition+="namespace Ivy.PhoneBridge {`n"+(Slice $source 'public sealed record NativeVoiceInput' 'public sealed partial class PhoneDesktopRuntime')+"`npublic sealed partial class PhoneDesktopRuntime {`n"+(Slice $source '    internal static VoiceInputPlan FallbackInput()' '    public ValueTask DisposeAsync()')+"`n}}`n"
$source=[IO.File]::ReadAllText((Join-Path $native 'phone-runtime/NativeSipOperations.cs'))
$definition+="namespace Ivy.PhoneBridge { public static class NativeSipOperations {`n"+(Slice $source '    internal static T Read<T>' '    private SipCall Original(')+"`n}}`n"
$source=[IO.File]::ReadAllText((Join-Path $native 'phone-runtime/CallDesktopSession.cs'))
$definition+="namespace Ivy.PhoneBridge {`n"+(Slice $source 'public sealed record NativeHotkey' '// Only OS primitives.')+"`n}`n"
foreach($pair in @(@('MicroAttachment.cs','public sealed record MicroAttachmentStatus','//'),@('MicroUsbSession.cs','public sealed record MicroInputReceipt','//'))) {
 $source=[IO.File]::ReadAllText((Join-Path $native ('phone-runtime/'+$pair[0])))
 $definition+="namespace Ivy.PhoneBridge {`n"+(Slice $source $pair[1] $pair[2])+"`n}`n"
}
$source=[IO.File]::ReadAllText((Join-Path $native 'phone-runtime/NativeRpc.cs'))
$definition+="namespace Ivy.PhoneBridge { public static class NativeRpc {`n"+(Slice $source '    public static readonly JsonSerializerOptions Json' '    private sealed record Receipt')+"`n}}`n"
$definition+=@'
namespace Ivy.PhoneBridge {
public sealed partial class PhoneDesktopRuntime {
 internal bool IsCurrent(DesktopIdentity desktop)=>VoiceInputOracle.Step("current");
 internal bool PrepareVoiceFocus(DesktopIdentity desktop,Func<bool> original){VoiceInputOracle.Trace.Add("focus");if(VoiceInputOracle.Row.GetProperty("focusFault").GetBoolean())throw new IOException("fixture");return VoiceInputOracle.Row.GetProperty("focusOk").GetBoolean();}
 internal HotkeyResult VoiceToggle(DesktopIdentity desktop,NativeHotkey hotkey,Func<bool> before,bool focusAlreadyPrepared=false){VoiceInputOracle.Trace.Add("toggle:"+hotkey.KeyCode+":"+(hotkey.FocusBeforeHotkey?"true":"false")+":"+(focusAlreadyPrepared?"true":"false"));if(VoiceInputOracle.Row.GetProperty("toggleFault").GetBoolean())throw new IOException("fixture");string phase=VoiceInputOracle.Row.GetProperty("hotkeyPhase").GetString();if(VoiceInputOracle.Row.GetProperty("toggleChecks").GetBoolean()&&!before())phase="not_submitted";return new(phase,4,phase=="submitted"?4:0,phase=="submitted",null);}
}
public static class VoiceInputOracle {
 public static List<string> Trace;public static JsonElement Row;static Dictionary<string,int> Counts;static int sends,pauses;
 public static bool Step(string name){Trace.Add(name);int i=Counts.GetValueOrDefault(name);Counts[name]=i+1;if(Row.GetProperty(name+"Fault").GetInt32()==i)throw new IOException("fixture");var v=Row.GetProperty(name).EnumerateArray().Select(v=>v.GetBoolean()).ToArray();return v[Math.Min(i,v.Length-1)];}
 public static void Pause(int ms){Trace.Add("pause:"+ms);int i=pauses++;if(Row.GetProperty("pauseFault").GetInt32()==i)throw new IOException("fixture pause");}
 sealed class Input:IMicroVoiceInput {
  public MicroAttachmentStatus Status=>throw new Exception("unexpected status read");
  public MicroInputReceipt SendMicrophone(bool pressed,long generation,Func<bool> original){int i=sends++;Trace.Add("send:"+i+":"+(pressed?"down":"up")+":"+generation);var phases=Row.GetProperty("phases").EnumerateArray().Select(v=>v.GetString()).ToArray();string phase=phases[Math.Min(i,phases.Length-1)];if(phase is "throw" or "panic")throw new IOException("fixture send");var live=Row.GetProperty("live").EnumerateArray().Select(v=>v.GetInt64()).ToArray();if(live[Math.Min(i,live.Length-1)]!=generation||!original())phase="not_submitted";return new(phase,pressed,generation);}
  public ValueTask DisposeAsync()=>throw new Exception("unexpected dispose");
 }
 static void Reset(JsonElement row){Row=row;Trace=new();Counts=new();sends=pauses=0;}
 static object Capture(Func<object> action,bool dto=false){try{return new{ok=true,value=action()};}catch(ArgumentException e){return new{ok=false,error=dto?"invalid_request":e.Message.Split('\n')[0].TrimEnd('\r')};}catch(JsonException){return new{ok=false,error="invalid_request"};}catch(InvalidOperationException e){return new{ok=false,error=dto?"operation_outcome_unknown":e.Message};}catch{return new{ok=false,error="fixture"};}}
 static NativeVoiceInput Settings(JsonElement row)=>new(null,row.GetProperty("fallback").GetBoolean()?new(["control"],32,row.GetProperty("focusBefore").GetBoolean()):null);
 static VoiceInputPlan Plan(JsonElement row)=>row.GetProperty("method").GetString()=="hotkey"?PhoneDesktopRuntime.FallbackInput():new("micro",row.GetProperty("generation").ValueKind==JsonValueKind.Null?null:row.GetProperty("generation").GetInt64(),row.GetProperty("device").GetBoolean()?new Input():null);
 static object Gesture(JsonElement row){Reset(row);var result=Capture(()=>MicroVoiceGesture.Dispatch(new Input(),row.GetProperty("generation").GetInt64(),row.GetProperty("cleanup").GetBoolean(),()=>Step("before"),()=>Step("release"),Pause));return new{result,trace=Trace};}
 static object Focus(JsonElement row){Reset(row);var result=Capture(()=>new PhoneDesktopRuntime().PrepareInputFocus(Plan(row),Settings(row),null,()=>Step("before")));return new{result,trace=Trace};}
 static object Dispatch(JsonElement row){Reset(row);MicroGestureResult prior=null;string phase=row.GetProperty("prior").GetString();if(phase!=null)prior=new(phase,new(phase,true,1),null);var result=Capture(()=>new PhoneDesktopRuntime().DispatchVoice(Plan(row),Settings(row),null,row.GetProperty("cleanup").GetBoolean(),()=>Step("before"),()=>Step("release"),prior,row.GetProperty("dictation").GetBoolean()));return new{result,trace=Trace};}
 static object Read(string raw)=>Capture(()=>{using var doc=JsonDocument.Parse(raw);return NativeVoiceInput.Read(doc.RootElement);},true);
 static object Matches(string[] values){var inputs=values.Select(raw=>{using var doc=JsonDocument.Parse(raw);return NativeVoiceInput.Read(doc.RootElement);}).ToArray();var matches=new List<bool>();foreach(var a in inputs){matches.Add(a.Matches(null));foreach(var b in inputs)matches.Add(a.Matches(b));}var modifiers=new[]{"control","alt"};var snapshot=new NativeVoiceInput(null,new(modifiers,32,false)).Snapshot();modifiers[0]="shift";return new{matches,snapshotStable=snapshot.HotkeyFallback.Modifiers.SequenceEqual(new[]{"control","alt"}),observed=((IMicroVoiceInput)new Input()).ObserveVoice(9007199254740991)};}
 public static string Run(string text){using var doc=JsonDocument.Parse(text);var r=doc.RootElement;return JsonSerializer.Serialize(new{inputSha256=Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(Encoding.UTF8.GetBytes(text))),settings=r.GetProperty("settings").EnumerateArray().Select(v=>Read(v.GetString())).ToArray(),matches=Matches(r.GetProperty("matching").EnumerateArray().Select(v=>v.GetString()).ToArray()),gestures=r.GetProperty("gestures").EnumerateArray().Select(Gesture).ToArray(),focus=r.GetProperty("focus").EnumerateArray().Select(Focus).ToArray(),dispatch=r.GetProperty("dispatch").EnumerateArray().Select(Dispatch).ToArray()},NativeRpc.Json);}
}}
'@
Add-Type -TypeDefinition $definition
try{[Console]::Out.Write([Ivy.PhoneBridge.VoiceInputOracle]::Run([Console]::In.ReadToEnd()))}
catch{[Console]::Error.WriteLine($_.Exception.ToString());exit 1}
