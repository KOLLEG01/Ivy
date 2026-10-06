using System.Diagnostics;
using System.Text;
using System.Text.RegularExpressions;

namespace Ivy.PhoneBridge;

public sealed record MicroSettings(string UsbipExecutable, string ExecutableHash, int Port) {
    public void Validate() {
        if (UsbipExecutable == null || UsbipExecutable.Length > 32767 || !Path.IsPathFullyQualified(UsbipExecutable) ||
            !string.Equals(Path.GetFileName(UsbipExecutable), "usbip.exe", StringComparison.OrdinalIgnoreCase) ||
            ExecutableHash == null || !Regex.IsMatch(ExecutableHash, "^sha256:[0-9a-f]{64}$", RegexOptions.CultureInvariant) ||
            Port is < 1024 or > 65535) throw new ArgumentException("Pinned installed USBip client and bounded port required.");
    }
}
public sealed record MicroClientResult(bool Started, bool Completed, int? ExitCode, string Output, string ErrorCode);

// The driver is installed/verified separately. Ordinary runtime calls never install drivers,
// import certificates, request elevation, change persistence, or stop unrelated attachments.
public sealed class MicroUsbClient(MicroSettings settings) {
    public async Task<MicroClientResult> Run(string[] arguments, CancellationToken cancellation) {
        settings.Validate();
        using var process = new Process(); bool started = false;
        try {
            var pins = MicroClientFiles.ReleasePins();
            if (settings.ExecutableHash != "sha256:" + pins["usbip.exe"]) return new(false, false, null, "", "micro_client_changed");
            using var clientFiles = await MicroClientFiles.Acquire(Path.GetDirectoryName(settings.UsbipExecutable), pins, cancellation);
            cancellation.ThrowIfCancellationRequested();
            process.StartInfo = new ProcessStartInfo(settings.UsbipExecutable) {
                UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true,
                RedirectStandardError = true, WorkingDirectory = Path.GetDirectoryName(settings.UsbipExecutable)
            };
            foreach (var argument in arguments) process.StartInfo.ArgumentList.Add(argument);
            started = process.Start(); if (!started) return new(false, false, null, "", "micro_client_unavailable");
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            deadline.CancelAfter(TimeSpan.FromSeconds(10));
            var stdout = Bounded(process.StandardOutput, deadline.Token);
            var stderr = Bounded(process.StandardError, deadline.Token);
            try {
                await Task.WhenAll(process.WaitForExitAsync(deadline.Token), stdout, stderr);
                return new(true, true, process.ExitCode, await stdout, null);
            } catch {
                try { if (!process.HasExited) process.Kill(entireProcessTree: true); } catch { }
                using var drain = new CancellationTokenSource(TimeSpan.FromSeconds(2));
                try { await process.WaitForExitAsync(drain.Token); } catch { }
                deadline.Cancel();
                // Observe all pipe tasks without waiting indefinitely for another process's handles.
                _ = Task.WhenAll(stdout, stderr).ContinueWith(done => { _ = done.Exception; }, TaskScheduler.Default);
                return new(true, false, null, "", "micro_attach_unknown");
            }
        } catch (MicroClientChangedException) {
            return new(false, false, null, "", "micro_client_changed");
        } catch (Exception error) when (error is IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception or OperationCanceledException) {
            return new(started, false, null, "", started ? "micro_attach_unknown" : "micro_client_unavailable");
        }
    }
    private static async Task<string> Bounded(StreamReader reader, CancellationToken cancellation) {
        var result = new StringBuilder(); var buffer = new char[2048];
        for (;;) {
            int count = await reader.ReadAsync(buffer.AsMemory(), cancellation); if (count == 0) return result.ToString();
            if (result.Length + count > 65536) throw new IOException("Bounded USBip command output exceeded.");
            result.Append(buffer, 0, count);
        }
    }
}

public static class MicroPortInventory {
    // Pinned usbip-win2 0.9.8.0 port.cpp emits these fixed labels. Require the whole
    // endpoint, remote bus/device and explicit serial, never VID/PID or a substring alone.
    public static int? OwnPort(string output, int tcpPort) {
        if (output == null || output.Length > 65536) throw new ArgumentException("Bounded USBip inventory required.");
        if (string.IsNullOrWhiteSpace(output)) return null;
        var blocks = Regex.Matches(output, @"(?m)^Port ([0-9]{1,3}):[^\r\n]*\r?$", RegexOptions.CultureInvariant);
        if (blocks.Count == 0) throw new ArgumentException("Unrecognized USBip inventory.");
        int? selected = null; var ports = new HashSet<int>();
        string expected = "usbip://127.0.0.1:" + tcpPort + "/" + MicroUsbDescriptors.BusId;
        for (int index = 0; index < blocks.Count; index++) {
            var match = blocks[index]; int port = int.Parse(match.Groups[1].Value, System.Globalization.CultureInfo.InvariantCulture);
            if (port is < 1 or > 255 || !ports.Add(port)) throw new ArgumentException("Invalid/duplicate USBip port.");
            int end = index + 1 < blocks.Count ? blocks[index + 1].Index : output.Length;
            string block = output[match.Index..end];
            var locations = Regex.Matches(block, @"(?m)^\s*-> (usbip://[^\r\n]+)\r?$", RegexOptions.CultureInvariant);
            if (locations.Count != 1) throw new ArgumentException("Exact USBip location required.");
            if (locations[0].Groups[1].Value.TrimEnd('\r') != expected) continue;
            if (selected != null || !Regex.IsMatch(block, @"(?m)^\s*-> remote bus/dev: 001/007\r?$", RegexOptions.CultureInvariant) ||
                !Regex.IsMatch(block, @"(?m)^\s*-> serial: " + MicroUsbDescriptors.Serial + @"\r?$", RegexOptions.CultureInvariant))
                throw new ArgumentException("Conflicting Micro attachment at the selected endpoint.");
            selected = port;
        }
        return selected;
    }
}
