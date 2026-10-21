$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$source=[IO.File]::ReadAllText((Join-Path $PSScriptRoot '../../../services/phone-bridge/native/input/DesktopLaunch.cs'))
foreach($entry in @(
 @('[DllImport("ole32.dll")] static extern int CoInitializeEx','[DllImport("phone_desktop_activation_fixture.dll", EntryPoint="FixtureInitialize")] static extern int CoInitializeEx'),
 @('[DllImport("ole32.dll")] static extern void CoUninitialize','[DllImport("phone_desktop_activation_fixture.dll", EntryPoint="FixtureUninitialize")] static extern void CoUninitialize'),
 @('[DllImport("ole32.dll", PreserveSig = false)] static extern void CoCreateInstance','[DllImport("phone_desktop_activation_fixture.dll", EntryPoint="FixtureCreate", PreserveSig = false)] static extern void CoCreateInstance'),
 @('!Environment.UserInteractive','!DesktopActivationOracle.Interactive()')
)){
 if($source.IndexOf($entry[0],[StringComparison]::Ordinal) -lt 0){throw 'Original activation I/O seam changed'}
 $source=$source.Replace($entry[0],$entry[1])
}
$definition=$source+@'
namespace Ivy.PhoneBridge {
// Unused Observe/Owner dependency only: no original process discovery is called.
public sealed class DesktopProcessOwner {public DesktopProcessOwner(int pid,long ticks,string image){}public bool IsCurrent()=>throw new InvalidOperationException("unused");}
public static class DesktopActivationOracle {
 static readonly System.Text.Json.JsonSerializerOptions Json=new(){PropertyNamingPolicy=System.Text.Json.JsonNamingPolicy.CamelCase};
 [DllImport("phone_desktop_activation_fixture.dll")] static extern void FixtureReset(int init,int create,int act,uint pid,int output,int station,int information,uint flags);
 [DllImport("phone_desktop_activation_fixture.dll")] static extern IntPtr FixtureTrace();
 [DllImport("phone_desktop_activation_fixture.dll")] static extern int FixtureReferences();
 [DllImport("phone_desktop_activation_fixture.dll")] static extern bool FixtureInteractive();
 public static bool Interactive()=>FixtureInteractive();
 sealed class Application(WindowsDesktopApplication original):IDesktopApplication {
  public DesktopObservation Observe()=>DesktopObservation.Select(false,[]);
  public int Activate()=>original.Activate();
 }
 static object Capture(Func<object> action){
  try{return new{ok=true,value=action()};}
  catch(ArgumentException e){return new{ok=false,kind="argument",error=e.Message.Split('\n')[0].TrimEnd('\r')};}
  catch(PlatformNotSupportedException e){return new{ok=false,kind="platform",error=e.Message};}
  catch{return new{ok=false,kind="unavailable"};}
 }
 static object Row(System.Text.Json.JsonElement row){
  FixtureReset(row.GetProperty("init").GetInt32(),row.GetProperty("create").GetInt32(),row.GetProperty("act").GetInt32(),row.GetProperty("pid").GetUInt32(),row.GetProperty("output").GetInt32(),row.GetProperty("station").GetInt32(),row.GetProperty("information").GetInt32(),row.GetProperty("flags").GetUInt32());
  var result=Capture(()=>{
   var application=new WindowsDesktopApplication(row.GetProperty("application").GetString());
   string operation=row.GetProperty("operation").GetString();
   if(operation=="construct")return true;
   if(operation=="direct")return application.Activate();
   var value=DesktopLaunch.Dispatch(new Application(application),true);
   return new{value.phase,identity=(object)null,value.activationPid,value.errorCode};
  });
  return new{result,trace=Marshal.PtrToStringUTF8(FixtureTrace()),references=FixtureReferences()};
 }
 public static string Run(string input){
  using var doc=System.Text.Json.JsonDocument.Parse(input);
  var rows=new System.Collections.Generic.List<object>();foreach(var row in doc.RootElement.EnumerateArray())rows.Add(Row(row));
  return System.Text.Json.JsonSerializer.Serialize(new{inputSha256=Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(Encoding.UTF8.GetBytes(input))),actualUserInteractive=Environment.UserInteractive,rows},Json);
 }
}
}
'@
$fixture=(Resolve-Path -LiteralPath $env:IVY_PHONE_ACTIVATION_FIXTURE).Path.Replace('\','\\').Replace('"','\"')
$definition=$definition.Replace('"phone_desktop_activation_fixture.dll"','"'+$fixture+'"')
Add-Type -TypeDefinition $definition
try {[Console]::Out.Write([Ivy.PhoneBridge.DesktopActivationOracle]::Run([Console]::In.ReadToEnd()))}
catch {[Console]::Error.WriteLine($_.Exception.ToString());exit 1}
