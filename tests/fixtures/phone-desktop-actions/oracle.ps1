$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$definition=@'
using System;using System.IO;using System.Linq;using System.Text;using System.Text.Json;using System.Collections.Generic;using System.Threading;using System.Threading.Tasks;using System.Diagnostics;using System.Globalization;using System.Runtime.InteropServices;using System.Security.Principal;using System.Text.RegularExpressions;
'@ + "`n"
foreach($name in @('DesktopLaunch.cs','VoiceHotkey.cs')) {
 $source=[IO.File]::ReadAllText((Join-Path $PSScriptRoot "../../../services/phone-bridge/native/input/$name"))
 if($name -eq 'VoiceHotkey.cs') {
  # Only the two keyboard P/Invoke declarations target the owned SDK fixture.
  $send='[DllImport("user32.dll", SetLastError = true)] static extern uint SendInput'
  $keys='[DllImport("user32.dll")] static extern short GetAsyncKeyState'
  if($source.IndexOf($send) -lt 0 -or $source.IndexOf($keys) -lt 0){throw 'Original keyboard I/O seam changed'}
  $source=$source.Replace($send,'[DllImport("phone_desktop_actions_fixture.dll", EntryPoint="FixtureSendInput")] static extern uint SendInput').Replace($keys,'[DllImport("phone_desktop_actions_fixture.dll", EntryPoint="FixtureKeyState")] static extern short GetAsyncKeyState')
 }
 $at=$source.IndexOf('namespace Ivy.PhoneBridge {',[StringComparison]::Ordinal);if($at -lt 0){throw 'Original namespace unavailable'}
 $definition+=$source.Substring($at)+"`n"
}
foreach($name in @('DesktopControls.cs','DesktopVoiceControls.cs','DesktopVoiceSession.cs')) {
 $source=[IO.File]::ReadAllText((Join-Path $PSScriptRoot "../../../services/phone-bridge/native/phone-runtime/$name"));$marker='namespace Ivy.PhoneBridge;';$at=$source.IndexOf($marker,[StringComparison]::Ordinal)
 if($at -lt 0){throw 'Original control namespace unavailable'}
 $definition+="namespace Ivy.PhoneBridge {`n"+$source.Substring($at+$marker.Length)+"`n}`n"
}
$runtime=[IO.File]::ReadAllText((Join-Path $PSScriptRoot '../../../services/phone-bridge/native/phone-runtime/CallDesktopSession.cs'))
$start=$runtime.IndexOf('private sealed class CurrentApplication(',[StringComparison]::Ordinal);$end=$runtime.IndexOf('internal object Launch(', $start,[StringComparison]::Ordinal)
if($start -lt 0 -or $end -lt 0){throw 'Original owner wrappers unavailable'}
$wrappers=$runtime.Substring($start,$end-$start)
$source=[IO.File]::ReadAllText((Join-Path $PSScriptRoot '../../../services/phone-bridge/native/phone-runtime/WindowsDesktopControls.cs'))
$start=$source.IndexOf('public sealed record DesktopReadStage',[StringComparison]::Ordinal);$end=$source.IndexOf('    delegate bool WindowCallback(', $start,[StringComparison]::Ordinal)
if($start -lt 0 -or $end -lt 0){throw 'Original outer control policy unavailable'}
$definition+="namespace FixtureControlPolicy { using Ivy.PhoneBridge; using DesktopIdentity=FixtureControlPolicy.Identity;`n"+$source.Substring($start,$end-$start)+@'
    sealed class Source : IDesktopVoiceSource {
        public Source(int pid,bool retainVoice=false,Action<DesktopReadStage> onReadStage=null){State.Log("open");}
        public void Prepare(string[] labels){State.Log("prepare");}
        public DesktopControlWindow[] Read(string[] labels){State.Log("read:"+string.Join('|',labels));return State.Windows;}
        public DesktopControlWindow[] Refresh(VoiceControlIdentity identity)=>throw new Exception("unused");
        public DesktopInvokeResult Invoke(string window,DesktopControl control,Func<bool> current){State.Log("invoke:"+window+":"+string.Join(',',control.Id));for(int i=0;i<State.Checks;i++)if(!current())return new("not_submitted");return new(State.Phase);}
        public void Dispose(){State.Log("close");}
    }
}
public sealed class Identity {public int pid=123;public Owner Owner()=>new();}
public sealed class Owner {public bool IsCurrent()=>State.Owner();}
public static class State {
 public static List<string> Trace=new();public static DesktopControlWindow[] Windows;public static string Fault,Phase;public static int Checks,CurrentCount,OwnerCount;public static bool[] Currents,Owners;public static int CurrentFault,OwnerFault;
 public static void Log(string value){Trace.Add(value);if(Fault==value||Fault=="read"&&value.StartsWith("read:"))throw new Exception("fixture");}
 public static bool Current(){Trace.Add("current");int i=CurrentCount++;if(i==CurrentFault)throw new Exception("fixture");return Currents[Math.Min(i,Currents.Length-1)];}
 public static bool Owner(){Trace.Add("owner");int i=OwnerCount++;if(i==OwnerFault)throw new Exception("fixture");return Owners[Math.Min(i,Owners.Length-1)];}
}
}
'@
$definition+="namespace Ivy.PhoneBridge { public static class DesktopActionsOracle {`n"+$wrappers+@'
static readonly JsonSerializerOptions Json=new(){PropertyNamingPolicy=JsonNamingPolicy.CamelCase};
[DllImport("phone_desktop_actions_fixture.dll")]static extern void FixtureReset(int key,int state,uint first,uint second);
[DllImport("phone_desktop_actions_fixture.dll")]static extern IntPtr FixtureTrace();
static T Decode<T>(JsonElement value)=>JsonSerializer.Deserialize<T>(value.GetRawText(),Json);
static object Identity(DesktopIdentity v)=>v==null?null:new{v.pid,v.startTimeUtcTicks,v.imagePath,v.appUserModelId};
static object Capture(Func<object> f){try{return new{ok=true,value=f()};}catch(ArgumentException e){return new{ok=false,error=e.Message.Split('\n')[0].TrimEnd('\r')};}catch{return new{ok=false,error="fixture"};}}
static DesktopIdentity Target(string path,string application="Fixture.Package!UI")=>new(123,1,path,application);
sealed class App(JsonElement row,List<string> trace,DesktopIdentity identity):IDesktopApplication {
 public DesktopObservation Observe(){trace.Add("observe");if(row.GetProperty("fault").GetString()=="observe")throw new Exception("fixture");string state=row.GetProperty("state").GetString();return state switch{"null"=>null,"unavailable"=>DesktopObservation.Unavailable(),"ambiguous"=>DesktopObservation.Select(true,[identity,identity]),"ready"=>DesktopObservation.Select(true,[identity]),"waiting"=>DesktopObservation.Select(true,[]),_=>DesktopObservation.Select(false,[])};}
 public int Activate(){trace.Add("activate");if(row.GetProperty("fault").GetString()=="activate")throw new Exception("fixture");return row.GetProperty("pid").GetInt32();}
}
sealed class Owner(JsonElement row,List<string> trace):IDesktopOwner {
 int calls;bool Step(string method,bool result){trace.Add(method);if(row.GetProperty("fault").GetString()==method)throw new Exception("fixture");return result;}
 public bool IsCurrent(){var values=Decode<bool[]>(row.GetProperty("owners"));return Step("owner",values[Math.Min(calls++,values.Length-1)]);}
 public bool TryFocus()=>Step("focus",row.GetProperty("focusOk").GetBoolean());
 public bool IsFocused()=>Step("focused",row.GetProperty("focused").GetBoolean());
}
sealed class Keyboard(JsonElement row,List<string> trace):IKeyboardInput {
 int calls;public bool IsIdle(){trace.Add("idle");if(row.GetProperty("fault").GetString()=="idle")throw new Exception("fixture");return row.GetProperty("idle").GetBoolean();}
 public int Send(KeyStroke[] keys){trace.Add("send:"+string.Join(',',keys.Select(k=>k.Key+":"+(k.Up?"up":"down"))));int i=calls++;if(row.GetProperty("fault").GetString()==(i==0?"send":"release"))throw new Exception("fixture");return row.GetProperty(i==0?"sent":"released").GetInt32();}
}
static object Plans(JsonElement row)=>Capture(()=>VoiceHotkey.Plan(Decode<string[]>(row.GetProperty("modifiers")),row.GetProperty("key").GetInt32()).Select(k=>new{key=k.Key,up=k.Up}).ToArray());
static object Chord(JsonElement row){var trace=new List<string>();var owner=new Owner(row,trace);int current=0;bool Current(){trace.Add("current");int i=current++;if(i==row.GetProperty("currentFault").GetInt32())throw new Exception("fixture");var v=Decode<bool[]>(row.GetProperty("currents"));return v[Math.Min(i,v.Length-1)];}
 var result=Capture(()=>VoiceHotkey.Dispatch(row.GetProperty("wrapped").GetBoolean()?new CurrentOwner(owner,Current):owner,new Keyboard(row,trace),Decode<string[]>(row.GetProperty("modifiers")),row.GetProperty("key").GetInt32(),row.GetProperty("focusBefore").GetBoolean(),row.GetProperty("prepared").GetBoolean()));return new{result,trace};}
static object Launch(JsonElement row,string path){var trace=new List<string>();int current=0;bool Current(){trace.Add("current");int i=current++;if(i==row.GetProperty("currentFault").GetInt32())throw new Exception("fixture");var v=Decode<bool[]>(row.GetProperty("currents"));return v[Math.Min(i,v.Length-1)];}
 var app=new App(row,trace,Target(path));var result=Capture(()=>{var r=DesktopLaunch.Dispatch(row.GetProperty("wrapped").GetBoolean()?new CurrentApplication(app,Current):app,row.GetProperty("start").GetBoolean());return new{r.phase,identity=Identity(r.identity),r.activationPid,r.errorCode};});return new{result,trace};}
static object Focus(JsonElement row){var trace=new List<string>();var calls=new Dictionary<string,int>();bool Step(string key){trace.Add(key);int i=calls.GetValueOrDefault(key);calls[key]=i+1;if(row.GetProperty("fault").GetString()==key)throw new Exception("fixture");var values=Decode<bool[]>(row.GetProperty(key));return values[Math.Min(i,values.Length-1)];}
 var result=Capture(()=>DesktopProcessOwner.ConfirmActivation(()=>Step("current"),()=>Step("foreground"),()=>Step("restore"),()=>Step("activate"),()=>Step("processed")));return new{result,trace};}
static object Invoke(JsonElement row){var s=typeof(FixtureControlPolicy.State);FixtureControlPolicy.State.Trace=new();FixtureControlPolicy.State.Windows=Decode<DesktopControlWindow[]>(row.GetProperty("windows"));FixtureControlPolicy.State.Fault=row.GetProperty("fault").GetString();FixtureControlPolicy.State.Phase=row.GetProperty("phase").GetString();FixtureControlPolicy.State.Checks=row.GetProperty("checks").GetInt32();FixtureControlPolicy.State.Currents=Decode<bool[]>(row.GetProperty("currents"));FixtureControlPolicy.State.Owners=Decode<bool[]>(row.GetProperty("owners"));FixtureControlPolicy.State.CurrentCount=FixtureControlPolicy.State.OwnerCount=0;FixtureControlPolicy.State.CurrentFault=row.GetProperty("currentFault").GetInt32();FixtureControlPolicy.State.OwnerFault=row.GetProperty("ownerFault").GetInt32();var result=Capture(()=>FixtureControlPolicy.WindowsDesktopControls.InvokeVoice(new(),row.GetProperty("stop").GetBoolean(),FixtureControlPolicy.State.Current).Phase);return new{result,trace=FixtureControlPolicy.State.Trace};}
static object Native(JsonElement row){FixtureReset(row.GetProperty("pressed").GetInt32(),row.GetProperty("word").GetInt32(),row.GetProperty("sent").GetUInt32(),row.GetProperty("released").GetUInt32());var keyboard=new WindowsKeyboardInput();var result=Capture(()=>row.GetProperty("kind").GetString()=="idle"?(object)keyboard.IsIdle():keyboard.Send(Decode<KeyStrokeInput[]>(row.GetProperty("keys")).Select(k=>new KeyStroke(k.Key,k.Up)).ToArray()));return new{result,trace=Marshal.PtrToStringUTF8(FixtureTrace())};}
sealed record KeyStrokeInput(ushort Key,bool Up);
sealed class ReentrantApp(bool inner,List<string> trace):IDesktopApplication {
 public DesktopObservation Observe(){trace.Add(inner?"inner.observe":"outer.observe");if(!inner){var r=DesktopLaunch.Dispatch(new ReentrantApp(true,trace),false);trace.Add("nested_launch:"+r.phase);}return DesktopObservation.Select(false,[]);}
 public int Activate(){trace.Add("outer.activate");return 123;}
}
sealed class AlwaysOwner:IDesktopOwner {public bool IsCurrent()=>true;public bool TryFocus()=>true;public bool IsFocused()=>true;}
sealed class ReentrantKeyboard(bool inner,List<string> trace):IKeyboardInput {
 public bool IsIdle()=>true;
 public int Send(KeyStroke[] keys){trace.Add((inner?"inner.send:":"outer.send:")+keys.Length);if(!inner){var r=VoiceHotkey.Dispatch(new AlwaysOwner(),new ReentrantKeyboard(true,trace),["control"],32);trace.Add("nested_hotkey:"+r.phase);}return keys.Length;}
}
static object Reentrant(){var trace=new List<string>();var launch=DesktopLaunch.Dispatch(new ReentrantApp(false,trace),true);var hotkey=VoiceHotkey.Dispatch(new AlwaysOwner(),new ReentrantKeyboard(false,trace),["control"],32);return new{launch=launch.phase,hotkey=hotkey.phase,trace};}
public static string Run(string input){using var doc=JsonDocument.Parse(input);var r=doc.RootElement;string path=r.GetProperty("image").GetString();return JsonSerializer.Serialize(new{inputSha256=Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(Encoding.UTF8.GetBytes(input))),reentrant=Reentrant(),applications=r.GetProperty("applications").EnumerateArray().Select(v=>Capture(()=>{DesktopLaunch.ValidateApplication(v.GetString());Target(path,v.GetString());return true;})).ToArray(),plans=r.GetProperty("plans").EnumerateArray().Select(Plans).ToArray(),chords=r.GetProperty("chords").EnumerateArray().Select(Chord).ToArray(),launches=r.GetProperty("launches").EnumerateArray().Select(v=>Launch(v,path)).ToArray(),focus=r.GetProperty("focus").EnumerateArray().Select(Focus).ToArray(),invokes=r.GetProperty("invokes").EnumerateArray().Select(Invoke).ToArray(),native=r.GetProperty("native").EnumerateArray().Select(Native).ToArray()},Json);}
}}
'@
# Add-Type assemblies have no on-disk location. Bind only this fixture's four
# imports to its exact path so PowerShell's relative native loader is not used.
$fixture=(Resolve-Path -LiteralPath $env:IVY_PHONE_ACTIONS_FIXTURE).Path.Replace('\','\\').Replace('"','\"')
$definition=$definition.Replace('"phone_desktop_actions_fixture.dll"','"'+$fixture+'"')
Add-Type -TypeDefinition $definition
try {
 [Console]::Out.Write([Ivy.PhoneBridge.DesktopActionsOracle]::Run([Console]::In.ReadToEnd()))
} catch {
 [Console]::Error.WriteLine($_.Exception.ToString())
 exit 1
}
