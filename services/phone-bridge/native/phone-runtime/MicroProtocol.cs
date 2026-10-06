using System.Text;
using System.Text.Json;

namespace Ivy.PhoneBridge;

// Adapted from sepivip/codexdeck d8d8e737, emulator/{framing,protocol,emulator}.js.
// MIT attribution: licenses/CodexMicro-MIT.txt. No physical controls or lighting renderer.
// A protocol reply or microphone frame does not establish Desktop/Voice ownership.
public sealed class MicroProtocol {
    private const int SnakeEffect = 2, DictationRecordingColor = 3050327;
    public const int ReportSize = 64, PayloadSize = 61, MaximumMessageBytes = 16384;
    private readonly List<byte> pending = new();
    public bool SawRgbConfiguration { get; private set; }
    public bool SawStatus { get; private set; }
    public long StatusRequests { get; private set; }
    public long DictationSequence { get; private set; }
    public string LightingState { get; private set; } = "unknown";
    public string LastMethod { get; private set; }

    public static byte[][] Frame(ReadOnlySpan<byte> message) {
        if (message.Length is < 1 or > MaximumMessageBytes) throw new ArgumentException("Bounded Micro message required.");
        var reports = new List<byte[]>();
        for (int offset = 0; offset < message.Length; offset += PayloadSize) {
            int count = Math.Min(PayloadSize, message.Length - offset);
            var report = new byte[ReportSize]; report[0] = 6; report[1] = 2; report[2] = (byte)count;
            message.Slice(offset, count).CopyTo(report.AsSpan(3)); reports.Add(report);
        }
        return reports.ToArray();
    }
    private static byte[][] Encode(object message) => Frame(JsonSerializer.SerializeToUtf8Bytes(message).Concat(new byte[] { 10 }).ToArray());
    public static byte[][] Microphone(bool pressed) => Encode(new { m = "v.oai.hid", p = new { k = "ACT10", act = pressed ? 1 : 0 } });

    // USB/IP carries the report ID. Accept only the declared interface and exact report size.
    // Keep bytes across fragments: decoding each report independently corrupts split UTF-8.
    public byte[][] Receive(ReadOnlySpan<byte> report) {
        if (report.Length != ReportSize || report[0] != 6 || report[1] != 2 || report[2] > PayloadSize)
            throw new ArgumentException("Invalid Micro RPC report.");
        if (pending.Count + report[2] > MaximumMessageBytes) throw new ArgumentException("Micro RPC message limit exceeded.");
        pending.AddRange(report.Slice(3, report[2]).ToArray());
        var bytes = pending.ToArray(); int consumed = 0; var replies = new List<byte[]>();
        try {
            while (consumed < bytes.Length) {
                while (consumed < bytes.Length && bytes[consumed] is 9 or 10 or 13 or 32) consumed++;
                if (consumed == bytes.Length) break;
                var reader = new Utf8JsonReader(bytes.AsSpan(consumed), isFinalBlock: false,
                    new JsonReaderState(new JsonReaderOptions { MaxDepth = 32 }));
                if (!JsonDocument.TryParseValue(ref reader, out var document)) break;
                using (document) replies.AddRange(Reply(document.RootElement));
                consumed += checked((int)reader.BytesConsumed);
            }
        } catch (JsonException error) { throw new ArgumentException("Invalid Micro RPC JSON.", error); }
        if (consumed > 0) pending.RemoveRange(0, consumed);
        return replies.ToArray();
    }
    private byte[][] Reply(JsonElement message) {
        if (message.ValueKind != JsonValueKind.Object) throw new ArgumentException("Micro RPC object required.");
        // The ui uses both compact and long field names; numeric string IDs are echoed numerically.
        if (!message.TryGetProperty("id", out var rawId) && !message.TryGetProperty("i", out rawId)) return [];
        long id;
        if (rawId.ValueKind == JsonValueKind.Number) {
            if (!rawId.TryGetInt64(out id)) throw new ArgumentException("Integral Micro RPC id required.");
        } else if (rawId.ValueKind != JsonValueKind.String || !long.TryParse(rawId.GetString(),
            System.Globalization.NumberStyles.None, System.Globalization.CultureInfo.InvariantCulture, out id))
            throw new ArgumentException("Integral Micro RPC id required.");
        if (id < 0 || id > 9007199254740991L) throw new ArgumentException("Micro RPC id outside exact JSON range.");
        if (!message.TryGetProperty("method", out var method) && !message.TryGetProperty("m", out method) ||
            method.ValueKind != JsonValueKind.String || method.GetString().Length is < 1 or > 128)
            throw new ArgumentException("Micro RPC method required.");
        LastMethod = method.GetString();
        object result = true;
        switch (LastMethod) {
            case "device.status":
                SawStatus = true; StatusRequests++;
                result = new { version = "1.0.0", profile_index = 0, layer_index = 0, battery = 100, is_charging = false };
                break;
            case "sys.version": result = "1.0.0"; break;
            case "v.oai.rgbcfg": SawRgbConfiguration = true; ObserveLighting(message); break;
            // Required host requests are answered even though Ivy has no physical lights.
            case "v.oai.thstatus": case "lights.preview": break;
            // Preserve upstream firmware acknowledgement behavior for future configuration calls.
            // Acknowledgement never maps an unknown host request to a local action.
            default: break;
        }
        return Encode(new { id, result });
    }
    private void ObserveLighting(JsonElement message) {
        if (!message.TryGetProperty("params", out var parameters) && !message.TryGetProperty("p", out parameters) ||
            parameters.ValueKind != JsonValueKind.Object || !parameters.TryGetProperty("ambient", out var ambient) ||
            ambient.ValueKind != JsonValueKind.Object) return;
        bool hasEffect = ambient.TryGetProperty("e", out var effect) || ambient.TryGetProperty("effect", out effect);
        bool hasColor = ambient.TryGetProperty("c", out var color) || ambient.TryGetProperty("color", out color);
        if (!hasEffect || !hasColor || !effect.TryGetInt32(out var effectValue) || !color.TryGetInt32(out var colorValue)) return;
        bool recording = effectValue == SnakeEffect && colorValue == DictationRecordingColor;
        if (recording && LightingState != "dictation_recording") DictationSequence++;
        LightingState = recording ? "dictation_recording" : "other";
    }
}
