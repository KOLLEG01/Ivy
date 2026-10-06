using System.Security.Cryptography;
using Ivy.PhoneBridge;

static partial class Program {
    static async Task MicroClientFileUnits() {
        var release = MicroClientFiles.ReleasePins();
        Check(release.Count == 3 && release.ContainsKey("usbip.exe") && release.ContainsKey("libusbip.dll") && release.ContainsKey("resources.dll"),
            "published native assembly carries the exact client dependency pins");
        string directory = Directory.CreateTempSubdirectory("ivy-micro-files-").FullName;
        var pins = new Dictionary<string, string>();
        try {
            foreach (string name in release.Keys) {
                byte[] bytes = System.Text.Encoding.UTF8.GetBytes("fixture only: " + name);
                await File.WriteAllBytesAsync(Path.Combine(directory, name), bytes);
                pins[name] = Convert.ToHexStringLower(SHA256.HashData(bytes));
            }
            using (await MicroClientFiles.Acquire(directory, pins, CancellationToken.None)) {
                foreach (string name in pins.Keys) {
                    bool denied = false;
                    try { using var writer = new FileStream(Path.Combine(directory, name), FileMode.Open, FileAccess.Write, FileShare.ReadWrite); }
                    catch (IOException) { denied = true; }
                    Check(denied, "every executable dependency remains non-writable for the command lifetime");
                }
            }
            await File.WriteAllTextAsync(Path.Combine(directory, "libusbip.dll"), "changed dependency");
            await ThrowsAsync<MicroClientChangedException>(MicroClientFiles.Acquire(directory, pins, CancellationToken.None),
                "a substituted DLL refuses the entire client before any process starts");
            // A failed acquisition releases earlier handles as well.
            await File.WriteAllTextAsync(Path.Combine(directory, "usbip.exe"), "released after failure");
        } finally {
            foreach (string name in release.Keys) File.Delete(Path.Combine(directory, name));
            Directory.Delete(directory);
        }
    }
}
