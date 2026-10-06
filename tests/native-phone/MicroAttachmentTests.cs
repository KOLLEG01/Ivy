using System.Net;
using System.Net.Sockets;
using Ivy.PhoneBridge;

static partial class Program {
    static string MicroInventory(int port, string serial = "IvyPhoneMicro", int hubPort = 3) =>
        $"Imported USB devices\n====================\nPort {hubPort:00}: device in use at high-speed\n         Codex Micro\n           -> usbip://127.0.0.1:{port}/1-7\n           -> remote bus/dev: 001/007\n           -> serial: {serial}\n           -> mode: low-latency\n";
    static async Task MicroAttachments() {
        await MicroClientFileUnits();
        Check(System.Text.RegularExpressions.Regex.IsMatch(MicroUsbDescriptors.Serial, "^[A-Za-z0-9]{1,15}$"),
            "USB/IP serial satisfies the installed client's CLI contract before any attachment");
        Check(System.Text.Encoding.Unicode.GetString(MicroUsbDescriptors.String(3)[2..]) == MicroUsbDescriptors.Serial,
            "USB descriptor and client attachment retain the same serial");
        Check(MicroPortInventory.OwnPort("", 3241) == null, "successful empty inventory permits initial attach");
        Check(MicroPortInventory.OwnPort(MicroInventory(3241), 3241) == 3, "complete own endpoint/serial identity");
        Check(MicroPortInventory.OwnPort(MicroInventory(3240), 3241) == null, "another emulation port is not ours");
        Check(MicroPortInventory.OwnPort(MicroInventory(3241).Replace("127.0.0.1", "127.0.0.10"), 3241) == null, "hostname prefix cannot establish ownership");
        foreach (var invalid in new[] { "unexpected output", MicroInventory(3241, "foreign"),
            MicroInventory(3241).Replace("001/007", "001/001"), MicroInventory(3241) + MicroInventory(3241, hubPort: 4) })
            Reject(() => MicroPortInventory.OwnPort(invalid, 3241), "ambiguous/conflicting inventory is not an absent device");

        static MicroSettings Settings() {
            var probe = new TcpListener(IPAddress.Loopback, 0); probe.Start();
            int port = ((IPEndPoint)probe.LocalEndpoint).Port; probe.Stop();
            return new(Path.Combine(Path.GetTempPath(), "usbip.exe"), "sha256:" + new string('0', 64), port);
        }
        static async Task Until(Func<bool> condition) {
            using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(8));
            while (!condition()) await Task.Delay(10, limit.Token);
        }
        var settings = Settings(); var commands = new List<string[]>(); bool attached = false;
        var occupied = new TcpListener(IPAddress.Loopback, settings.Port); occupied.Start();
        try {
            bool refused = false;
            try { await using var unexpected = new MicroAttachment(settings); }
            catch (SocketException) { refused = true; }
            Check(refused && occupied.Server.IsBound, "conflicting listener is refused without closing the existing owner");
        } finally { occupied.Stop(); }
        using var commandSync = new SemaphoreSlim(1);
        async Task<MicroClientResult> Execute(string[] args, CancellationToken token) {
            await commandSync.WaitAsync(token);
            try {
                commands.Add((string[])args.Clone());
                if (args.SequenceEqual(new[] { "port" })) return new(true, true, 0, attached ? MicroInventory(settings.Port) : "", null);
                Check(args.SequenceEqual(new[] { "--tcp-port", settings.Port.ToString(), "attach", "--remote", "127.0.0.1",
                    "--bus-id", "1-7", "--serial", "IvyPhoneMicro", "--receive-mode", "low-latency" }), "only one exact nonpersistent attach action that cancels older driver retries");
                attached = true; return new(true, true, 0, "succesfully attached to port 3", null);
            } finally { commandSync.Release(); }
        }
        await using (var owner = new MicroAttachment(settings, Execute)) {
            await Until(() => owner.Status.State == "attached");
            Check(owner.Status.Generation == null, "driver inventory is not a completed ui handshake");
            await using var peer = new MicroPeer(); await peer.Connect(settings.Port);
            await peer.Rpc(1, "v.oai.rgbcfg"); await peer.Rpc(2, "device.status");
            Check(owner.Status.State == "ready" && owner.Status.Generation != null, "inventory plus actual HID/RPC handshake admits input");
        }
        Check(commands.Count(args => args.Contains("attach")) == 1 && commands.All(args => !args.Contains("detach") && !args.Contains("--stop")),
            "shutdown never acts on a possibly reused driver port or unrelated device");

        int attempts = 0, queries = 0; var unknownSettings = Settings();
        await using (var owner = new MicroAttachment(unknownSettings, (args, _) => {
            if (args.SequenceEqual(new[] { "port" })) { Interlocked.Increment(ref queries); return Task.FromResult(new MicroClientResult(true, true, 0, "", null)); }
            Interlocked.Increment(ref attempts); return Task.FromResult(new MicroClientResult(true, false, null, "", "micro_attach_unknown"));
        })) {
            await Until(() => Volatile.Read(ref queries) >= 2);
            Check(owner.Status.State == "outcome_unknown" && Volatile.Read(ref attempts) == 1, "unknown attach permits inventory only, never another attach attempt");
        }
        int collisionEffects = 0; var collisionSettings = Settings();
        await using (var owner = new MicroAttachment(collisionSettings, (args, _) => {
            if (!args.SequenceEqual(new[] { "port" })) Interlocked.Increment(ref collisionEffects);
            return Task.FromResult(new MicroClientResult(true, true, 0, MicroInventory(collisionSettings.Port, "another-owner"), null));
        })) {
            await Until(() => owner.Status.ErrorCode == "micro_attachment_conflict");
            Check(collisionEffects == 0, "conflicting inventory never becomes attach or detach authority");
        }
        var recovering = Settings(); int recoveryReads = 0, recoveryAttaches = 0;
        await using (var owner = new MicroAttachment(recovering, (args, _) => {
            if (!args.SequenceEqual(new[] { "port" })) { Interlocked.Increment(ref recoveryAttaches); throw new Exception("unexpected attach"); }
            int attempt = Interlocked.Increment(ref recoveryReads);
            if (attempt == 1) throw new IOException("transient inventory read");
            return Task.FromResult(new MicroClientResult(true, true, 0,
                attempt == 2 ? "partial output" : MicroInventory(recovering.Port), null));
        })) {
            await Until(() => owner.Status.State == "attached");
            Check(recoveryReads >= 3 && recoveryAttaches == 0,
                "transient and malformed inventory recovers by observation without another attach");
        }
        var malformedResult = Settings(); int malformedReads = 0;
        await using (var owner = new MicroAttachment(malformedResult, (args, _) => {
            Check(args.SequenceEqual(new[] { "port" }), "unexpected monitor failure must never infer attachment authority");
            return Task.FromResult<MicroClientResult>(Interlocked.Increment(ref malformedReads) == 1
                ? null! : new(true, true, 0, MicroInventory(malformedResult.Port), null));
        })) {
            await Until(() => owner.Status.State == "attached");
            Check(malformedReads >= 2, "unexpected monitor failure is reported and observed again");
        }
        var missing = Settings();
        var unavailable = await new MicroUsbClient(missing).Run(["port"], CancellationToken.None);
        Check(!unavailable.Started && !unavailable.Completed, "missing pinned client produces no process effect");
        string scratch = Directory.CreateTempSubdirectory("ivy-micro-client-").FullName;
        string executable = Path.Combine(scratch, "usbip.exe");
        try {
            await File.WriteAllTextAsync(executable, "not the approved USBip client");
            var changed = await new MicroUsbClient(missing with { UsbipExecutable = executable }).Run(["port"], CancellationToken.None);
            Check(!changed.Started && changed.ErrorCode == "micro_client_changed", "hash mismatch prevents execution of substituted client bytes");
        } finally { File.Delete(executable); Directory.Delete(scratch); }
    }
}
