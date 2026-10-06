using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

// The job handle is never inherited. Closing it kills the complete child tree, including after
// launcher/parent failure. Assign the job atomically during suspended process creation: killing
// the launcher between CreateProcess and a later assignment must not orphan a suspended child.
internal static class JobLauncher {
    [StructLayout(LayoutKind.Sequential)] struct BasicLimits {
        public long PerProcessUserTimeLimit, PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize, MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass, SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)] struct IoCounters {
        public ulong ReadOperationCount, WriteOperationCount, OtherOperationCount, ReadTransferCount, WriteTransferCount, OtherTransferCount;
    }
    [StructLayout(LayoutKind.Sequential)] struct ExtendedLimits {
        public BasicLimits Basic;
        public IoCounters Io;
        public UIntPtr ProcessMemoryLimit, JobMemoryLimit, PeakProcessMemoryUsed, PeakJobMemoryUsed;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct StartupInfo {
        public uint cb;
        public string Reserved, Desktop, Title;
        public uint X, Y, XSize, YSize, XCountChars, YCountChars, FillAttribute, Flags;
        public ushort ShowWindow, Reserved2;
        public IntPtr ReservedPointer, StdInput, StdOutput, StdError;
    }
    [StructLayout(LayoutKind.Sequential)] struct ProcessInfo { public IntPtr Process, Thread; public uint ProcessId, ThreadId; }
    [StructLayout(LayoutKind.Sequential)] struct StartupInfoEx { public StartupInfo Startup; public IntPtr Attributes; }
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr CreateJobObject(IntPtr attributes, string name);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr OpenJobObject(uint access, bool inherit, string name);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetInformationJobObject(IntPtr job, int kind, ref ExtendedLimits information, uint length);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool InitializeProcThreadAttributeList(IntPtr attributes, uint count, uint flags, ref UIntPtr size);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool UpdateProcThreadAttribute(IntPtr attributes, uint flags, UIntPtr key, IntPtr value, UIntPtr size, IntPtr previous, IntPtr returned);
    [DllImport("kernel32.dll")] static extern void DeleteProcThreadAttributeList(IntPtr attributes);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool TerminateJobObject(IntPtr job, uint exitCode);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool TerminateProcess(IntPtr process, uint exitCode);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool CreateProcess(string application, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags, IntPtr environment, string directory, ref StartupInfoEx startup, out ProcessInfo process);
    [DllImport("kernel32.dll", SetLastError = true)] static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll", SetLastError = true)] static extern uint WaitForMultipleObjects(uint count, IntPtr[] handles, bool waitAll, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int kind);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr CreateFile(string name, uint access, uint share, IntPtr attributes, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetHandleInformation(IntPtr handle, uint mask, uint flags);
    static void Check(bool success) { if (!success) throw new Win32Exception(Marshal.GetLastWin32Error()); }
    static void ValidateJobName(string name) {
        if (name == null || !System.Text.RegularExpressions.Regex.IsMatch(name, @"^Global\\Ivy\.[a-zA-Z0-9._-]{1,128}$"))
            throw new ArgumentException("Invalid process job name.");
    }
    static string Quote(string value) {
        var result = new StringBuilder("\""); int slashes = 0;
        foreach (char character in value) {
            if (character == '\\') { slashes++; continue; }
            if (character == '"') result.Append('\\', slashes * 2 + 1);
            else result.Append('\\', slashes);
            result.Append(character); slashes = 0;
        }
        result.Append('\\', slashes * 2); result.Append('"'); return result.ToString();
    }
    static int Main(string[] args) {
        IntPtr job = IntPtr.Zero, parent = IntPtr.Zero, nul = IntPtr.Zero, attributes = IntPtr.Zero, jobList = IntPtr.Zero;
        bool attributesInitialized = false; var child = new ProcessInfo();
        try {
            if (args.Length == 2 && args[0] == "--probe-job") {
                ValidateJobName(args[1]);
                job = OpenJobObject(4, false, args[1]); // JOB_OBJECT_QUERY; never signal a probed job.
                if (job == IntPtr.Zero && Marshal.GetLastWin32Error() != 2)
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                // A named job survives until its last handle and all associated processes are gone.
                Console.WriteLine(job == IntPtr.Zero ? "{\"stopped\":true}" : "{\"stopped\":false}");
                return 0;
            }
            if (args.Length < 3 || args[0] != "--parent") throw new ArgumentException("Expected --parent PID executable [args...].");
            uint parentId = UInt32.Parse(args[1]);
            if (parentId != 0) { parent = OpenProcess(0x00100000, false, parentId); Check(parent != IntPtr.Zero); }
            bool allowBreakaway = false;
            string jobName = null;
            int executableIndex = 2;
            if (args[executableIndex] == "--allow-breakaway") { allowBreakaway = true; executableIndex++; }
            if (args.Length > executableIndex && args[executableIndex] == "--job-name") {
                if (args.Length <= executableIndex + 1) throw new ArgumentException("Missing process job name.");
                jobName = args[executableIndex + 1]; ValidateJobName(jobName); executableIndex += 2;
            }
            if (args.Length <= executableIndex) throw new ArgumentException("Missing executable.");
            job = CreateJobObject(IntPtr.Zero, jobName); Check(job != IntPtr.Zero);
            // A delayed launcher must not join or replace a task's previous process tree.
            if (jobName != null && Marshal.GetLastWin32Error() == 183)
                throw new InvalidOperationException("The process job is already owned.");
            // Keep ordinary descendants owned. Only CREATE_BREAKAWAY_FROM_JOB can opt out,
            // and every enclosing Ivy AgentManager job must explicitly permit it.
            var limits = new ExtendedLimits(); limits.Basic.LimitFlags = 0x00002000U | (allowBreakaway ? 0x00000800U : 0U);
            Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf(typeof(ExtendedLimits))));
            var command = new StringBuilder();
            for (int index = executableIndex; index < args.Length; index++) { if (index > executableIndex) command.Append(' '); command.Append(Quote(args[index])); }
            var startup = new StartupInfo(); startup.cb = (uint)Marshal.SizeOf(typeof(StartupInfoEx));
            startup.Flags = 0x00000101; startup.ShowWindow = 0;
            startup.StdInput = GetStdHandle(-10); startup.StdOutput = GetStdHandle(-11); startup.StdError = GetStdHandle(-12);
            // The windowless OS entry point has no console. Give the child valid inherited NUL
            // handles; ordinary process runs keep the caller's explicit pipes for bounded evidence.
            if (startup.StdInput == IntPtr.Zero || startup.StdOutput == IntPtr.Zero || startup.StdError == IntPtr.Zero || startup.StdInput == new IntPtr(-1) || startup.StdOutput == new IntPtr(-1) || startup.StdError == new IntPtr(-1)) {
                nul = CreateFile("NUL", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero); Check(nul != new IntPtr(-1));
                Check(SetHandleInformation(nul, 1, 1));
                if (startup.StdInput == IntPtr.Zero || startup.StdInput == new IntPtr(-1)) startup.StdInput = nul;
                if (startup.StdOutput == IntPtr.Zero || startup.StdOutput == new IntPtr(-1)) startup.StdOutput = nul;
                if (startup.StdError == IntPtr.Zero || startup.StdError == new IntPtr(-1)) startup.StdError = nul;
            }
            UIntPtr attributeBytes = UIntPtr.Zero;
            // The sizing call intentionally returns ERROR_INSUFFICIENT_BUFFER.
            InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref attributeBytes);
            Check(attributeBytes != UIntPtr.Zero && attributeBytes.ToUInt64() <= 65536);
            attributes = Marshal.AllocHGlobal(checked((int)attributeBytes.ToUInt64()));
            Check(InitializeProcThreadAttributeList(attributes, 1, 0, ref attributeBytes)); attributesInitialized = true;
            jobList = Marshal.AllocHGlobal(IntPtr.Size); Marshal.WriteIntPtr(jobList, job);
            Check(UpdateProcThreadAttribute(attributes, 0, new UIntPtr(0x0002000D), jobList, new UIntPtr((uint)IntPtr.Size), IntPtr.Zero, IntPtr.Zero));
            var extended = new StartupInfoEx { Startup = startup, Attributes = attributes };
            Check(CreateProcess(args[executableIndex], command, IntPtr.Zero, IntPtr.Zero, true, 0x08080004, IntPtr.Zero, null, ref extended, out child));
            DeleteProcThreadAttributeList(attributes); attributesInitialized = false;
            Marshal.FreeHGlobal(attributes); attributes = IntPtr.Zero;
            Marshal.FreeHGlobal(jobList); jobList = IntPtr.Zero;
            if (parent != IntPtr.Zero && WaitForMultipleObjects(1, new[] { parent }, false, 0) != 258)
                throw new InvalidOperationException("The process owner has exited.");
            Check(ResumeThread(child.Thread) != UInt32.MaxValue);
            var waits = parent == IntPtr.Zero ? new[] { child.Process } : new[] { child.Process, parent };
            uint outcome = WaitForMultipleObjects((uint)waits.Length, waits, false, UInt32.MaxValue);
            if (outcome != 0) { TerminateJobObject(job, 1); return 1; }
            uint code; Check(GetExitCodeProcess(child.Process, out code)); return unchecked((int)code);
        } catch (Exception error) {
            // Do not print arguments, environment or configurations.
            Console.Error.WriteLine("{\"code\":\"process_launch_failed\",\"type\":\"" + error.GetType().Name + "\"}");
            if (child.Process != IntPtr.Zero) TerminateProcess(child.Process, 1);
            return 1;
        } finally {
            if (attributesInitialized) DeleteProcThreadAttributeList(attributes);
            if (attributes != IntPtr.Zero) Marshal.FreeHGlobal(attributes);
            if (jobList != IntPtr.Zero) Marshal.FreeHGlobal(jobList);
            if (job != IntPtr.Zero) CloseHandle(job);
            if (child.Thread != IntPtr.Zero) CloseHandle(child.Thread);
            if (child.Process != IntPtr.Zero) CloseHandle(child.Process);
            if (parent != IntPtr.Zero) CloseHandle(parent);
            if (nul != IntPtr.Zero && nul != new IntPtr(-1)) CloseHandle(nul);
        }
    }
}
