using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using System.Text.RegularExpressions;

namespace Ivy.PhoneBridge {
    public sealed class DesktopIdentity {
        public readonly int pid;
        // Keep ticks as decimal text across the JSON/JavaScript boundary.
        public readonly string startTimeUtcTicks, imagePath, appUserModelId;
        public DesktopIdentity(int pid, long ticks, string imagePath, string appUserModelId) {
            DesktopLaunch.ValidateApplication(appUserModelId);
            if (pid <= 0 || ticks <= 0 || String.IsNullOrWhiteSpace(imagePath) || !Path.IsPathRooted(imagePath))
                throw new ArgumentException("Invalid Desktop identity.");
            this.pid = pid; this.startTimeUtcTicks = ticks.ToString(CultureInfo.InvariantCulture);
            this.imagePath = Path.GetFullPath(imagePath); this.appUserModelId = appUserModelId;
        }
        public DesktopProcessOwner Owner() {
            return new DesktopProcessOwner(pid, Int64.Parse(startTimeUtcTicks, CultureInfo.InvariantCulture), imagePath);
        }
    }

    public sealed class DesktopObservation {
        public readonly string state;
        public readonly DesktopIdentity identity;
        DesktopObservation(string state, DesktopIdentity identity) { this.state = state; this.identity = identity; }
        public static DesktopObservation Unavailable() { return new DesktopObservation("unavailable", null); }
        public static DesktopObservation Select(bool matchingProcessExists, DesktopIdentity[] windows) {
            if (windows == null) throw new ArgumentNullException("windows");
            foreach (var window in windows) if (window == null) throw new ArgumentException("Invalid window owner.");
            if (windows.Length > 1) return new DesktopObservation("ambiguous", null);
            if (windows.Length == 1) return new DesktopObservation("ready", windows[0]);
            return new DesktopObservation(matchingProcessExists ? "waiting" : "absent", null);
        }
    }

    public interface IDesktopApplication {
        DesktopObservation Observe();
        int Activate();
    }

    public sealed class DesktopLaunchResult {
        public readonly string phase, errorCode;
        public readonly DesktopIdentity identity;
        public readonly int? activationPid;
        public DesktopLaunchResult(string phase, DesktopIdentity identity, int? activationPid, string errorCode) {
            this.phase = phase; this.identity = identity; this.activationPid = activationPid; this.errorCode = errorCode;
        }
    }

    public static class DesktopLaunch {
        static readonly object gate = new object();
        public static void ValidateApplication(string value) {
            if (value == null || value.Length > 128 || !Regex.IsMatch(value, @"\A[A-Za-z0-9_.-]+![A-Za-z0-9_.-]+\z"))
                throw new ArgumentException("An exact packaged application ID is required.");
        }
        // The owner persists intent before entering this primitive, and never retries an unknown
        // launch. Read-only observations may later resolve availability; they do not establish Voice.
        public static DesktopLaunchResult Dispatch(IDesktopApplication application, bool startIfMissing) {
            if (application == null) throw new ArgumentNullException("application");
            lock (gate) {
                DesktopObservation observed;
                try { observed = application.Observe(); }
                catch { observed = null; }
                if (observed == null || observed.state == "unavailable")
                    return new DesktopLaunchResult("not_submitted", null, null, "desktop_observation_unavailable");
                if (observed.state == "ambiguous") return new DesktopLaunchResult("not_submitted", null, null, "desktop_ambiguous");
                if (observed.state == "ready") return new DesktopLaunchResult("ready", observed.identity, null, null);
                if (observed.state == "waiting") return new DesktopLaunchResult("waiting", null, null, null);
                if (!startIfMissing) return new DesktopLaunchResult("not_submitted", null, null, "desktop_missing");
                try {
                    int pid = application.Activate();
                    if (pid > 0) return new DesktopLaunchResult("submitted", null, pid, null);
                } catch { /* Entered the OS activation call: do not infer no effect from a lost result. */ }
                return new DesktopLaunchResult("outcome_unknown", null, null, "desktop_launch_outcome_unknown");
            }
        }
    }

    public sealed class WindowsDesktopApplication : IDesktopApplication {
        readonly string appUserModelId;
        [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern int GetApplicationUserModelId(IntPtr process, ref uint length, StringBuilder value);
        [DllImport("advapi32.dll", SetLastError = true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
        [DllImport("ole32.dll")] static extern int CoInitializeEx(IntPtr reserved, uint mode);
        [DllImport("ole32.dll")] static extern void CoUninitialize();
        [DllImport("ole32.dll", PreserveSig = false)] static extern void CoCreateInstance(ref Guid clsid, IntPtr outer, uint context, ref Guid iid,
            [MarshalAs(UnmanagedType.Interface)] out IApplicationActivationManager manager);
        [ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IApplicationActivationManager {
            [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string id,
                [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint pid);
            [PreserveSig] int ActivateForFile([MarshalAs(UnmanagedType.LPWStr)] string id, IntPtr items,
                [MarshalAs(UnmanagedType.LPWStr)] string verb, out uint pid);
            [PreserveSig] int ActivateForProtocol([MarshalAs(UnmanagedType.LPWStr)] string id, IntPtr items, out uint pid);
        }
        public WindowsDesktopApplication(string appUserModelId) {
            DesktopLaunch.ValidateApplication(appUserModelId);
            if (Environment.OSVersion.Platform != PlatformID.Win32NT || IntPtr.Size != 8 || !Environment.UserInteractive)
                throw new PlatformNotSupportedException("Desktop activation requires interactive Windows x64.");
            this.appUserModelId = appUserModelId;
        }
        public DesktopObservation Observe() {
            Process[] processes = null;
            try {
                processes = Process.GetProcessesByName("ChatGPT");
                var windows = new List<DesktopIdentity>(); bool matching = false;
                using (var current = Process.GetCurrentProcess())
                using (var user = WindowsIdentity.GetCurrent()) {
                    foreach (var process in processes) {
                        if (process.HasExited || process.SessionId != current.SessionId) continue;
                        IntPtr handle = OpenProcess(0x1000, false, process.Id), token = IntPtr.Zero;
                        if (handle == IntPtr.Zero) return DesktopObservation.Unavailable();
                        try {
                            if (!OpenProcessToken(handle, 8, out token)) return DesktopObservation.Unavailable();
                            using (var owner = new WindowsIdentity(token)) if (owner.User == null || !owner.User.Equals(user.User)) continue;
                            uint length = 0; int status = GetApplicationUserModelId(handle, ref length, null);
                            if (status == 15703) continue; // APPMODEL_ERROR_NO_APPLICATION: unrelated unpackaged process.
                            if (status != 122 || length < 2 || length > 129) return DesktopObservation.Unavailable();
                            var id = new StringBuilder((int)length);
                            if (GetApplicationUserModelId(handle, ref length, id) != 0) return DesktopObservation.Unavailable();
                            if (!String.Equals(id.ToString(), appUserModelId, StringComparison.Ordinal)) continue;
                            matching = true;
                            if (process.MainWindowHandle == IntPtr.Zero) continue;
                            var identity = new DesktopIdentity(process.Id, process.StartTime.ToUniversalTime().Ticks, process.MainModule.FileName, appUserModelId);
                            if (!identity.Owner().IsCurrent()) return DesktopObservation.Unavailable();
                            windows.Add(identity);
                        } finally {
                            if (token != IntPtr.Zero) CloseHandle(token);
                            CloseHandle(handle);
                        }
                    }
                }
                return DesktopObservation.Select(matching, windows.ToArray());
            } catch { return DesktopObservation.Unavailable(); }
            finally { if (processes != null) foreach (var process in processes) process.Dispose(); }
        }
        public int Activate() {
            // Windows brokers the normal launch outside the helper's kill-on-close job. Only the
            // no-error-dialog flag is set. No debug/design/prelaunch/splash or lifecycle changes.
            int initialized = CoInitializeEx(IntPtr.Zero, 0);
            if (initialized < 0 && initialized != unchecked((int)0x80010106)) Marshal.ThrowExceptionForHR(initialized);
            IApplicationActivationManager manager = null;
            try {
                var clsid = new Guid("45ba127d-10a8-46ea-8ab7-56ea9078943c");
                var iid = typeof(IApplicationActivationManager).GUID;
                CoCreateInstance(ref clsid, IntPtr.Zero, 4, ref iid, out manager); // CLSCTX_LOCAL_SERVER
                uint pid; int result = manager.ActivateApplication(appUserModelId, null, 2, out pid);
                Marshal.ThrowExceptionForHR(result);
                return checked((int)pid);
            } finally {
                try { if (manager != null) Marshal.FinalReleaseComObject(manager); }
                finally { if (initialized >= 0) CoUninitialize(); }
            }
        }
    }
}
