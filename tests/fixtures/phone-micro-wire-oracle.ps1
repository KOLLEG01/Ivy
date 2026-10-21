$ErrorActionPreference = 'Stop'
$imports = @'
using System;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Reflection;
using System.Threading;
using System.Threading.Tasks;
'@
$definition = $imports + "`n"
foreach ($name in @('MicroProtocol.cs', 'MicroUsbDescriptors.cs', 'MicroUsbSession.cs')) {
    $source = [IO.File]::ReadAllText((Join-Path $PSScriptRoot "../../services/phone-bridge/native/phone-runtime/$name"))
    $marker = 'namespace Ivy.PhoneBridge;'
    $at = $source.IndexOf($marker, [StringComparison]::Ordinal)
    if ($at -lt 0) { throw 'Original namespace marker unavailable' }
    $definition += "namespace Ivy.PhoneBridge {`n" + $source.Substring($at + $marker.Length) + "`n}`n"
}
$client = [IO.File]::ReadAllText((Join-Path $PSScriptRoot '../../services/phone-bridge/native/phone-runtime/MicroUsbClient.cs'))
$at = $client.IndexOf('public static class MicroPortInventory', [StringComparison]::Ordinal)
if ($at -lt 0) { throw 'Original inventory class unavailable' }
$definition += "namespace Ivy.PhoneBridge {`n" + $client.Substring($at) + "`n}`n"
$definition += @'
namespace Ivy.PhoneBridge {
public static class MicroWireOracle {
    private delegate void ControlProbe(uint sequence, uint direction, uint capacity, ReadOnlySpan<byte> setup, byte[] payload);
    public static string Run(string input) {
        using var document = JsonDocument.Parse(input);
        var frames = new List<object>();
        foreach (var item in document.RootElement.GetProperty("frames").EnumerateArray()) {
            try {
                frames.Add(new { ok = true, reports = MicroProtocol.Frame(Convert.FromHexString(item.GetString()!)).Select(Convert.ToHexString).ToArray() });
            } catch (ArgumentException e) { frames.Add(new { ok = false, error = e.Message }); }
        }
        var inventories = new List<object>();
        foreach (var item in document.RootElement.GetProperty("inventories").EnumerateArray()) {
            try {
                inventories.Add(new { ok = true, port = MicroPortInventory.OwnPort(item.GetProperty("output").GetString(), item.GetProperty("tcpPort").GetInt32()) });
            } catch (ArgumentException e) { inventories.Add(new { ok = false, error = e.Message }); }
        }
        var controls = new List<object>();
        foreach (var item in document.RootElement.GetProperty("controls").EnumerateArray()) {
            using var stream = new MemoryStream();
            var session = new MicroUsbSession(stream, 1, _ => true);
            try {
                var method = typeof(MicroUsbSession).GetMethod("Control", BindingFlags.Instance | BindingFlags.NonPublic)!;
                var control = method.CreateDelegate<ControlProbe>(session);
                control(17, item.GetProperty("direction").GetUInt32(), item.GetProperty("capacity").GetUInt32(),
                    Convert.FromHexString(item.GetProperty("setup").GetString()!), Convert.FromHexString(item.GetProperty("payload").GetString()!));
                var bytes = stream.ToArray();
                if (bytes.Length < 48 || BinaryPrimitives.ReadUInt32BigEndian(bytes) != 3 || BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(4)) != 17 ||
                    BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(24)) != bytes.Length - 48) throw new Exception("Original return wire inconsistent");
                controls.Add(new { ok = true, status = BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(20)), payload = Convert.ToHexString(bytes.AsSpan(48)) });
            } catch (IOException e) { controls.Add(new { ok = false, error = e.Message }); }
              catch (ArgumentException e) { controls.Add(new { ok = false, error = e.Message }); }
            finally { session.DisposeAsync().AsTask().GetAwaiter().GetResult(); }
        }
        var strings = Enumerable.Range(-1, 258).Select(i => MicroUsbDescriptors.String(i)).Select(b => b == null ? null : Convert.ToHexString(b)).ToArray();
        return JsonSerializer.Serialize(new {
            descriptors = new { report = Convert.ToHexString(MicroUsbDescriptors.Report()), device = Convert.ToHexString(MicroUsbDescriptors.Device()),
                configuration = Convert.ToHexString(MicroUsbDescriptors.Configuration()), export = Convert.ToHexString(MicroUsbDescriptors.Export()), strings,
                busId = MicroUsbDescriptors.BusId, serial = MicroUsbDescriptors.Serial, deviceId = MicroUsbDescriptors.DeviceId },
            frames, inventories, controls,
            microphone = new[] { MicroProtocol.Microphone(false).Select(Convert.ToHexString).ToArray(), MicroProtocol.Microphone(true).Select(Convert.ToHexString).ToArray() }
        });
    }
}
}
'@
Add-Type -TypeDefinition $definition
[Console]::Out.Write([Ivy.PhoneBridge.MicroWireOracle]::Run([Console]::In.ReadToEnd()))
