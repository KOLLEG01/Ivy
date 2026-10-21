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
using System.Threading.Tasks;
using System.Globalization;
'@ + "`n"
foreach($name in @('DesktopControls.cs','DesktopVoiceControls.cs','DesktopVoiceSession.cs')) {
    $source=[IO.File]::ReadAllText((Join-Path $PSScriptRoot "../../services/phone-bridge/native/phone-runtime/$name"))
    $marker='namespace Ivy.PhoneBridge;'; $at=$source.IndexOf($marker,[StringComparison]::Ordinal)
    if($at -lt 0){throw 'Original Desktop source namespace unavailable'}
    $definition += "namespace Ivy.PhoneBridge {`n"+$source.Substring($at+$marker.Length)+"`n}`n"
}
$definition+=@'
namespace Ivy.PhoneBridge {
public static class DesktopControlsOracle {
    static readonly JsonSerializerOptions Json = new() { PropertyNamingPolicy = JsonNamingPolicy.CamelCase };
    static T Decode<T>(JsonElement value) => JsonSerializer.Deserialize<T>(value.GetRawText(), Json);
    static Exception Fault() => new IOException("fixture");
    sealed class Source(JsonElement input, List<string> trace) : IDesktopControlSource {
        public void Prepare(string[] labels) { trace.Add("prepare"); if (input.GetProperty("fault").GetString()=="prepare") throw Fault(); }
        public DesktopControlWindow[] Read(string[] labels) { trace.Add("read"); if(input.GetProperty("fault").GetString()=="read") throw Fault(); return Decode<DesktopControlWindow[]>(input.GetProperty("windows")); }
        public void Dispose() { trace.Add("close"); if(input.GetProperty("fault").GetString()=="close") throw Fault(); }
    }
    sealed class Clock : TimeProvider { public long Ticks; public override long GetTimestamp()=>Ticks; public override long TimestampFrequency=>1000; }
    static DesktopControlWindow[] Base() => [new("123",false,[
        new([42,1],[42,0],"Mute microphone",true,false,0),new([42,2],[42,0],"Stop voice chat",true,false,null)])];
    sealed class Voice : IDesktopVoiceSource {
        public DesktopControlWindow[] Windows=Base(); public int Discoveries,Refreshes,Disposals;
        public Clock Time; public int Delta; public bool Fail,CloseFail; public VoiceControlIdentity Received;
        public DesktopControlWindow[] Read(string[] labels){Discoveries++;Time.Ticks=618;return Windows;}
        public DesktopControlWindow[] Refresh(VoiceControlIdentity id){Refreshes++;Received=id;Time.Ticks+=Delta;if(Fail)throw Fault();return Windows;}
        public void Dispose(){Disposals++;if(CloseFail)throw Fault();}
    }
    static object Sessions() {
        var clock=new Clock();var source=new Voice{Time=clock};bool current=true;
        var session=DesktopVoiceSession.Open(()=>source,()=>current,clock);
        var rows=new List<object>();
        TimedVoiceControlObservation Read(string action) {var r=session.Read();rows.Add(new{action,timestamp=r.Timestamp,observation=r.Observation,discoveries=source.Discoveries,refreshes=source.Refreshes,disposals=source.Disposals});return r;}
        source.Delta=40;var first=Read("active");first.Observation.Identity.MicrophoneId[1]=90;source.Received.MicrophoneId[1]=91;
        Read("snapshot_mutations");source.Windows[0].Controls[0]=source.Windows[0].Controls[0] with {Name="Unmute microphone",Toggle=1};source.Delta=0;Read("muted");
        source.Windows=Base();source.Delta=499;Read("under_limit");source.Delta=500;Read("at_limit");source.Delta=501;Read("over_limit");
        source.Delta=-1;Read("backwards");source.Delta=0;Read("fresh_again");
        foreach(var change in new[]{"id","parent","disabled","hidden","toggle","transition","window","minimized","duplicate","empty"}) {
            source.Windows=Base();var m=source.Windows[0].Controls[0];
            if(change=="id")source.Windows[0].Controls[0]=m with{Id=[42,99]};
            if(change=="parent")source.Windows[0].Controls[0]=m with{ParentId=[42,99]};
            if(change=="disabled")source.Windows[0].Controls[0]=m with{Enabled=false};
            if(change=="hidden")source.Windows[0].Controls[0]=m with{Offscreen=true};
            if(change=="toggle")source.Windows[0].Controls[0]=m with{Toggle=null};
            if(change=="transition")source.Windows[0].Controls[0]=m with{Name="Cancel voice chat"};
            if(change=="window")source.Windows[0]=source.Windows[0] with{Handle="124"};
            if(change=="minimized")source.Windows[0]=source.Windows[0] with{Minimized=true};
            if(change=="duplicate")source.Windows[0]=source.Windows[0] with{Controls=[m,m]};
            if(change=="empty")source.Windows=[];
            Read(change);
        }
        source.Windows=Base();current=false;Read("not_current");current=true;source.Fail=true;Read("provider_failure");source.Fail=false;
        bool wrongRead=false,wrongClose=false;
        var foreign=new Thread(()=>{try{session.Read();}catch(InvalidOperationException){wrongRead=true;}try{session.Dispose();}catch(InvalidOperationException){wrongClose=true;}});
        foreign.Start();foreign.Join();Read("after_foreign_thread");
        source.CloseFail=true;bool closeFailed=false;try{session.Dispose();}catch(IOException){closeFailed=true;}
        Read("after_failed_close");session.Dispose();
        var failedSource=new Voice{Time=clock,Windows=[]};bool discoveryFailed=false;
        try{DesktopVoiceSession.Open(()=>failedSource,()=>true,clock);}catch(InvalidOperationException){discoveryFailed=true;}
        bool opened=false,oldOwnerFailed=false;
        try{DesktopVoiceSession.Open(()=>{opened=true;return new Voice{Time=clock};},()=>false,clock);}catch(InvalidOperationException){oldOwnerFailed=true;}
        return new{rows,wrongRead,wrongClose,closeFailed,disposals=source.Disposals,discoveryFailed,failedDiscoveryDisposals=failedSource.Disposals,oldOwnerFailed,opened};
    }
    public static string Run(string input) {
        using var document=JsonDocument.Parse(input);var root=document.RootElement;
        var reads=new List<object>();
        foreach(var item in root.GetProperty("reads").EnumerateArray()) {
            var trace=new List<string>();int calls=0;
            try {
                var owners=Decode<bool[]>(item.GetProperty("owners"));var labels=Decode<string[]>(item.GetProperty("labels"));
                var result=DesktopControls.Read(labels,()=>{trace.Add("current");return owners[Math.Min(calls++,owners.Length-1)];},()=>{
                    trace.Add("open");if(item.GetProperty("fault").GetString()=="open")throw Fault();return new Source(item,trace);
                },_=>{trace.Add("diagnostic");if(item.GetProperty("fault").GetString()=="diagnostic")throw Fault();},()=>{
                    trace.Add("started");if(item.GetProperty("fault").GetString()=="started")throw Fault();
                });
                reads.Add(new{ok=true,result,trace});
            } catch(ArgumentException e){reads.Add(new{ok=false,error=e.Message,trace});}
        }
        var classifications=new List<object>();
        foreach(var item in root.GetProperty("classifications").EnumerateArray())classifications.Add(DesktopVoiceControls.Classify(Decode<DesktopControlsObservation>(item)));
        var selected=new List<object>();
        foreach(var item in root.GetProperty("selections").EnumerateArray()) {
            int at=0,names=0;try {
                var labels=Decode<string[]>(item.GetProperty("labels"));var candidates=Decode<string[]>(item.GetProperty("names"));
                int count=item.GetProperty("count").GetInt32(),atFail=item.GetProperty("atFail").GetInt32(),nameFail=item.GetProperty("nameFail").GetInt32();
                var result=DesktopControls.SelectNamed(count,i=>{at++;if(i==atFail)throw Fault();return i;},i=>{names++;if(i==nameFail)throw Fault();return candidates[i%candidates.Length];},labels);
                selected.Add(new{ok=true,result,at,names});
            }catch(Exception e){selected.Add(new{ok=false,error=e.Message,at,names});}
        }
        return JsonSerializer.Serialize(new{inputSha256=Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(Encoding.UTF8.GetBytes(input))),labels=DesktopVoiceControls.Labels,starts=DesktopVoiceControls.StartLabels,stops=DesktopVoiceControls.StopLabels,reads,classifications,selected,sessions=Sessions()},Json);
    }
}
}
'@
Add-Type -TypeDefinition $definition
[Console]::Out.Write([Ivy.PhoneBridge.DesktopControlsOracle]::Run([Console]::In.ReadToEnd()))
