using Ivy.PhoneBridge;

static partial class Program {
    // Explicit installed-Windows entrypoint. Passive process-loopback only; no playback or files.
    static async Task InstalledLoopbackProbe() {
        var began = System.Diagnostics.Stopwatch.StartNew();
        var interrupted = await PhoneLoopbackProbe.RunAsync(() => began.ElapsedMilliseconds < 150, CancellationToken.None);
        Check(interrupted.State == "interrupted" && !interrupted.Detected, "new positive work interrupts the original passive probe");
        using (var cancelled = new CancellationTokenSource(150)) {
            await ThrowsAsync<OperationCanceledException>(PhoneLoopbackProbe.RunAsync(() => true, cancelled.Token),
                "owner shutdown cancels and disposes the original recorder");
        }
        var complete = await PhoneLoopbackProbe.RunAsync(() => true, CancellationToken.None);
        Check(complete.State == "completed" && complete.Samples is >= 0 and <= 960000 && complete.Peak is >= 0 and <= 1 &&
            complete.ExcludedProcessId == Environment.ProcessId && complete.SampleRate == 48000 && complete.Channels == 2,
            "a fresh complete probe works after interruption and cancellation, with bounded level-only results");
        Console.WriteLine("phone_loopback_probe_passed: actual Windows recorder completion/interruption/cancellation/reopen; no playback or retained audio");
    }
}
