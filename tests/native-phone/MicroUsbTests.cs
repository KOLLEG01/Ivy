using System.Buffers.Binary;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using Ivy.PhoneBridge;

static partial class Program {
    sealed class MicroPeer : IAsyncDisposable {
        readonly TcpClient client = new();
        NetworkStream stream = null!;
        uint next;
        public async Task Connect(int port) {
            await client.ConnectAsync(IPAddress.Loopback, port); stream = client.GetStream();
            var request = new byte[40]; BinaryPrimitives.WriteUInt16BigEndian(request, 0x0111);
            BinaryPrimitives.WriteUInt16BigEndian(request.AsSpan(2), 0x8003);
            Encoding.ASCII.GetBytes(MicroUsbDescriptors.BusId).CopyTo(request, 8);
            await stream.WriteAsync(request); var reply = await Read(320);
            Check(BinaryPrimitives.ReadUInt16BigEndian(reply.AsSpan(2)) == 3 && BinaryPrimitives.ReadUInt32BigEndian(reply.AsSpan(4)) == 0,
                "actual loopback import succeeds");
        }
        async Task<byte[]> Read(int length) {
            var bytes = new byte[length]; using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(5));
            await stream.ReadExactlyAsync(bytes, deadline.Token); return bytes;
        }
        public async Task<(uint Sequence, int Status, byte[] Payload)> Reply() {
            var header = await Read(48); uint length = BinaryPrimitives.ReadUInt32BigEndian(header.AsSpan(24));
            Check(length <= 4096, "bounded wire response");
            return (BinaryPrimitives.ReadUInt32BigEndian(header.AsSpan(4)), BinaryPrimitives.ReadInt32BigEndian(header.AsSpan(20)), await Read((int)length));
        }
        public async Task<uint> Submit(uint direction, uint endpoint, uint length, byte[]? payload = null, byte[]? setup = null, uint? sequence = null, uint packets = uint.MaxValue) {
            var header = new byte[48]; uint id = sequence ?? ++next;
            BinaryPrimitives.WriteUInt32BigEndian(header, 1); BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(4), id);
            BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(8), MicroUsbDescriptors.DeviceId);
            BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(12), direction);
            BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(16), endpoint);
            BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(24), length);
            BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(32), packets);
            setup?.CopyTo(header, 40); await stream.WriteAsync(header);
            if (payload != null) await stream.WriteAsync(payload); return id;
        }
        public async Task DescriptorBarrier() {
            uint id = await Submit(1, 0, 18, setup: [0x80, 6, 0, 1, 0, 0, 18, 0]);
            var reply = await Reply(); Check(reply.Sequence == id && reply.Status == 0 && reply.Payload.SequenceEqual(MicroUsbDescriptors.Device()), "USB descriptor and processing barrier");
        }
        public async Task Rpc(int id, string method, bool control = false) {
            foreach (var frame in MicroProtocol.Frame(JsonSerializer.SerializeToUtf8Bytes(new { id, method }))) {
                uint sequence = await Submit(0, control ? 0u : 1u, 64, frame, control ? new byte[] { 0x21, 9, 6, 2, 0, 0, 64, 0 } : null);
                var ack = await Reply();
                Check(ack.Sequence == sequence && ack.Status == 0, "interrupt OUT acknowledgement");
            }
            var response = new List<byte>();
            do {
                uint sequence = await Submit(1, 1, 64); var reply = await Reply();
                Check(reply.Sequence == sequence && reply.Status == 0 && reply.Payload.Length == 64, "interrupt IN HID response");
                response.AddRange(reply.Payload.Skip(3).Take(reply.Payload[2]));
            } while (response[^1] != 10);
            using var json = JsonDocument.Parse(response.ToArray()); Check(json.RootElement.GetProperty("id").GetInt32() == id, "matching RPC response through USB/IP");
        }
        public async Task Cancel(uint victim) {
            var header = new byte[48]; uint id = ++next;
            BinaryPrimitives.WriteUInt32BigEndian(header, 2); BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(4), id);
            BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(8), MicroUsbDescriptors.DeviceId);
            BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(12), 1); BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(16), 1);
            BinaryPrimitives.WriteUInt32BigEndian(header.AsSpan(20), victim);
            await stream.WriteAsync(header); var reply = await Reply(); Check(reply.Sequence == id && reply.Status == -104, "exact original HID read cancelled");
            await DescriptorBarrier();
        }
        public async Task Closed() {
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(5));
            try { Check(await stream.ReadAsync(new byte[1], deadline.Token) == 0, "invalid transfer closes connection"); }
            catch (IOException) { /* TCP reset also closes the invalid stream. */ }
        }
        public ValueTask DisposeAsync() { client.Dispose(); return ValueTask.CompletedTask; }
    }
    static async Task MicroUsbChecks() {
        await using var device = new MicroUsbDevice(0);
        await using var first = new MicroPeer(); await first.Connect(device.Port); await first.DescriptorBarrier();
        Check(device.ReadyGeneration == null, "USB enumeration alone is not a completed control handshake");
        await first.Rpc(1, "v.oai.rgbcfg", control: true); await first.Rpc(2, "device.status");
        uint zeroPackets = await first.Submit(1, 0, 18, setup: [0x80, 6, 0, 1, 0, 0, 18, 0], packets: 0);
        Check((await first.Reply()).Sequence == zeroPackets, "zero and Windows -1 both identify non-isochronous transfers");
        long generation = device.ReadyGeneration ?? throw new Exception("handshake should be ready");
        Check(device.SendMicrophone(true, generation, () => true).Phase == "not_submitted", "no microphone buffering without a waiting reader");
        uint read = await first.Submit(1, 1, 64); await first.DescriptorBarrier();
        Check(device.SendMicrophone(true, generation, () => false).Phase == "not_submitted", "original call authority checked before any input");
        Check(device.SendMicrophone(true, generation, () => device.ReadyGeneration == generation).Phase == "submitted", "current microphone report and original-generation status check share a safe lock order");
        var pressed = await first.Reply();
        Check(pressed.Sequence == read && Encoding.UTF8.GetString(pressed.Payload.Skip(3).Take(pressed.Payload[2]).ToArray()).Contains("\"act\":1"), "correct waiting URB receives press");

        uint cancelled = await first.Submit(1, 1, 64); await first.DescriptorBarrier(); await first.Cancel(cancelled);
        Check(device.ReadyGeneration == null, "HID close clears handshake even while USB/IP remains connected");
        await first.Rpc(3, "v.oai.rgbcfg"); await first.Rpc(4, "device.status");
        long reopened = device.ReadyGeneration ?? throw new Exception("reopened HID should handshake");
        Check(reopened != generation, "same USB connection has a fresh HID input generation after cancellation");
        uint newRead = await first.Submit(1, 1, 64); await first.DescriptorBarrier();
        Check(device.SendMicrophone(false, generation, () => true).Phase == "not_submitted", "old release cannot reach a reopened HID handle");
        Check(device.SendMicrophone(false, reopened, () => true).Phase == "submitted", "new generation owns its release");
        Check((await first.Reply()).Sequence == newRead, "cancelled read never consumes a later frame");

        await using var replacement = new MicroPeer(); await replacement.Connect(device.Port); await replacement.DescriptorBarrier();
        await first.Closed();
        Check(device.SendMicrophone(true, reopened, () => true).Phase == "not_submitted", "replacement import cannot inherit input authority");
        await replacement.Rpc(5, "v.oai.rgbcfg"); await replacement.Rpc(6, "device.status");
        Check(device.ReadyGeneration != reopened, "new import has independent protocol state");
        await replacement.Submit(0, 1, 4097); await replacement.Closed();
        await using var isochronous = new MicroPeer(); await isochronous.Connect(device.Port);
        await isochronous.Submit(1, 1, 64, packets: 1); await isochronous.Closed();

        await using var duplicate = new MicroPeer(); await duplicate.Connect(device.Port);
        uint originalRead = await duplicate.Submit(1, 1, 64); await duplicate.DescriptorBarrier();
        await duplicate.Submit(1, 1, 64, sequence: originalRead); await duplicate.Closed();

        await using var bounded = new MicroPeer(); await bounded.Connect(device.Port);
        for (int index = 0; index < 32; index++) await bounded.Submit(1, 1, 64);
        await bounded.DescriptorBarrier();
        await bounded.Submit(1, 1, 64); await bounded.Closed();
    }
}
