using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
class SharedServerProbe {
  [DllImport("kernel32.dll")] static extern IntPtr GetConsoleWindow();
  [DllImport("kernel32.dll")] static extern uint GetConsoleProcessList([Out] uint[] ids, uint size);
  static void Main(string[] args) {
    var pid = Process.GetCurrentProcess().Id;
    for (int tick = 0; tick < 600 && !File.Exists(args[1]); tick++) {
      File.WriteAllText(args[0], "{\"pid\":" + pid + ",\"console\":" + GetConsoleWindow().ToInt64() + ",\"consoleProcesses\":" + GetConsoleProcessList(new uint[8], 8) + ",\"tick\":" + tick + "}");
      Thread.Sleep(100);
    }
  }
}
