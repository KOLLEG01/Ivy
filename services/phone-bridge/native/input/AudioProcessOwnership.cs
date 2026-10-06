using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;

namespace Ivy.PhoneBridge {
    public sealed class AudioProcessRecord {
        public readonly int pid, parentPid, sessionId;
        public readonly long startTicks;
        public readonly string imagePath;
        public AudioProcessRecord(int pid, int parentPid, int sessionId, long startTicks, string imagePath) {
            this.pid = pid; this.parentPid = parentPid; this.sessionId = sessionId;
            this.startTicks = startTicks; this.imagePath = imagePath;
        }
        public bool Same(AudioProcessRecord other) {
            return other != null && pid == other.pid && parentPid == other.parentPid && sessionId == other.sessionId &&
                startTicks == other.startTicks && String.Equals(imagePath, other.imagePath, StringComparison.OrdinalIgnoreCase);
        }
    }
    public interface IAudioProcessSnapshot : IDisposable { AudioProcessRecord Read(int pid); }

    public sealed class AudioProcessOwnership {
        public readonly int pid;
        public readonly string state;
        AudioProcessOwnership(int pid, string state) { this.pid = pid; this.state = state; }
        // A fresh DesktopObservation supplies the trusted root. This proves only current process
        // ancestry, not that a previously observed stream still belongs to that process incarnation.
        public static AudioProcessOwnership Read(DesktopIdentity root, int pid, Func<bool> rootCurrent, Func<IAudioProcessSnapshot> open) {
            if (root == null || pid <= 0 || rootCurrent == null || open == null) throw new ArgumentException("Pinned Desktop and positive audio PID required.");
            try {
                if (!rootCurrent()) throw new InvalidOperationException("Desktop changed.");
                using (var snapshot = open()) {
                    var pinned = snapshot.Read(root.pid);
                    if (pinned == null || pinned.pid != root.pid || pinned.startTicks.ToString(CultureInfo.InvariantCulture) != root.startTimeUtcTicks ||
                        !String.Equals(pinned.imagePath, root.imagePath, StringComparison.OrdinalIgnoreCase))
                        throw new InvalidOperationException("Desktop changed.");
                    var chain = new List<AudioProcessRecord>(); var seen = new HashSet<int>();
                    int next = pid; string result = "unavailable";
                    while (chain.Count < 64) {
                        if (!seen.Add(next)) throw new InvalidOperationException("Invalid process ancestry.");
                        var process = snapshot.Read(next);
                        if (process == null || process.pid != next || process.startTicks <= 0 || String.IsNullOrWhiteSpace(process.imagePath))
                            throw new InvalidOperationException("Process observation unavailable.");
                        // Windows parent PIDs can be reused after parent exit. A newer parent cannot
                        // be the creator of this child, even when it has the same executable name.
                        if (chain.Count > 0 && process.startTicks > chain[chain.Count - 1].startTicks)
                            throw new InvalidOperationException("Parent identity changed.");
                        chain.Add(process);
                        if (process.sessionId != pinned.sessionId || !String.Equals(process.imagePath, pinned.imagePath, StringComparison.OrdinalIgnoreCase)) {
                            result = "foreign"; break;
                        }
                        if (process.pid == root.pid) {
                            if (!pinned.Same(process)) throw new InvalidOperationException("Desktop changed.");
                            result = "owned"; break;
                        }
                        if (process.parentPid <= 0) { result = "foreign"; break; }
                        next = process.parentPid;
                    }
                    foreach (var process in chain) if (!process.Same(snapshot.Read(process.pid)))
                        throw new InvalidOperationException("Process identity changed during observation.");
                    if (!pinned.Same(snapshot.Read(root.pid)) || !rootCurrent()) throw new InvalidOperationException("Desktop changed.");
                    return new AudioProcessOwnership(pid, result);
                }
            } catch { return new AudioProcessOwnership(pid, "unavailable"); }
        }
    }

    public static class WindowsAudioProcessOwnership {
        public static AudioProcessOwnership Observe(DesktopIdentity root, int pid) {
            if (root == null) throw new ArgumentNullException("root");
            var owner = root.Owner();
            return AudioProcessOwnership.Read(root, pid, owner.IsCurrent, () => new Snapshot());
        }
        // The combined capture observer borrows this owner while correlating fresh inventories.
        internal static IAudioProcessSnapshot OpenSnapshot() { return new Snapshot(); }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct Entry {
            public uint size, usage, pid; public UIntPtr heap;
            public uint module, threads, parentPid; public int priority; public uint flags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string executable;
        }
        [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool Process32FirstW(IntPtr snapshot, ref Entry entry);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool Process32NextW(IntPtr snapshot, ref Entry entry);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
        [DllImport("kernel32.dll", SetLastError = true)] static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
        sealed class Snapshot : IAudioProcessSnapshot {
            readonly Dictionary<int, IntPtr> handles = new Dictionary<int, IntPtr>();
            bool disposed;
            public Snapshot() {
                if (Environment.OSVersion.Platform != PlatformID.Win32NT || IntPtr.Size != 8)
                    throw new PlatformNotSupportedException("Windows x64 required.");
            }
            static int Parent(int pid) {
                var handle = CreateToolhelp32Snapshot(2, 0);
                if (handle == new IntPtr(-1)) throw new InvalidOperationException("Process snapshot unavailable.");
                try {
                    var entry = new Entry { size = (uint)Marshal.SizeOf(typeof(Entry)) };
                    int count = 0;
                    bool found = Process32FirstW(handle, ref entry);
                    while (found) {
                        if (++count > 65536) throw new InvalidOperationException("Process inventory limit exceeded.");
                        if (entry.pid == pid) return checked((int)entry.parentPid);
                        found = Process32NextW(handle, ref entry);
                    }
                    throw new InvalidOperationException("Process missing from inventory.");
                } finally { CloseHandle(handle); }
            }
            public AudioProcessRecord Read(int pid) {
                if (disposed) throw new ObjectDisposedException("Snapshot");
                if (pid <= 0) throw new ArgumentException("Positive process ID required.");
                // Retain the original object through all reads and the caller's capture recheck.
                // A terminated process cannot be replaced by another incarnation of the same PID.
                IntPtr handle;
                if (!handles.TryGetValue(pid, out handle)) {
                    if (handles.Count >= 4096) throw new InvalidOperationException("Process handle limit exceeded.");
                    handle = OpenProcess(0x00101000, false, pid);
                    if (handle == IntPtr.Zero) return null;
                    handles.Add(pid, handle);
                }
                if (WaitForSingleObject(handle, 0) != 258) return null;
                try {
                    using (var process = Process.GetProcessById(pid)) {
                        int parent = Parent(pid);
                        var module = process.MainModule;
                        if (module == null) return null;
                        var result = new AudioProcessRecord(pid, parent, process.SessionId, process.StartTime.ToUniversalTime().Ticks, module.FileName);
                        return process.HasExited || WaitForSingleObject(handle, 0) != 258 ? null : result;
                    }
                } catch (ArgumentException) { return null; }
                catch (InvalidOperationException) { return null; }
                catch (System.ComponentModel.Win32Exception) { return null; }
            }
            public void Dispose() {
                if (disposed) return; disposed = true;
                bool failed = false;
                foreach (var handle in handles.Values) if (!CloseHandle(handle)) failed = true;
                handles.Clear();
                if (failed) throw new InvalidOperationException("Process observation cleanup failed.");
            }
        }
    }
}
