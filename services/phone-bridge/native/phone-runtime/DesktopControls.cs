using System.Globalization;

namespace Ivy.PhoneBridge;

public sealed record DesktopControl(int[] Id, int[] ParentId, string Name, bool Enabled, bool Offscreen, int? Toggle);
public sealed record DesktopControlWindow(string Handle, bool Minimized, DesktopControl[] Controls);
public sealed record DesktopControlsObservation(string State, DesktopControlWindow[] Windows);
public sealed record TimedDesktopControlsObservation(long Timestamp, DesktopControlsObservation Observation);
public interface IDesktopControlSource : IDisposable {
    // May construct fixed query/cache objects only; must not read windows or control evidence.
    void Prepare(string[] labels) { }
    DesktopControlWindow[] Read(string[] labels);
}

public static class DesktopControls {
    internal static T[] SelectNamed<T>(int count, Func<int, T> at, Func<T, string> name, string[] labels) {
        ValidateLabels(labels);
        if (count is < 0 or > 4096) throw new InvalidOperationException("Desktop button inventory limit exceeded.");
        var selected = new List<T>();
        var names = new HashSet<string>(labels, StringComparer.Ordinal);
        for (int index = 0; index < count; index++) {
            var item = at(index);
            if (!names.Contains(name(item))) continue;
            if (selected.Count == 64) throw new InvalidOperationException("Desktop control limit exceeded.");
            selected.Add(item);
        }
        return selected.ToArray();
    }
    public static void ValidateLabels(string[] labels) {
        if (labels == null || labels.Length is < 1 or > 32 || labels.Any(label => string.IsNullOrWhiteSpace(label) || label.Length > 256 || label.Contains('\0')) ||
            labels.Distinct(StringComparer.Ordinal).Count() != labels.Length) throw new ArgumentException("Bounded exact control labels required.");
    }
    static bool Id(int[] id) => id != null && id.Length is >= 1 and <= 32;
    public static DesktopControlsObservation Read(string[] labels, Func<bool> current, Func<IDesktopControlSource> open,
        Action<Exception> onUnavailable = null, Action onReadStarted = null) {
        ValidateLabels(labels); ArgumentNullException.ThrowIfNull(current); ArgumentNullException.ThrowIfNull(open);
        labels = (string[])labels.Clone();
        try {
            if (!current()) throw new InvalidOperationException();
            DesktopControlWindow[] windows;
            using (var source = open()) {
                source.Prepare(labels);
                onReadStarted?.Invoke();
                if (!current()) throw new InvalidOperationException();
                windows = source.Read(labels);
                if (windows == null || windows.Length is < 1 or > 32) throw new InvalidOperationException();
                var handles = new HashSet<string>(StringComparer.Ordinal);
                var ids = new HashSet<string>(StringComparer.Ordinal);
                foreach (var window in windows) {
                    if (window == null || !long.TryParse(window.Handle, NumberStyles.None, CultureInfo.InvariantCulture, out var handle) || handle <= 0 ||
                        handle.ToString(CultureInfo.InvariantCulture) != window.Handle || !handles.Add(window.Handle) ||
                        window.Controls == null || window.Controls.Length > 64) throw new InvalidOperationException();
                    foreach (var control in window.Controls) if (control == null || !Id(control.Id) || !Id(control.ParentId) ||
                        !ids.Add(string.Join(',', control.Id)) || control.Id.SequenceEqual(control.ParentId) ||
                        !labels.Contains(control.Name, StringComparer.Ordinal) || control.Toggle is < 0 or > 2) throw new InvalidOperationException();
                }
                windows = windows.Select(window => window with { Controls = window.Controls.Select(control =>
                    control with { Id = (int[])control.Id.Clone(), ParentId = (int[])control.ParentId.Clone() }).ToArray() }).ToArray();
            }
            if (!current()) throw new InvalidOperationException();
            return new("observed", windows);
        } catch (Exception error) {
            // Optional local diagnostics never alter the conservative observation or expose data
            // through the service's control contract. A failed diagnostic sink is also harmless.
            try { onUnavailable?.Invoke(error); } catch { }
            return new("unavailable", []);
        }
    }
}
