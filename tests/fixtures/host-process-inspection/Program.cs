using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text.Json;

// All handles, ACL changes and windows belong to this disposable fixture.
static class Program {
    delegate nint WindowProcedure(nint window, uint message, nuint word, nint parameter);
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    struct WindowClass {
        public uint size, style; public WindowProcedure procedure; public int classExtra, windowExtra;
        public nint instance, icon, cursor, background; public string? menu, name; public nint smallIcon;
    }
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern ushort RegisterClassExW(ref WindowClass value);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern nint CreateWindowExW(uint extra,string cls,string title,uint style,int x,int y,int width,int height,nint parent,nint menu,nint instance,nint parameter);
    [DllImport("user32.dll")] static extern nint DefWindowProcW(nint window,uint message,nuint word,nint parameter);
    [DllImport("user32.dll")] static extern bool ShowWindow(nint window,int command);
    [DllImport("user32.dll")] static extern bool DestroyWindow(nint window);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern bool UnregisterClassW(string name,nint instance);
    [DllImport("kernel32.dll")] static extern nint GetCurrentProcess();
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool GetKernelObjectSecurity(nint handle,uint info,byte[]? descriptor,uint length,out uint needed);
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool SetKernelObjectSecurity(nint handle,uint info,nint descriptor);
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool SetKernelObjectSecurity(nint handle,uint info,byte[] descriptor);
    [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(string text,uint revision,out nint descriptor,out uint size);
    [DllImport("kernel32.dll")] static extern nint LocalFree(nint pointer);
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode)] static extern int GetApplicationUserModelId(nint process,ref uint length,nint text);
    [DllImport("kernel32.dll")] static extern nint GetCurrentThread();
    [DllImport("kernel32.dll")] static extern bool CloseHandle(nint handle);
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool ImpersonateAnonymousToken(nint thread);
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool RevertToSelf();
    [DllImport("advapi32.dll",SetLastError=true)] static extern bool OpenThreadToken(nint thread,uint access,bool self,out nint token);
    static object Result(Func<object?> action) {
        try { return new {ok=true,value=action()}; }
        catch(Exception error) { return new {ok=false,error=error.GetType().Name,native=(error as Win32Exception)?.NativeErrorCode}; }
    }
    static object Snapshot(Process process) => new {
        pid=process.Id,
        exited=Result(()=>process.HasExited),
        session=Result(()=>process.SessionId),
        created=Result(()=>process.StartTime.ToUniversalTime().ToFileTimeUtc().ToString(System.Globalization.CultureInfo.InvariantCulture)),
        window=Result(()=>process.MainWindowHandle.ToInt64()),
        module=Result(()=>process.MainModule?.FileName),
    };
    static void Print(object value) { Console.WriteLine(JsonSerializer.Serialize(value)); Console.Out.Flush(); }
    static void Main(string[] args) {
        Console.InputEncoding=new System.Text.UTF8Encoding(false);Console.OutputEncoding=new System.Text.UTF8Encoding(false);
        if(args.Length==1 && args[0]=="--child") { while(Console.ReadLine()!=null) {} return; }
        string executable=Environment.ProcessPath!;
        using var child=Process.Start(new ProcessStartInfo(executable,"--child") {UseShellExecute=false,CreateNoWindow=true,RedirectStandardInput=true,RedirectStandardOutput=true,RedirectStandardError=true})!;
        using var retained=Process.GetCurrentProcess();
        var procedure=new WindowProcedure(DefWindowProcW);
        string className="IvyOwnedProcessInspection-"+Environment.ProcessId;
        var cls=new WindowClass {size=(uint)Marshal.SizeOf<WindowClass>(),procedure=procedure,name=className};
        ushort registered=RegisterClassExW(ref cls);
        if(registered==0) throw new Win32Exception();
        nint root=CreateWindowExW(0x08000000,className,"Owned inspection fixture",0x80000000,0,0,40,40,0,0,0,0);
        nint popup=CreateWindowExW(0x08000000,className,"Owned popup",0x80000000,0,0,20,20,root,0,0,0);
        if(root==0 || popup==0) throw new Win32Exception();
        ShowWindow(popup,4); // Visible but owned: never a main-window candidate.
        byte[]? originalSecurity=null;
        try {
            using var currentUser=WindowsIdentity.GetCurrent();
            uint appLength=0;int appStatus=GetApplicationUserModelId(GetCurrentProcess(),ref appLength,0);
            Print(new {pid=Environment.ProcessId,childPid=child.Id,name=retained.ProcessName,runtime=Environment.Version.ToString(),framework=RuntimeInformation.FrameworkDescription,processAssembly=typeof(Process).Assembly.Location,identityAssembly=typeof(WindowsIdentity).Assembly.Location,windowClassSize=Marshal.SizeOf<WindowClass>(),root=root.ToInt64(),popup=popup.ToInt64(),initial=Snapshot(retained),child=Snapshot(child),sid=currentUser.User!.Value,appStatus,appLength});
            for(string? line;(line=Console.ReadLine())!=null;) {
                string[] parts=line.Split(' ',2);
                switch(parts[0]) {
                    case "snapshot": using(var fresh=Process.GetCurrentProcess()) Print(Snapshot(fresh)); break;
                    case "retained": Print(Snapshot(retained)); break;
                    case "child": Print(Snapshot(child)); break;
                    case "show": ShowWindow(root,4); Print(new {ok=true}); break;
                    case "hide": ShowWindow(root,0); Print(new {ok=true}); break;
                    case "name": {
                        var found=Process.GetProcessesByName(parts[1]);
                        try { Print(found.Select(process=>process.Id).ToArray()); }
                        finally { foreach(var process in found)process.Dispose(); }
                        break;
                    }
                    case "deny-module": {
                        GetKernelObjectSecurity(GetCurrentProcess(),4,null,0,out uint needed);
                        originalSecurity=new byte[needed];
                        if(!GetKernelObjectSecurity(GetCurrentProcess(),4,originalSecurity,needed,out _))throw new Win32Exception();
                        string sid=currentUser.User!.Value;
                        if(!ConvertStringSecurityDescriptorToSecurityDescriptorW("D:(D;;0x10;;;"+sid+")(A;;GA;;;"+sid+")",1,out nint descriptor,out _))throw new Win32Exception();
                        try { if(!SetKernelObjectSecurity(GetCurrentProcess(),4,descriptor))throw new Win32Exception(); }
                        finally { LocalFree(descriptor); }
                        Print(new {ok=true}); break;
                    }
                    case "restore":
                        if(originalSecurity!=null && !SetKernelObjectSecurity(GetCurrentProcess(),4,originalSecurity))throw new Win32Exception();
                        originalSecurity=null; Print(new {ok=true}); break;
                    case "exit-child": child.StandardInput.Close();child.WaitForExit();Print(new {ok=true,code=child.ExitCode});break;
                    case "by-id": Print(Result(()=> {using var process=Process.GetProcessById(int.Parse(parts[1]));return process.Id;}));break;
                    case "wrong-token": Print(Result(()=> {using var identity=new WindowsIdentity(GetCurrentProcess());return identity.User?.Value;}));break;
                    case "anonymous": {
                        if(!ImpersonateAnonymousToken(GetCurrentThread()))throw new Win32Exception();
                        try {
                            object original=Result(()=> {using var identity=WindowsIdentity.GetCurrent();return identity.User?.Value;});
                            bool opened=OpenThreadToken(GetCurrentThread(),0x02000000,false,out nint token);
                            int error=opened?0:Marshal.GetLastPInvokeError();if(opened)CloseHandle(token);
                            Print(new {original,probeError=error});
                        } finally {if(!RevertToSelf())throw new Win32Exception();}
                        break;
                    }
                    case "done": Print(new {ok=true});return;
                    default:throw new ArgumentException("Unknown fixture command.");
                }
            }
        } finally {
            if(originalSecurity!=null)SetKernelObjectSecurity(GetCurrentProcess(),4,originalSecurity);
            if(!child.HasExited){child.StandardInput.Close();child.WaitForExit();}
            DestroyWindow(popup);DestroyWindow(root);UnregisterClassW(className,0);GC.KeepAlive(procedure);
        }
    }
}
