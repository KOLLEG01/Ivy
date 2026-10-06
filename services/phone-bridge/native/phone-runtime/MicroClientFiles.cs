using System.Security.Cryptography;
using System.Text.Json;

namespace Ivy.PhoneBridge;

internal sealed class MicroClientChangedException() : IOException("Reviewed USBip client files changed.");

// DLLs are part of executable identity. Keep all three pinned files non-writable and
// non-deletable while the command runs, not merely the top-level process image.
internal sealed class MicroClientFiles : IDisposable {
    private readonly List<FileStream> files = [];
    internal static IReadOnlyDictionary<string, string> ReleasePins() {
        using var resource = typeof(MicroClientFiles).Assembly.GetManifestResourceStream("Ivy.PhoneBridge.MicroUsbClientPins");
        var pins = JsonSerializer.Deserialize<Dictionary<string, string>>(resource);
        if (pins == null || pins.Count != 3 || !pins.Keys.ToHashSet(StringComparer.Ordinal).SetEquals(new[] { "usbip.exe", "libusbip.dll", "resources.dll" }))
            throw new InvalidOperationException("Exact bundled USBip release pins required.");
        return pins;
    }
    internal static async Task<MicroClientFiles> Acquire(string directory, IReadOnlyDictionary<string, string> pins, CancellationToken cancellation) {
        var lease = new MicroClientFiles();
        try {
            foreach (var (name, expected) in pins) {
                if (name != Path.GetFileName(name) || name.Contains('/') || name.Contains('\\')) throw new ArgumentException("Client pins require sibling files.");
                var file = new FileStream(Path.Combine(directory, name), FileMode.Open, FileAccess.Read, FileShare.Read);
                lease.files.Add(file);
                var actual = Convert.ToHexStringLower(await SHA256.HashDataAsync(file, cancellation));
                if (actual != expected) throw new MicroClientChangedException();
            }
            return lease;
        } catch { lease.Dispose(); throw; }
    }
    public void Dispose() { foreach (var file in files) file.Dispose(); files.Clear(); }
}
