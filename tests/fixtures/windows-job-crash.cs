using System;
using System.Diagnostics;
using System.IO;
using System.Threading;

internal static class WindowsJobCrash {
  static string Quote(string value) { return "\"" + value.Replace("\"", "\\\"") + "\""; }
  static int Main(string[] args) {
    Process launcher=null, child=null;
    try {
      var start=new ProcessStartInfo(args[0], "--parent 0 " + Quote(args[1]) + " -e " + Quote("setInterval(()=>{},1000)"));
      start.UseShellExecute=false; start.CreateNoWindow=true;
      launcher=Process.Start(start);
      var timer=Stopwatch.StartNew(); int id=0;
      while(timer.ElapsedMilliseconds<15000) {
        if(File.Exists(args[2]) && Int32.TryParse(File.ReadAllText(args[2]),out id)) break;
        if(launcher.HasExited) throw new Exception("Launcher exited before the creation barrier.");
        Thread.Sleep(20);
      }
      if(id==0) throw new Exception("Creation barrier was not observed.");
      child=Process.GetProcessById(id);
      // Hold the original child object across termination; never query/kill a later reused PID.
      IntPtr heldHandle=child.Handle;
      if(child.HasExited) throw new Exception("Child exited before launcher fault injection.");
      launcher.Kill(); bool launcherExited=launcher.WaitForExit(5000), childExited=child.WaitForExit(5000);
      Console.WriteLine("{\"launcherExited\":" + launcherExited.ToString().ToLowerInvariant() + ",\"childExited\":" + childExited.ToString().ToLowerInvariant() + ",\"childPid\":" + id + "}");
      return launcherExited && childExited ? 0 : 1;
    } catch(Exception error) { Console.Error.WriteLine(error.Message); return 2; }
    finally {
      if(launcher!=null) { if(!launcher.HasExited) { launcher.Kill(); launcher.WaitForExit(5000); } launcher.Dispose(); }
      if(child!=null) { if(!child.HasExited) { child.Kill(); child.WaitForExit(5000); } child.Dispose(); }
    }
  }
}
