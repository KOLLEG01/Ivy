using System.Globalization;
using System.Runtime.InteropServices;

namespace Ivy.PhoneBridge;

public sealed record DesktopReadStage(string Stage, double ElapsedMs, int Count);
public sealed record DesktopInvokeResult(string Phase);

public static partial class WindowsDesktopControls {
    public static DesktopVoiceSession OpenVoice(DesktopIdentity desktop) {
        ArgumentNullException.ThrowIfNull(desktop);
        return DesktopVoiceSession.Open(() => new Source(desktop.pid, true), desktop.Owner().IsCurrent);
    }
    public static DesktopControlsObservation Observe(DesktopIdentity desktop, string[] labels) {
        return Observe(desktop, labels, null);
    }
    public static DesktopControlsObservation Observe(DesktopIdentity desktop, string[] labels, Action<Exception> onUnavailable) {
        ArgumentNullException.ThrowIfNull(desktop);
        return DesktopControls.Read(labels, desktop.Owner().IsCurrent, () => new Source(desktop.pid), onUnavailable);
    }
    public static TimedDesktopControlsObservation ObserveTimed(DesktopIdentity desktop, string[] labels,
        TimeProvider time = null, Action<Exception> onUnavailable = null, Action onReadStarted = null,
        Action<DesktopReadStage> onReadStage = null) {
        ArgumentNullException.ThrowIfNull(desktop); time ??= TimeProvider.System;
        long timestamp = time.GetTimestamp();
        // COM and fixed query/cache preparation read no UI evidence.
        // Stamp before the renewed owner check and complete window/control read, never at completion.
        var observation = DesktopControls.Read(labels, desktop.Owner().IsCurrent, () => new Source(desktop.pid, onReadStage: onReadStage),
            onUnavailable, () => { timestamp = time.GetTimestamp(); onReadStarted?.Invoke(); });
        return new(timestamp, observation);
    }
    public static DesktopInvokeResult InvokeVoice(DesktopIdentity desktop, bool stop, Func<bool> current) {
        ArgumentNullException.ThrowIfNull(desktop); ArgumentNullException.ThrowIfNull(current);
        if (!current() || !desktop.Owner().IsCurrent()) return new("not_submitted");
        try {
            using var source = new Source(desktop.pid, true);
            var labels = stop ? DesktopVoiceControls.StopLabels : DesktopVoiceControls.StartLabels;
            var result = source.Read(labels);
            var matches = result.Where(window => !window.Minimized).SelectMany(window => window.Controls.Select(control => (window, control)))
                .Where(item => item.control.Enabled && !item.control.Offscreen).ToArray();
            var selected = labels.Select(label => matches.Where(item => item.control.Name == label).ToArray())
                .FirstOrDefault(items => items.Length > 0);
            if (selected == null || selected.Length != 1 || !current() || !desktop.Owner().IsCurrent()) return new("not_submitted");
            return source.Invoke(selected[0].window.Handle, selected[0].control, current);
        } catch { return new("not_submitted"); }
    }
    delegate bool WindowCallback(IntPtr window, IntPtr data);
    [DllImport("user32.dll", SetLastError = true)] static extern bool EnumWindows(WindowCallback callback, IntPtr data);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr window);
    [DllImport("ole32.dll")] static extern int CoInitializeEx(IntPtr reserved, uint mode);
    [DllImport("ole32.dll")] static extern void CoUninitialize();
    static void Check(int result) { if (result != 0) Marshal.ThrowExceptionForHR(result < 0 ? result : unchecked((int)0x80004005)); }

    sealed class Source : IDesktopVoiceSource {
        readonly int pid;
        readonly bool retainVoice;
        readonly Action<DesktopReadStage> onReadStage;
        readonly List<object> references = [];
        readonly IAutomation automation;
        readonly Dictionary<string, (string Window, IElement Element)> controlsById = new(StringComparer.Ordinal);
        readonly Dictionary<string, IWalker> windowWalkers = new(StringComparer.Ordinal);
        ICache controlCache;
        ICache parentCache;
        IWalker rawWalker;
        object match;
        string[] preparedLabels;
        bool initialized;
        T Keep<T>(T value) where T : class {
            if (value == null) throw new InvalidOperationException("Desktop accessibility unavailable.");
            references.Add(value); return value;
        }
        public Source(int pid, bool retainVoice = false, Action<DesktopReadStage> onReadStage = null) {
            this.pid = pid; this.retainVoice = retainVoice; this.onReadStage = onReadStage;
            if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess) throw new PlatformNotSupportedException();
            try {
                int result = CoInitializeEx(IntPtr.Zero, 0);
                if (result != 0 && result != 1) Check(result);
                initialized = true;
                automation = (IAutomation)Keep(Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("e22ad333-b25f-460c-83d0-0581107395c9"))));
                Check(automation.SetAutoFocus(false)); Check(automation.SetConnectionTimeout(250)); Check(automation.SetTransactionTimeout(500));
            } catch { Dispose(); throw; }
        }
        (IntPtr Window, bool Minimized)[] Windows() {
            var windows = new List<(IntPtr, bool)>(); bool overflow = false;
            if (!EnumWindows((window, _) => {
                GetWindowThreadProcessId(window, out var owner);
                if (owner == pid && IsWindowVisible(window)) {
                    if (windows.Count == 32) { overflow = true; return false; }
                    windows.Add((window, IsIconic(window)));
                }
                return true;
            }, IntPtr.Zero) || overflow) throw new InvalidOperationException("Desktop window inventory unavailable.");
            return windows.OrderBy(item => item.Item1.ToInt64()).ToArray();
        }
        object Property(IElement element, int property, bool cached = true) {
            object value;
            Check(cached ? element.CachedEx(property, true, out value) : element.CurrentEx(property, true, out value));
            if (value != null && Marshal.IsComObject(value)) references.Add(value);
            return value;
        }
        T Property<T>(IElement element, int property, bool cached = true) {
            try {
                if (Property(element, property, cached) is T value) return value;
                throw new InvalidOperationException("Required accessibility property unavailable.");
            } catch (Exception error) { error.Data["propertyId"] = property; throw; }
        }
        public void Prepare(string[] labels) {
            DesktopControls.ValidateLabels(labels);
            if (preparedLabels != null) {
                if (!preparedLabels.SequenceEqual(labels)) throw new InvalidOperationException("Original accessibility query cannot change.");
                return;
            }
            // Fetch button properties together, then compare exact cached names locally. A long
            // nested name condition makes the provider repeat property work during traversal.
            // Scope, hidden controls and complete matching inventory remain unchanged.
            Check(automation.CreatePropertyCondition(30003, 50000, out match)); Keep(match);
            Check(automation.CreateCacheRequest(out var cache)); Keep(cache);
            controlCache = cache;
            foreach (int property in new[] { 30000, 30005, 30010, 30022, 30086 }) Check(cache.AddProperty(property));
            Check(automation.CreateCacheRequest(out var parents)); Keep(parents);
            Check(parents.AddProperty(30000)); parentCache = parents;
            Check(automation.RawWalker(out var walker)); Keep(walker);
            rawWalker = walker;
            preparedLabels = (string[])labels.Clone();
        }
        public DesktopControlWindow[] Read(string[] labels) {
            Prepare(labels);
            var phase = System.Diagnostics.Stopwatch.StartNew();
            void Stage(string name, int count) {
                double elapsed = phase.Elapsed.TotalMilliseconds;
                try { onReadStage?.Invoke(new(name, elapsed, count)); } catch { }
                phase.Restart();
            }
            var windows = Windows();
            Stage("window_inventory", windows.Length);
            var cache = controlCache; var walker = rawWalker;
            var result = new List<DesktopControlWindow>();
            foreach (var (window, minimized) in windows) {
                GetWindowThreadProcessId(window, out var owner);
                if (owner != pid) throw new InvalidOperationException("Desktop window changed.");
                Check(automation.ElementFromHandle(window, out var element)); Keep(element);
                Stage("window_element", 1);
                if (retainVoice) {
                    Check(automation.CreatePropertyCondition(30020, checked((int)window.ToInt64()), out var windowCondition)); Keep(windowCondition);
                    Check(automation.CreateTreeWalker(windowCondition, out var windowWalker)); Keep(windowWalker);
                    windowWalkers.Add(window.ToInt64().ToString(CultureInfo.InvariantCulture), windowWalker);
                }
                Check(element.FindAllCached(4, match, cache, out var found)); Keep(found);
                Check(found.Length(out int count));
                Stage("button_cache", count);
                var matching = DesktopControls.SelectNamed(count, index => {
                    Check(found.At(index, out var candidate)); return Keep(candidate);
                }, candidate => Property<string>(candidate, 30005), labels);
                Stage("cached_names", matching.Length);
                var controls = new List<DesktopControl>();
                foreach (var control in matching) {
                    Check(walker.ParentCached(control, parentCache, out var parent)); Keep(parent);
                    var toggle = Property(control, 30086);
                    var id = Property<int[]>(control, 30000);
                    if (retainVoice) controlsById.Add(string.Join(',', id), (window.ToInt64().ToString(CultureInfo.InvariantCulture), control));
                    controls.Add(new(id, Property<int[]>(parent, 30000), Property<string>(control, 30005),
                        Property<bool>(control, 30010), Property<bool>(control, 30022), toggle is int value ? value : null));
                }
                GetWindowThreadProcessId(window, out owner);
                if (owner != pid) throw new InvalidOperationException("Desktop window changed.");
                result.Add(new(window.ToInt64().ToString(CultureInfo.InvariantCulture), minimized, controls.ToArray()));
                Stage("matching_properties", controls.Count);
            }
            if (!windows.SequenceEqual(Windows())) throw new InvalidOperationException("Desktop window inventory changed.");
            Stage("window_recheck", windows.Length);
            return result.ToArray();
        }
        public DesktopControlWindow[] Refresh(VoiceControlIdentity identity) {
            int retained = references.Count;
            try {
                var windows = Windows();
                var original = windows.Single(item => item.Window.ToInt64().ToString(CultureInfo.InvariantCulture) == identity.Window);
                if (original.Minimized) throw new InvalidOperationException("Original Voice window is minimized.");
                var refreshed = new List<DesktopControl>();
                foreach (var id in new[] { identity.MicrophoneId, identity.StopId }) {
                    if (!controlsById.TryGetValue(string.Join(',', id), out var bound) || bound.Window != identity.Window)
                        throw new InvalidOperationException("Original Voice element unavailable.");
                    Check(bound.Element.UpdateCache(controlCache, out var element)); Keep(element);
                    Check(rawWalker.ParentCached(element, parentCache, out var parent)); Keep(parent);
                    var actualId = Property<int[]>(element, 30000); var parentId = Property<int[]>(parent, 30000);
                    if (!actualId.SequenceEqual(id) || !parentId.SequenceEqual(identity.ParentId))
                        throw new InvalidOperationException("Original Voice group changed.");
                    Check(windowWalkers[identity.Window].Normalize(element, out var ancestor)); Keep(ancestor);
                    if (Property<int>(ancestor, 30020, false) != checked((int)original.Window.ToInt64()))
                        throw new InvalidOperationException("Original Voice window changed.");
                    var toggle = Property(element, 30086);
                    refreshed.Add(new(actualId, parentId, Property<string>(element, 30005), Property<bool>(element, 30010),
                        Property<bool>(element, 30022), toggle is int value ? value : null));
                }
                if (!windows.SequenceEqual(Windows())) throw new InvalidOperationException("Desktop window inventory changed.");
                return [new(identity.Window, false, refreshed.ToArray())];
            } finally { ReleaseSince(retained); }
        }
        public DesktopInvokeResult Invoke(string window, DesktopControl control, Func<bool> current) {
            int retained = references.Count;
            try {
                if (!current()) return new("not_submitted");
                var windows = Windows();
                var original = windows.Single(item => item.Window.ToInt64().ToString(CultureInfo.InvariantCulture) == window);
                if (original.Minimized || !controlsById.TryGetValue(string.Join(',', control.Id), out var bound) || bound.Window != window)
                    return new("not_submitted");
                Check(bound.Element.UpdateCache(controlCache, out var element)); Keep(element);
                if (!Property<int[]>(element, 30000).SequenceEqual(control.Id) ||
                    Property<string>(element, 30005) != control.Name || !Property<bool>(element, 30010) || Property<bool>(element, 30022))
                    return new("not_submitted");
                if (!current() || !windows.SequenceEqual(Windows())) return new("not_submitted");
                var iid = new Guid("fb377fbe-8ea6-46d5-9c73-6499642d3059");
                Check(element.GetCurrentPatternAs(10000, ref iid, out var pattern)); Keep(pattern);
                if (!current()) return new("not_submitted");
                try { Check(pattern.Invoke()); return new("submitted"); }
                catch { return new("outcome_unknown"); }
            } finally { ReleaseSince(retained); }
        }
        void ReleaseSince(int retained) {
            Exception failure = null;
            for (int index = references.Count - 1; index >= retained; index--) {
                try { Marshal.ReleaseComObject(references[index]); } catch (Exception error) { failure ??= error; }
            }
            references.RemoveRange(retained, references.Count - retained);
            if (failure != null) throw failure;
        }
        public void Dispose() {
            try {
                ReleaseSince(0);
            } finally { controlsById.Clear(); windowWalkers.Clear(); if (initialized) { initialized = false; CoUninitialize(); } }
        }
    }
}
