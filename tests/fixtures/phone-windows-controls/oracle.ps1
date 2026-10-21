$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$definition=@'
using System;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Collections.Generic;
using System.Threading;
using System.Reflection;
using System.Globalization;
using System.Runtime.InteropServices;
'@ + "`n"
foreach($name in @('DesktopControls.cs','DesktopVoiceControls.cs','DesktopVoiceSession.cs','WindowsDesktopControls.cs','WindowsDesktopControls.Com.cs')) {
    $source=[IO.File]::ReadAllText((Join-Path $PSScriptRoot "../../../services/phone-bridge/native/phone-runtime/$name"))
    $marker='namespace Ivy.PhoneBridge;';$at=$source.IndexOf($marker,[StringComparison]::Ordinal)
    if($at -lt 0){throw 'Original source namespace unavailable'}
    $definition += "namespace Ivy.PhoneBridge {`n"+$source.Substring($at+$marker.Length)+"`n}`n"
}
$definition+=@'
namespace Ivy.PhoneBridge {
// Only the unused outer desktop-owner dependency is synthetic. Source bodies above are unchanged.
public sealed class DesktopIdentity { public int pid; public Owner Owner()=>new(); }
public sealed class Owner { public bool IsCurrent()=>true; }
public static partial class WindowsDesktopControls {
    static readonly JsonSerializerOptions Json = new() { PropertyNamingPolicy=JsonNamingPolicy.CamelCase };
    [DllImport("phone_windows_controls_fixture")] static extern int FixtureReset(int mode,int windows);
    [DllImport("phone_windows_controls_fixture")] static extern IntPtr FixtureAutomation();
    [DllImport("phone_windows_controls_fixture")] static extern void FixtureFailure(int call,int hr,int hide);
    [DllImport("phone_windows_controls_fixture")] static extern void FixturePhase(int phase);
    [DllImport("phone_windows_controls_fixture")] static extern IntPtr FixtureTrace();
    [DllImport("phone_windows_controls_fixture")] static extern int FixtureReferences();
    [DllImport("phone_windows_controls_fixture")] static extern int FixtureWindowOrdinal(IntPtr window);
    [DllImport("phone_windows_controls_fixture")] static extern void FixtureDestroy();
    static object Windows(DesktopControlWindow[] windows)=>windows.Select(w=>new{handle=FixtureWindowOrdinal(new IntPtr(long.Parse(w.Handle,CultureInfo.InvariantCulture))).ToString(CultureInfo.InvariantCulture),minimized=w.Minimized,controls=w.Controls});
    static object Capture(Func<object> action) {try{return new{ok=true,value=JsonSerializer.SerializeToElement(action(),Json)};}catch(Exception e){return new{ok=false,propertyId=e.Data["propertyId"]};}}
    static object Case(JsonElement item) {
        int mode=item.GetProperty("mode").GetInt32(),count=item.GetProperty("windows").GetInt32();
        if(FixtureReset(mode,count)!=1)throw new Exception("Owned window creation failed");
        Source source=null;object result=null;int refsBefore=0,refsAfter=0;
        var stages=new List<object>();var labels=new[]{"Mute microphone","Stop voice chat"};
        try {
            source=new Source(Environment.ProcessId,item.GetProperty("action").GetString()!="observe",s=>{stages.Add(new{stage=s.Stage,count=s.Count});if(item.GetProperty("stageThrow").GetBoolean())throw new Exception("stage");});
            var pointer=FixtureAutomation();
            var replacement=(IAutomation)Marshal.GetTypedObjectForIUnknown(pointer,typeof(IAutomation));Marshal.Release(pointer);
            typeof(Source).GetField("automation",BindingFlags.Instance|BindingFlags.NonPublic).SetValue(source,replacement);
            ((List<object>)typeof(Source).GetField("references",BindingFlags.Instance|BindingFlags.NonPublic).GetValue(source)).Add(replacement);
            FixtureFailure(item.GetProperty("failure").GetInt32(),item.GetProperty("hr").GetInt32(),item.GetProperty("hide").GetInt32());
            result=Capture(()=>{
                // Exercise the fixed setup against the SDK fixture too, after real activation.
                Check(replacement.SetAutoFocus(false));Check(replacement.SetConnectionTimeout(250));Check(replacement.SetTransactionTimeout(500));
                source.Prepare(labels);source.Prepare((string[])labels.Clone());
                var read=source.Read(labels);var action=item.GetProperty("action").GetString();
                if(action=="prepare_changed"){source.Prepare([labels[1],labels[0]]);return null;}
                if(action=="read_twice"){source.Read(labels);return null;}
                if(action=="read"||action=="observe")return Windows(read);
                FixturePhase(1);refsBefore=FixtureReferences();
                object operation=Capture(()=>{
                    if(action=="refresh")return Windows(source.Refresh(new(read[0].Handle,[42,0],[42,1],[42,2])));
                    int current=0,stop=item.GetProperty("currentStop").GetInt32();
                    return source.Invoke(read[0].Handle,read[0].Controls[0],()=>{int i=current++;if(stop<=-2&&i==-(stop+2))throw new Exception("fixture current");return i!=stop;}).Phase;
                });
                refsAfter=FixtureReferences();return new{operation};
            });
        } finally {source?.Dispose();FixtureDestroy();}
        return new{result,stages,trace=Marshal.PtrToStringUTF8(FixtureTrace()),released=FixtureReferences()==0,transientReleased=refsBefore==refsAfter};
    }
    public static string Oracle(string input) {
        using var document=JsonDocument.Parse(input);
        var rows=document.RootElement.EnumerateArray().Select(Case).ToArray();
        return JsonSerializer.Serialize(new{inputSha256=Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(Encoding.UTF8.GetBytes(input))),rows},Json);
    }
}
}
'@
# Run managed C# directly on an MTA thread; PowerShell's own thread apartment is irrelevant.
$definition+=@'
public static class PhoneWindowsOracleThread {
 public static string Run(string input) {string result=null;Exception error=null;var t=new System.Threading.Thread(()=>{try{result=Ivy.PhoneBridge.WindowsDesktopControls.Oracle(input);}catch(Exception e){error=e;}});t.SetApartmentState(System.Threading.ApartmentState.MTA);t.Start();t.Join();if(error!=null)throw error;return result;}
}
'@
Add-Type -TypeDefinition $definition
[Console]::Out.Write([PhoneWindowsOracleThread]::Run([Console]::In.ReadToEnd()))
