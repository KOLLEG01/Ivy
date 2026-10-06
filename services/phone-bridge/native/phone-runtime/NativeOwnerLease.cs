using System.Globalization;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace Ivy.PhoneBridge;

public static class NativeOwnerLease {
    // Deliberately never disposed after successful admission. Cleanup may still contain OS work
    // when Main returns; only process termination may release this fence to a replacement owner.
    static FileStream retained;
    static readonly object sync = new();
    public sealed record Owner(int SchemaVersion, string Epoch, int Pid, string CreationFileTime);
    static readonly JsonSerializerOptions json = new() { PropertyNamingPolicy = JsonNamingPolicy.CamelCase };

    public static void Acquire(string root, string epoch) {
        if (!OperatingSystem.IsWindows() || !Path.IsPathFullyQualified(root) || !Directory.Exists(root) ||
            !Guid.TryParseExact(epoch, "D", out _)) throw new ArgumentException("Exact native owner directory and epoch required.");
        lock (sync) {
            if (retained != null) throw new InvalidOperationException("Native process already owns a directory.");
            var lease = new FileStream(Path.Combine(root, "phone-native.lock"), FileMode.OpenOrCreate,
                FileAccess.ReadWrite, FileShare.None, 1, FileOptions.WriteThrough);
            try {
                var recordPath = Path.Combine(root, "phone-native-owner.json");
                FileStream previous = null;
                try { previous = new FileStream(recordPath, FileMode.Open, FileAccess.Read, FileShare.Read); }
                catch (FileNotFoundException) { /* No admitted predecessor has published a record. */ }
                if (previous != null) {
                    using var record = previous;
                    if (record.Length is < 2 or > 4096) throw new InvalidOperationException("Native owner record unavailable.");
                    using var document = JsonDocument.Parse(record, new JsonDocumentOptions { MaxDepth = 4 });
                    var original = Read(document.RootElement);
                    if (!OriginalExited(original)) throw new InvalidOperationException("Original native process has not exited.");
                }
                var current = Current(epoch);
                var temporary = recordPath + "." + epoch + ".tmp";
                using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 4096, FileOptions.WriteThrough)) {
                    JsonSerializer.Serialize(output, current, json); output.Flush(true);
                }
                File.Move(temporary, recordPath, true);
                retained = lease;
            } catch { lease.Dispose(); throw; }
        }
    }

    static Owner Read(JsonElement value) {
        var names = new HashSet<string>(StringComparer.Ordinal);
        if (value.ValueKind != JsonValueKind.Object) throw new InvalidOperationException("Native owner record unavailable.");
        foreach (var property in value.EnumerateObject()) if (!names.Add(property.Name)) throw new InvalidOperationException("Duplicate native owner field.");
        if (!names.SetEquals(new[] { "schemaVersion", "epoch", "pid", "creationFileTime" })) throw new InvalidOperationException("Native owner record unavailable.");
        var owner = JsonSerializer.Deserialize<Owner>(value, json);
        if (owner.SchemaVersion != 1 || owner.Pid <= 0 || !Guid.TryParseExact(owner.Epoch, "D", out _) ||
            !long.TryParse(owner.CreationFileTime, NumberStyles.None, CultureInfo.InvariantCulture, out var created) || created <= 0 ||
            created.ToString(CultureInfo.InvariantCulture) != owner.CreationFileTime)
            throw new InvalidOperationException("Native owner record unavailable.");
        return owner;
    }
    static Owner Current(string epoch) {
        if (!GetProcessTimes(GetCurrentProcess(), out var created, out _, out _, out _)) throw new InvalidOperationException("Native process identity unavailable.");
        return new Owner(1, epoch, Environment.ProcessId, created.ToString(CultureInfo.InvariantCulture));
    }
    static bool OriginalExited(Owner original) {
        var handle = OpenProcess(0x00101000, false, original.Pid); // SYNCHRONIZE | QUERY_LIMITED_INFORMATION
        if (handle == IntPtr.Zero) {
            // ERROR_INVALID_PARAMETER means this positive PID no longer names a process. Access
            // denied or any other failure is uncertainty, never permission to replace that owner.
            return Marshal.GetLastWin32Error() == 87;
        }
        try {
            if (!GetProcessTimes(handle, out var created, out _, out _, out _)) return false;
            if (created.ToString(CultureInfo.InvariantCulture) != original.CreationFileTime) return true;
            return WaitForSingleObject(handle, 0) == 0;
        } finally { CloseHandle(handle); }
    }
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetProcessTimes(IntPtr process, out long created, out long exited, out long kernel, out long user);
    [DllImport("kernel32.dll", SetLastError = true)] static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
}
