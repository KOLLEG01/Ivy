using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;

namespace Ivy.PhoneBridge {
    public struct KeyStroke {
        public readonly ushort Key;
        public readonly bool Up;
        public KeyStroke(ushort key, bool up) { Key = key; Up = up; }
    }

    public interface IKeyboardInput {
        bool IsIdle();
        int Send(KeyStroke[] keys);
    }

    public interface IDesktopOwner {
        bool IsCurrent();
        bool TryFocus();
        bool IsFocused();
    }

    // Exact process metadata and optional normal foreground activation; no debugger or injection.
    public sealed class DesktopProcessOwner : IDesktopOwner {
        readonly int pid;
        readonly long startTicks;
        readonly string imagePath;
        [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr window);
        [DllImport("user32.dll")] static extern bool IsIconic(IntPtr window);
        [DllImport("user32.dll")] static extern bool ShowWindowAsync(IntPtr window, int command);
        [DllImport("kernel32.dll")] static extern uint GetCurrentProcessId();
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool ProcessIdToSessionId(uint processId, out uint sessionId);
        [DllImport("user32.dll", SetLastError = true)] static extern IntPtr SendMessageTimeout(
            IntPtr window, uint message, UIntPtr wParam, IntPtr lParam, uint flags, uint milliseconds, out UIntPtr result);
        public DesktopProcessOwner(int pid, long startTicks, string imagePath) {
            if (pid <= 0 || startTicks <= 0 || String.IsNullOrWhiteSpace(imagePath) ||
                !Path.IsPathRooted(imagePath)) throw new ArgumentException("Invalid Desktop process identity.");
            this.pid = pid; this.startTicks = startTicks; this.imagePath = Path.GetFullPath(imagePath);
        }
        public bool IsCurrent() {
            try {
                using (var desktop = Process.GetProcessById(pid)) {
                    return !desktop.HasExited && SameSession(pid) &&
                        desktop.StartTime.ToUniversalTime().Ticks == startTicks &&
                        desktop.MainWindowHandle != IntPtr.Zero &&
                        String.Equals(Path.GetFullPath(desktop.MainModule.FileName), imagePath, StringComparison.OrdinalIgnoreCase);
                }
            } catch { return false; }
        }
        internal static bool SameSession(int processId) {
            // Process.SessionId populates a general process snapshot on each new Process object.
            // Ask Windows for just the two session IDs; keep the same fresh equality requirement.
            uint target, caller;
            return processId > 0 && ProcessIdToSessionId((uint)processId, out target) &&
                ProcessIdToSessionId(GetCurrentProcessId(), out caller) && target == caller;
        }
        public bool IsFocused() {
            if (!IsCurrent()) return false;
            try {
                using (var desktop = Process.GetProcessById(pid))
                    return GetForegroundWindow() == desktop.MainWindowHandle;
            } catch { return false; }
        }
        public bool TryFocus() {
            if (!IsCurrent()) return false;
            try {
                using (var desktop = Process.GetProcessById(pid)) {
                    var window = desktop.MainWindowHandle;
                    return ConfirmActivation(IsCurrent, () => GetForegroundWindow() == window && IsFocused(),
                        () => !IsIconic(window) || ShowWindowAsync(window, 9),
                        () => SetForegroundWindow(window), () => {
                            // Cross-process foreground activation is asynchronous. WM_NULL changes
                            // no ui state; it waits for the original window's activation notification.
                            // Abort on a hung/destroyed window and never attach input queues or retry.
                            UIntPtr ignored;
                            return SendMessageTimeout(window, 0, UIntPtr.Zero, IntPtr.Zero, 0x22, 200, out ignored) != IntPtr.Zero;
                        });
                }
            } catch { return false; }
        }
        internal static bool ConfirmActivation(Func<bool> current, Func<bool> foreground,
            Func<bool> restore, Func<bool> activate, Func<bool> processed) {
            if (!current()) return false;
            if (foreground()) return current();
            if (!restore() || !current() || !activate() || !processed()) return false;
            return current() && foreground();
        }
    }

    public sealed class WindowsKeyboardInput : IKeyboardInput {
        // INPUT's union includes MOUSEINPUT; its complete x64 size is 40, even for keyboard events.
        [StructLayout(LayoutKind.Explicit, Size = 40)]
        struct Input {
            [FieldOffset(0)] public uint Type;
            [FieldOffset(8)] public ushort Key;
            [FieldOffset(10)] public ushort Scan;
            [FieldOffset(12)] public uint Flags;
            [FieldOffset(16)] public uint Time;
            [FieldOffset(24)] public UIntPtr Extra;
        }
        [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint count, Input[] inputs, int size);
        [DllImport("user32.dll")] static extern short GetAsyncKeyState(int key);

        public WindowsKeyboardInput() {
            if (Environment.OSVersion.Platform != PlatformID.Win32NT || IntPtr.Size != 8)
                throw new PlatformNotSupportedException("Voice input requires Windows x64.");
        }
        public bool IsIdle() {
            // Do not compensate for keys held by the user. Refuse the chord before any insertion.
            for (int key = 1; key < 255; key++) if ((GetAsyncKeyState(key) & 0x8000) != 0) return false;
            return true;
        }
        public int Send(KeyStroke[] keys) {
            var inputs = new Input[keys.Length];
            for (int index = 0; index < keys.Length; index++) {
                var key = keys[index];
                inputs[index] = new Input { Type = 1, Key = key.Key,
                    Flags = (key.Up ? 2u : 0u) | (key.Key == 0x5b ? 1u : 0u) };
            }
            return checked((int)SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(Input))));
        }
    }

    public sealed class HotkeyResult {
        // Properties (rather than fields) are intentional: PhoneRuntime serializes command
        // results with System.Text.Json's default property contract.
        readonly string _phase;
        readonly int _requested;
        readonly int? _submitted;
        readonly bool _keyUpSubmitted;
        readonly string _errorCode;
        public string phase { get { return _phase; } }
        public int requested { get { return _requested; } }
        public int? submitted { get { return _submitted; } }
        public bool keyUpSubmitted { get { return _keyUpSubmitted; } }
        public string errorCode { get { return _errorCode; } }
        public HotkeyResult(string phase, int requested, int? submitted, bool keyUpSubmitted, string errorCode) {
            _phase = phase; _requested = requested; _submitted = submitted;
            _keyUpSubmitted = keyUpSubmitted; _errorCode = errorCode;
        }
    }

    public static class VoiceHotkey {
        static readonly object gate = new object();

        public static KeyStroke[] Plan(string[] modifiers, int keyCode) {
            if (modifiers == null || modifiers.Length < 1 || modifiers.Length > 4 ||
                !(keyCode == 32 || (keyCode >= 65 && keyCode <= 90) || (keyCode >= 112 && keyCode <= 135)))
                throw new ArgumentException("Invalid configured Voice hotkey.");
            var selected = new HashSet<string>(modifiers, StringComparer.Ordinal);
            if (selected.Count != modifiers.Length || !(selected.Contains("control") || selected.Contains("alt") || selected.Contains("windows")))
                throw new ArgumentException("Voice hotkey requires distinct modifiers and Control, Alt or Windows.");
            var keys = new List<ushort>();
            // Canonical order: permutations of the same configured chord have the same native plan.
            foreach (var name in new[] { "control", "alt", "shift", "windows" }) {
                if (!selected.Remove(name)) continue;
                keys.Add(name == "control" ? (ushort)0x11 : name == "alt" ? (ushort)0x12 : name == "shift" ? (ushort)0x10 : (ushort)0x5b);
            }
            if (selected.Count != 0) throw new ArgumentException("Unknown Voice hotkey modifier.");
            keys.Add((ushort)keyCode);
            var plan = new List<KeyStroke>();
            foreach (var key in keys) plan.Add(new KeyStroke(key, false));
            for (int index = keys.Count - 1; index >= 0; index--) plan.Add(new KeyStroke(keys[index], true));
            return plan.ToArray();
        }

        // Caller must save its original action intent before entering this primitive. Never replay
        // an uncertain dispatch. A submitted chord does not prove Voice acceptance or media readiness.
        public static HotkeyResult Dispatch(IDesktopOwner owner, IKeyboardInput keyboard, string[] modifiers, int keyCode,
            bool focusBeforeHotkey = false, bool focusAlreadyPrepared = false) {
            if (owner == null || keyboard == null) throw new ArgumentNullException();
            var plan = Plan(modifiers, keyCode);
            lock (gate) {
                try {
                    if (!owner.IsCurrent()) return new HotkeyResult("not_submitted", plan.Length, 0, false, "desktop_owner_changed");
                    if (focusBeforeHotkey && !focusAlreadyPrepared && !owner.TryFocus()) return new HotkeyResult("not_submitted", plan.Length, 0, false, "desktop_focus_failed");
                    if (!keyboard.IsIdle()) return new HotkeyResult("not_submitted", plan.Length, 0, false, "keyboard_busy");
                    if (!owner.IsCurrent()) return new HotkeyResult("not_submitted", plan.Length, 0, false, "desktop_owner_changed");
                    if (focusBeforeHotkey && !owner.IsFocused()) return new HotkeyResult("not_submitted", plan.Length, 0, false, "desktop_focus_failed");
                } catch { return new HotkeyResult("not_submitted", plan.Length, 0, false, "input_unavailable"); }
                int? submitted = null;
                try {
                    var count = keyboard.Send(plan);
                    if (count >= 0 && count <= plan.Length) submitted = count;
                    if (count == plan.Length) return new HotkeyResult("submitted", plan.Length, count, true, null);
                    if (count == 0) return new HotkeyResult("not_submitted", plan.Length, 0, false, "input_rejected");
                } catch { /* After entering Send, submission is unknown. Only key-up cleanup follows. */ }
                var releases = new KeyStroke[plan.Length / 2];
                Array.Copy(plan, plan.Length / 2, releases, 0, releases.Length);
                bool keyUpSubmitted = false;
                try { keyUpSubmitted = keyboard.Send(releases) == releases.Length; } catch { }
                return new HotkeyResult("outcome_unknown", plan.Length, submitted, keyUpSubmitted,
                    submitted.HasValue ? "partial_input" : "input_outcome_unknown");
            }
        }
    }
}
