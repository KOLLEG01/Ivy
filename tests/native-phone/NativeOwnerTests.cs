using System.Diagnostics;
using System.Text.Json;
using Ivy.PhoneBridge;

static partial class Program {
    static async Task NativeOwnerRecovery(string nativeExecutable) {
        string root = Path.Combine(Path.GetTempPath(), "ivy-phone-owner-recovery-" + Guid.NewGuid());
        Directory.CreateDirectory(root);
        var filename = Path.Combine(root, "phone-native-owner.json");
        try {
            await using (var original = new PipePeer(nativeExecutable, root)) {
                Check((await original.Send("status", new { })).Ok, "original process owns the data directory before status");
                string saved = await File.ReadAllTextAsync(filename);
                using var record = JsonDocument.Parse(saved);
                Check(record.RootElement.GetProperty("epoch").GetString() == original.Epoch &&
                    record.RootElement.GetProperty("pid").GetInt32() == original.Process.Id, "durable original native identity");
                await using (var collision = new PipePeer(nativeExecutable, root)) {
                    await collision.Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
                    Check(collision.Process.ExitCode == 1 && await File.ReadAllTextAsync(filename) == saved,
                        "concurrent native startup cannot change or release the original lease");
                }
                Check((await original.Send("status", new { })).Ok, "collision leaves original pipe and owner intact");
                original.Process.Kill(); await original.Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
            }
            await using (var replacement = new PipePeer(nativeExecutable, root)) {
                var status = await replacement.Send("status", new { });
                Check(status.Ok && !status.Result!.Value.GetProperty("configured").GetBoolean(), "killed predecessor allows only fresh unconfigured replacement");
                using var record = JsonDocument.Parse(await File.ReadAllTextAsync(filename));
                Check(record.RootElement.GetProperty("epoch").GetString() == replacement.Epoch, "replacement publishes its own identity");
            }
            // A released file alone is not process-exit proof. Record the real still-live test
            // process without holding a lock, then prove native startup refuses that identity.
            using var current = Process.GetCurrentProcess();
            string live = JsonSerializer.Serialize(new { schemaVersion = 1, epoch = Guid.NewGuid().ToString(), pid = current.Id,
                creationFileTime = current.StartTime.ToUniversalTime().ToFileTimeUtc().ToString(System.Globalization.CultureInfo.InvariantCulture) });
            foreach (string invalid in new[] { live, "{", live.Replace("\"schemaVersion\":1", "\"schemaVersion\":1,\"schemaVersion\":1") }) {
                await File.WriteAllTextAsync(filename, invalid);
                await using var refused = new PipePeer(nativeExecutable, root);
                await refused.Process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5));
                Check(refused.Process.ExitCode == 1 && await File.ReadAllTextAsync(filename) == invalid,
                    "live or unverifiable prior owner is preserved, never killed or overwritten");
            }
        } finally { Directory.Delete(root, true); }
    }
}
