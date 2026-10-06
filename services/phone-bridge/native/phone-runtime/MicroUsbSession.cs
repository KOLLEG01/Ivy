using System.Buffers.Binary;
using System.Text;

namespace Ivy.PhoneBridge;

public sealed record MicroInputReceipt(string Phase, bool Pressed, long Generation);
public sealed record MicroVoiceObservation(string State, long Generation, long DictationSequence);

// One imported USB/IP connection. Queues contain protocol replies only, never future key input.
// Wire layout derives from codexdeck d8d8e737; see licenses/CodexMicro-MIT.txt.
public sealed class MicroUsbSession : IAsyncDisposable {
    private readonly object sync = new();
    private readonly Stream stream;
    private readonly CancellationTokenSource stop = new();
    private readonly Func<MicroUsbSession, bool> activate;
    private readonly TimeProvider time;
    private readonly Action<string> diagnostic;
    private string lastTransfer = "import";
    private MicroProtocol protocol = new();
    private readonly Queue<uint> reads = new();
    private readonly Queue<Group> replies = new();
    private Task running;
    private bool imported, closed;
    private long lastStatus, lastInbound, generation;
    private readonly long created;
    private int reportCount;
    private sealed class Group(byte[][] reports, long created) {
        public readonly Queue<byte[]> Reports = new(reports);
        public readonly long Created = created;
        public bool Started;
    }
    public long Generation { get { lock (sync) return generation; } }
    public MicroVoiceObservation ObserveVoice(long expectedGeneration) {
        lock (sync) return closed || expectedGeneration != Generation
            ? new("unavailable", Generation, 0)
            : new(protocol.LightingState, Generation, protocol.DictationSequence);
    }
    public MicroUsbSession(Stream stream, long generation, Func<MicroUsbSession, bool> activate, TimeProvider time = null, Action<string> diagnostic = null) {
        ArgumentNullException.ThrowIfNull(stream); ArgumentNullException.ThrowIfNull(activate);
        // Connection + HID epoch remain exactly representable in the JSON status/receipt contract.
        if (generation is <= 0 or > 2097151) throw new ArgumentException("Bounded positive connection generation required.");
        this.stream = stream; this.generation = generation << 32; this.activate = activate; this.time = time ?? TimeProvider.System;
        this.diagnostic = diagnostic;
        created = this.time.GetTimestamp();
    }
    public bool Ready { get { lock (sync) return !closed && imported && protocol.SawRgbConfiguration && protocol.SawStatus &&
        time.GetElapsedTime(lastStatus) < TimeSpan.FromMinutes(4); } }
    // Never enter the import callback while retaining this session lock: device-level
    // dispatch/status use the consistent device -> session lock order.
    public Task RunAsync() { lock (sync) return running ??= Task.Run(Run); }
    public void Close() {
        lock (sync) {
            if (closed) return;
            closed = true; stop.Cancel(); stream.Dispose(); reads.Clear(); replies.Clear(); reportCount = 0;
        }
    }
    private static uint U32(ReadOnlySpan<byte> bytes, int offset) => BinaryPrimitives.ReadUInt32BigEndian(bytes[offset..]);
    private static ushort U16(ReadOnlySpan<byte> bytes, int offset) => BinaryPrimitives.ReadUInt16BigEndian(bytes[offset..]);
    private async Task<byte[]> Read(int length) {
        var value = new byte[length];
        await stream.ReadExactlyAsync(value, stop.Token); return value;
    }
    private void Write(byte[] value) {
        if (closed) throw new IOException("Micro connection closed.");
        stream.Write(value); // The owning network socket sets a finite send timeout.
    }
    private void Return(uint sequence, int status, byte[] payload = null, uint command = 3) {
        var result = new byte[48 + (payload?.Length ?? 0)];
        BinaryPrimitives.WriteUInt32BigEndian(result, command);
        BinaryPrimitives.WriteUInt32BigEndian(result.AsSpan(4), sequence);
        BinaryPrimitives.WriteInt32BigEndian(result.AsSpan(20), status);
        if (payload != null) {
            BinaryPrimitives.WriteUInt32BigEndian(result.AsSpan(24), (uint)payload.Length);
            payload.CopyTo(result, 48);
        }
        Write(result);
    }
    private byte[] NextReport() {
        while (replies.TryPeek(out var group)) {
            if (time.GetElapsedTime(group.Created) >= TimeSpan.FromSeconds(2)) {
                // Never splice a new response onto an old partially delivered JSON message.
                if (group.Started) throw new IOException("Partially delivered Micro response expired.");
                reportCount -= group.Reports.Count; replies.Dequeue(); continue;
            }
            group.Started = true; var report = group.Reports.Dequeue(); reportCount--;
            if (group.Reports.Count == 0) replies.Dequeue(); return report;
        }
        return null;
    }
    private void Inbound(byte[] report) {
        // Only a complete new status RPC is a heartbeat, not another fragment after a prior one.
        long before = protocol.StatusRequests;
        var response = protocol.Receive(report); lastInbound = time.GetTimestamp();
        if (protocol.StatusRequests != before) lastStatus = lastInbound;
        if (response.Length == 0) return;
        // Each newline terminates one RPC response; expiry/cancellation never splits groups.
        var group = new List<byte[]>();
        foreach (var frame in response) {
            group.Add(frame);
            if (frame[2] > 0 && frame[2] + 2 < frame.Length && frame[frame[2] + 2] == 10) {
                if (reportCount + group.Count > 128) throw new IOException("Micro response backlog limit.");
                replies.Enqueue(new(group.ToArray(), time.GetTimestamp())); reportCount += group.Count; group.Clear();
            }
        }
        if (group.Count > 0) throw new IOException("Unterminated Micro response.");
        while (reads.Count > 0) {
            var frame = NextReport(); if (frame == null) break;
            Return(reads.Dequeue(), 0, frame);
        }
    }
    public MicroInputReceipt SendMicrophone(bool pressed, long expectedGeneration, Func<bool> current) {
        lock (sync) {
            bool writing = false;
            try {
                // No event queue: a reopened HID handle cannot receive a stale microphone action.
                if (expectedGeneration != Generation || !Ready || !current() || reads.Count == 0 || reportCount != 0)
                    return new("not_submitted", pressed, Generation);
                var frame = MicroProtocol.Microphone(pressed);
                if (frame.Length != 1) throw new InvalidOperationException("Microphone event must fit one report.");
                uint sequence = reads.Dequeue(); writing = true; Return(sequence, 0, frame[0]);
                return new("submitted", pressed, Generation);
            } catch {
                Close(); return new(writing ? "outcome_unknown" : "not_submitted", pressed, Generation);
            }
        }
    }
    private async Task Run() {
        try {
            while (!stop.IsCancellationRequested) {
                if (!imported) {
                    var op = await Read(8);
                    if (U16(op, 0) != 0x0111 || U32(op, 4) != 0) throw new IOException("Invalid USB/IP operation header.");
                    ushort kind = U16(op, 2);
                    if (kind == 0x8005) {
                        var reply = new byte[328];
                        BinaryPrimitives.WriteUInt16BigEndian(reply, 0x0111);
                        BinaryPrimitives.WriteUInt16BigEndian(reply.AsSpan(2), 5);
                        BinaryPrimitives.WriteUInt32BigEndian(reply.AsSpan(8), 1);
                        MicroUsbDescriptors.Export().CopyTo(reply, 12); reply[324] = 3;
                        lock (sync) Write(reply);
                    } else if (kind == 0x8003) {
                        var bus = await Read(32);
                        var expected = new byte[32]; Encoding.ASCII.GetBytes(MicroUsbDescriptors.BusId).CopyTo(expected, 0);
                        if (!bus.SequenceEqual(expected) || !activate(this)) throw new IOException("USB/IP import refused.");
                        lock (sync) {
                            var reply = new byte[320]; BinaryPrimitives.WriteUInt16BigEndian(reply, 0x0111);
                            BinaryPrimitives.WriteUInt16BigEndian(reply.AsSpan(2), 3);
                            MicroUsbDescriptors.Export().CopyTo(reply, 8); Write(reply); imported = true;
                            lastStatus = lastInbound = time.GetTimestamp();
                        }
                    } else throw new IOException("Unsupported USB/IP operation.");
                    continue;
                }
                var header = await Read(48);
                uint command = U32(header, 0), sequence = U32(header, 4), direction = U32(header, 12), endpoint = U32(header, 16);
                if (diagnostic != null) lastTransfer = $"command={command} device={U32(header, 8)} direction={direction} endpoint={endpoint} length={U32(header, 24)} packets={U32(header, 32)} setup={Convert.ToHexString(header.AsSpan(40, 8))}";
                if (U32(header, 8) != MicroUsbDescriptors.DeviceId || direction > 1 || endpoint > 1)
                    throw new IOException("Invalid USB/IP transfer identity.");
                if (command == 2) {
                    lock (sync) {
                        uint victim = U32(header, 20); bool removed = reads.Contains(victim);
                        var remaining = reads.Where(value => value != victim).ToArray(); reads.Clear(); foreach (var value in remaining) reads.Enqueue(value);
                        Return(sequence, removed ? -104 : 0, command: 4);
                        if (removed && reads.Count == 0) {
                            // The host closed/cancelled its final HID reader. A reopen gets a new
                            // input generation even when the kernel keeps this USB/IP connection.
                            if ((generation & uint.MaxValue) == uint.MaxValue) throw new IOException("Micro generation exhausted.");
                            generation++; protocol = new(); replies.Clear(); reportCount = 0;
                            lastStatus = lastInbound = time.GetTimestamp();
                        }
                    }
                    continue;
                }
                uint length = U32(header, 24);
                // Windows USB/IP marks non-isochronous URBs with -1; Linux peers use 0.
                if (command != 1 || length > 4096 || U32(header, 32) is not (0 or uint.MaxValue)) throw new IOException("Unsupported USB/IP transfer.");
                var payload = direction == 0 ? await Read((int)length) : [];
                lock (sync) {
                    if (endpoint == 0) Control(sequence, direction, length, header.AsSpan(40, 8), payload);
                    else if (length != 64) throw new IOException("Exact HID report transfer required.");
                    else if (direction == 0) { Inbound(payload); Return(sequence, 0); }
                    else {
                        if (reads.Count >= 32 || reads.Contains(sequence)) throw new IOException("Pending HID read limit or duplicate sequence.");
                        var report = NextReport(); if (report != null) Return(sequence, 0, report); else reads.Enqueue(sequence);
                    }
                }
            }
        } catch (Exception error) when (error is IOException or ObjectDisposedException or OperationCanceledException or ArgumentException) {
            if (!stop.IsCancellationRequested) try { diagnostic?.Invoke(lastTransfer + ": " + error.Message); } catch { }
            // Lost connection is a new attachment opportunity, never an input replay.
        } finally { Close(); }
    }
    private void Control(uint sequence, uint direction, uint capacity, ReadOnlySpan<byte> setup, byte[] payload) {
        byte kind = setup[0], request = setup[1]; ushort value = BinaryPrimitives.ReadUInt16LittleEndian(setup[2..]);
        ushort index = BinaryPrimitives.ReadUInt16LittleEndian(setup[4..]);
        ushort length = BinaryPrimitives.ReadUInt16LittleEndian(setup[6..]);
        if ((kind >> 7) != direction || length > capacity || direction == 0 && payload.Length != length)
            throw new IOException("Inconsistent USB setup transfer.");
        byte[] descriptor = null;
        if (kind == 0x80 && request == 6) descriptor = (value >> 8) switch {
            1 => MicroUsbDescriptors.Device(), 2 => MicroUsbDescriptors.Configuration(), 3 => MicroUsbDescriptors.String(value & 255), _ => null
        };
        else if (kind == 0x81 && request == 6 && index == 0 && value == 0x2200) descriptor = MicroUsbDescriptors.Report();
        else if (kind == 0x81 && request == 6 && index == 0 && value == 0x2100) descriptor = MicroUsbDescriptors.Configuration()[18..27];
        else if (kind is 0x80 or 0x81 or 0x82 && request == 0 && value == 0 && length == 2 && index is 0 or 1 or 0x81) descriptor = [0, 0];
        else if (kind == 0x80 && request == 8 && value == 0 && index == 0 && length == 1) descriptor = [1];
        else if (kind == 0x81 && request == 10 && value == 0 && index == 0 && length == 1) descriptor = [0];
        else if (kind == 0xa1 && index == 0 && length == 1 && request is 2 or 3) descriptor = [(byte)(request == 3 ? 1 : 0)];
        else if (kind == 0x21 && request == 9 && index == 0 && value == 0x0206 && length == 64 && payload.Length == 64) {
            Inbound(payload); Return(sequence, 0); return;
        } else if (direction == 0 && length == 0 && ((kind == 0 && index == 0 && request is 5 or 9) ||
            (kind == 0x21 && index == 0 && request is 10 or 11) || (kind == 1 && request == 11 && value == 0 && index == 0) ||
            (kind == 2 && request == 1 && value == 0 && index is 1 or 0x81))) {
            Return(sequence, 0); return;
        }
        if (descriptor == null) { Return(sequence, -32); return; }
        Return(sequence, 0, descriptor.Take(length).ToArray());
    }
    public void CheckHeartbeat() {
        lock (sync) if (!closed && ((!imported && time.GetElapsedTime(created) > TimeSpan.FromSeconds(10)) ||
            (imported && time.GetElapsedTime(lastInbound) < TimeSpan.FromMinutes(4) && time.GetElapsedTime(lastStatus) >= TimeSpan.FromMinutes(4)))) Close();
    }
    public async ValueTask DisposeAsync() {
        Close(); Task task; lock (sync) task = running;
        if (task != null) await task; stop.Dispose();
    }
}
