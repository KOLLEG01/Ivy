namespace Ivy.PhoneBridge;

public sealed record MicroAttachmentStatus(string State, long? Generation, string ErrorCode);

// Resource supervision only. Microphone operations remain in the original call's RPC ledger;
// discovery/reattachment never replay input. No persistent driver stash or second supervisor.
public sealed class MicroAttachment : IMicroVoiceInput {
    private readonly MicroSettings settings;
    private MicroUsbDevice device;
    private readonly Action<string> diagnostic;
    private readonly Func<string[], CancellationToken, Task<MicroClientResult>> execute;
    private readonly CancellationTokenSource stop = new();
    private readonly Task monitoring;
    private readonly object sync = new();
    private Task disposal;
    private MicroAttachmentStatus observed = new("starting", null, null);
    public MicroAttachment(MicroSettings settings, Func<string[], CancellationToken, Task<MicroClientResult>> execute = null, Action<string> diagnostic = null) {
        settings.Validate(); this.settings = settings; this.diagnostic = diagnostic;
        this.execute = execute ?? new MicroUsbClient(settings).Run;
        try { device = new(settings.Port, diagnostic); }
        catch { stop.Dispose(); throw; }
        monitoring = Monitor();
    }
    public MicroAttachmentStatus Status {
        get {
            var value = Volatile.Read(ref observed);
            var currentDevice = Volatile.Read(ref device);
            if (value.State is "attached" or "ready") {
                var generation = currentDevice.ReadyGeneration;
                return !currentDevice.Healthy ? new("unavailable", null, "micro_transport_failed") :
                    new(generation == null ? "attached" : "ready", generation, null);
            }
            return value;
        }
    }
    private void State(string state, string code = null) => Volatile.Write(ref observed, new(state, null, code));
    public MicroInputReceipt SendMicrophone(bool pressed, long generation, Func<bool> original) {
        if (stop.IsCancellationRequested || Status.State != "ready") return new("not_submitted", pressed, generation);
        return Volatile.Read(ref device).SendMicrophone(pressed, generation, () => !stop.IsCancellationRequested && original());
    }
    public MicroVoiceObservation ObserveVoice(long generation) => Volatile.Read(ref device).ObserveVoice(generation);
    private async Task Monitor() {
        bool unknown = false; int delayMs = 1000;
        try {
            while (!stop.IsCancellationRequested) {
                try {
                if (!Volatile.Read(ref device).Healthy) {
                    State("unavailable", "micro_transport_failed");
                    var previous = Volatile.Read(ref device);
                    long generation = previous.LastGeneration;
                    try { await previous.DisposeAsync(); } catch { /* Listener is closed before its task failures surface. */ }
                    if (stop.IsCancellationRequested) break;
                    try {
                        Volatile.Write(ref device, new MicroUsbDevice(settings.Port, diagnostic, generation));
                        State("starting"); delayMs = 1000;
                    } catch (Exception error) when (error is IOException or UnauthorizedAccessException or System.Net.Sockets.SocketException) {
                        State("unavailable", "micro_transport_failed");
                        await Task.Delay(delayMs, stop.Token); delayMs = Math.Min(30000, delayMs * 2);
                        continue;
                    }
                }
                MicroClientResult inventory;
                try { inventory = await execute(["port"], stop.Token); }
                catch (OperationCanceledException) when (stop.IsCancellationRequested) { break; }
                catch {
                    State(unknown ? "outcome_unknown" : "unavailable", unknown ? "micro_attach_unknown" : "micro_query_failed");
                    await Task.Delay(delayMs, stop.Token); delayMs = Math.Min(30000, delayMs * 2);
                    continue;
                }
                if (!inventory.Completed || inventory.ExitCode != 0) {
                    State(unknown ? "outcome_unknown" : "unavailable", unknown ? "micro_attach_unknown" : inventory.ErrorCode ?? "micro_query_failed");
                } else {
                    int? existing;
                    try { existing = MicroPortInventory.OwnPort(inventory.Output, settings.Port); }
                    catch (ArgumentException) {
                        State("unavailable", "micro_attachment_conflict");
                        await Task.Delay(delayMs, stop.Token); delayMs = Math.Min(30000, delayMs * 2);
                        continue;
                    }
                    if (existing != null) {
                        unknown = false; State("attached"); delayMs = 1000;
                        await Task.Delay(15000, stop.Token); continue;
                    }
                    if (unknown) State("outcome_unknown", "micro_attach_unknown");
                    else {
                        // A normal successful attach cancels an older driver reattach request for
                        // the same endpoint. --once suppresses that cancellation in usbip-win2 0.9.8
                        // and therefore accumulates reconnects across PhoneBridge rollouts.
                        MicroClientResult attached;
                        try {
                            attached = await execute(["--tcp-port", settings.Port.ToString(System.Globalization.CultureInfo.InvariantCulture),
                                "attach", "--remote", "127.0.0.1", "--bus-id", MicroUsbDescriptors.BusId,
                                "--serial", MicroUsbDescriptors.Serial, "--receive-mode", "low-latency"], stop.Token);
                        } catch (OperationCanceledException) when (stop.IsCancellationRequested) { break; }
                        catch {
                            unknown = true;
                            State("outcome_unknown", "micro_attach_unknown");
                            await Task.Delay(delayMs, stop.Token); delayMs = Math.Min(30000, delayMs * 2);
                            continue;
                        }
                        unknown = attached.Started && !attached.Completed;
                        // Even exit0 is confirmed by inventory on the next tick before granting readiness.
                        State(unknown ? "outcome_unknown" : "starting", unknown ? "micro_attach_unknown" :
                            attached.Completed && attached.ExitCode == 0 ? null : attached.ErrorCode ?? "micro_attach_failed");
                    }
                }
                await Task.Delay(delayMs, stop.Token); delayMs = Math.Min(30000, delayMs * 2);
                } catch (OperationCanceledException) when (stop.IsCancellationRequested) { break; }
                  catch {
                    State("unavailable", "micro_transport_failed");
                    await Task.Delay(delayMs, stop.Token); delayMs = Math.Min(30000, delayMs * 2);
                  }
            }
        } catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
        catch { State("unavailable", "micro_transport_failed"); }
    }
    public ValueTask DisposeAsync() {
        lock (sync) return new(disposal ??= DisposeCore());
    }
    private async Task DisposeCore() {
        stop.Cancel();
        // Closing only our own TCP listener/imports lets the driver unplug this device.
        // No port-number detach race can accidentally remove a different device after port reuse.
        try { await monitoring; await Volatile.Read(ref device).DisposeAsync(); State("closed"); }
        catch { State("outcome_unknown", "micro_cleanup_unknown"); throw; }
        finally { stop.Dispose(); }
    }
}
