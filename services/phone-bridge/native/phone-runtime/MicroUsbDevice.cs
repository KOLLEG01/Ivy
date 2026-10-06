using System.Net;
using System.Net.Sockets;

namespace Ivy.PhoneBridge;

// Runtime-owned loopback server. No driver installation, scheduler or Desktop lifecycle work.
public sealed class MicroUsbDevice : IAsyncDisposable {
    private readonly object sync = new();
    private readonly TcpListener listener;
    private readonly CancellationTokenSource stop = new();
    private readonly HashSet<MicroUsbSession> sessions = new();
    private readonly HashSet<Task> completions = new();
    private readonly Task accepting, heartbeat;
    private MicroUsbSession current;
    private long generation;
    private readonly Action<string> diagnostic;
    private bool closed;
    public int Port => ((IPEndPoint)listener.LocalEndpoint).Port;
    public long LastGeneration { get { lock (sync) return generation; } }
    public bool Healthy { get { lock (sync) return !closed && !accepting.IsFaulted && !heartbeat.IsFaulted; } }
    public MicroUsbDevice(int port, Action<string> diagnostic = null, long initialGeneration = 0) {
        this.diagnostic = diagnostic;
        if (port is < 0 or > 65535 || initialGeneration is < 0 or > 2097151) throw new ArgumentException("Valid loopback port and generation required.");
        generation = initialGeneration;
        listener = new TcpListener(IPAddress.Loopback, port); listener.Server.ExclusiveAddressUse = true;
        try { listener.Start(4); }
        catch { listener.Stop(); stop.Dispose(); throw; }
        accepting = Accept(); heartbeat = Heartbeat();
    }
    public long? ReadyGeneration {
        get { lock (sync) return !closed && current?.Ready == true ? current.Generation : null; }
    }
    public MicroInputReceipt SendMicrophone(bool pressed, long expectedGeneration, Func<bool> original) {
        // Import replacement and the actual report write share one linearization boundary.
        // No old socket can receive a new event after another import became current.
        lock (sync) {
            var selected = closed || current?.Generation != expectedGeneration ? null : current;
            return selected?.SendMicrophone(pressed, expectedGeneration, original) ?? new("not_submitted", pressed, expectedGeneration);
        }
    }
    public MicroVoiceObservation ObserveVoice(long expectedGeneration) {
        lock (sync) {
            var selected = closed || current?.Generation != expectedGeneration ? null : current;
            return selected?.ObserveVoice(expectedGeneration) ?? new("unavailable", expectedGeneration, 0);
        }
    }
    private bool Activate(MicroUsbSession session) {
        MicroUsbSession previous;
        lock (sync) {
            if (closed || !sessions.Contains(session)) return false;
            previous = current; current = session;
        }
        // Do not nest the global lock with a session lock: close can complete an old read.
        if (previous != session) previous?.Close(); return true;
    }
    private async Task Accept() {
        try {
            while (!stop.IsCancellationRequested) {
                var client = await listener.AcceptTcpClientAsync(stop.Token);
                client.NoDelay = true; client.SendTimeout = 1000;
                lock (sync) {
                    if (closed || sessions.Count == 4) { client.Dispose(); continue; }
                    if (generation == 2097151) { client.Dispose(); throw new IOException("Micro connection generation limit reached."); }
                    var session = new MicroUsbSession(client.GetStream(), ++generation, Activate, diagnostic: diagnostic);
                    sessions.Add(session);
                    // Defer execution so completion cannot precede its registration in the set.
                    var work = Task.Run(async () => {
                        try { await session.RunAsync(); }
                        finally {
                            await session.DisposeAsync(); client.Dispose();
                            lock (sync) { sessions.Remove(session); if (current == session) current = null; }
                        }
                    });
                    completions.Add(work);
                    _ = work.ContinueWith(done => { lock (sync) completions.Remove(done); }, CancellationToken.None,
                        TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
                }
            }
        } catch (Exception error) when (error is OperationCanceledException or ObjectDisposedException or SocketException && stop.IsCancellationRequested) { }
    }
    private async Task Heartbeat() {
        try {
            using var timer = new PeriodicTimer(TimeSpan.FromSeconds(30));
            while (await timer.WaitForNextTickAsync(stop.Token)) {
                MicroUsbSession[] snapshot; lock (sync) snapshot = sessions.ToArray();
                foreach (var session in snapshot) session.CheckHeartbeat();
            }
        } catch (OperationCanceledException) { }
    }
    public async ValueTask DisposeAsync() {
        MicroUsbSession[] snapshot;
        lock (sync) {
            if (closed) return;
            closed = true; stop.Cancel(); listener.Stop(); current = null; snapshot = sessions.ToArray();
        }
        foreach (var session in snapshot) session.Close();
        await Task.WhenAll(accepting, heartbeat);
        Task[] pending; lock (sync) pending = completions.ToArray();
        await Task.WhenAll(pending); stop.Dispose();
    }
}
