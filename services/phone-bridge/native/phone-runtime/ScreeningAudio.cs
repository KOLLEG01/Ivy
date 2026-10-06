using System.Buffers.Binary;
using System.Security.Cryptography;
namespace Ivy.PhoneBridge;

public sealed record ScreeningSettings(string WavPath, string Sha256, int TimeoutSeconds = 30, int RepeatCount = 1);

internal sealed class ScreeningAudio : IPcmAudioPort, IDisposable {
    private readonly IPcmAudioPort normal;
    private readonly Func<bool> waiting;
    private float[] samples;
    private int offset, remaining, pauseSamples, pauseRemaining;
    private long generation;
    internal ScreeningAudio(IPcmAudioPort normal, Func<bool> waiting) { this.normal = normal; this.waiting = waiting; }
    public bool IsOpen => samples != null && waiting() || normal.IsOpen;
    public long Generation => generation + normal.Generation;
    internal void Load(ScreeningSettings settings) {
        if (samples != null || settings == null || !Path.IsPathFullyQualified(settings.WavPath) ||
            settings.TimeoutSeconds is < 1 or > 120 || settings.RepeatCount is < 0 or > 3)
            throw new ArgumentException("Bounded original screening announcement required.");
        using var file = new FileStream(settings.WavPath, FileMode.Open, FileAccess.Read, FileShare.Read);
        if (file.Length is < 44 or > 10485760) throw new ArgumentException("Bounded WAV required.");
        byte[] bytes = new byte[file.Length]; file.ReadExactly(bytes);
        try {
            if ("sha256:" + Convert.ToHexStringLower(SHA256.HashData(bytes)) != settings.Sha256) throw new ArgumentException("Announcement hash changed.");
            samples = Parse(bytes); remaining = settings.RepeatCount + 1;
            pauseSamples = Math.Max(0, (settings.TimeoutSeconds * 48000 - samples.Length * remaining) / remaining); generation++;
        } finally { Array.Clear(bytes); }
    }
    internal static float[] Parse(ReadOnlySpan<byte> bytes) {
        if (bytes.Length < 44 || !bytes[..4].SequenceEqual("RIFF"u8) || !bytes.Slice(8, 4).SequenceEqual("WAVE"u8)) throw new ArgumentException("PCM WAV required.");
        int rate = 0, channels = 0; ReadOnlySpan<byte> data = default;
        for (int offset = 12; offset + 8 <= bytes.Length;) {
            var id = bytes.Slice(offset, 4); uint declared = BinaryPrimitives.ReadUInt32LittleEndian(bytes.Slice(offset + 4, 4));
            int start = offset + 8;
            if (declared == uint.MaxValue && id.SequenceEqual("data"u8)) declared = (uint)(bytes.Length - start);
            if (declared > bytes.Length - start) throw new ArgumentException("Truncated WAV chunk.");
            int length = (int)declared;
            if (id.SequenceEqual("fmt "u8)) {
                if (length < 16 || BinaryPrimitives.ReadUInt16LittleEndian(bytes.Slice(start, 2)) != 1 ||
                    BinaryPrimitives.ReadUInt16LittleEndian(bytes.Slice(start + 14, 2)) != 16) throw new ArgumentException("PCM16 WAV required.");
                channels = BinaryPrimitives.ReadUInt16LittleEndian(bytes.Slice(start + 2, 2));
                rate = checked((int)BinaryPrimitives.ReadUInt32LittleEndian(bytes.Slice(start + 4, 4)));
            } else if (id.SequenceEqual("data"u8)) data = bytes.Slice(start, length);
            offset = checked(start + length + (length & 1));
        }
        if (channels is < 1 or > 8 || rate is < 8000 or > 192000 || data.IsEmpty || data.Length % (2 * channels) != 0) throw new ArgumentException("Invalid PCM layout.");
        int frames = data.Length / (2 * channels);
        if ((long)frames * 48000 / rate < 1) throw new ArgumentException("Announcement must contain a complete output sample.");
        if ((long)frames * 48000 / rate > 48000 * 120) throw new ArgumentException("Announcement exceeds two minutes.");
        var result = new float[(int)((long)frames * 48000 / rate)];
        for (int i = 0; i < result.Length; i++) {
            double source = (double)i * rate / 48000; int first = Math.Min((int)source, frames - 1), second = Math.Min(first + 1, frames - 1);
            double sum = 0;
            for (int c = 0; c < channels; c++) {
                short a = BinaryPrimitives.ReadInt16LittleEndian(data.Slice((first * channels + c) * 2, 2));
                short b = BinaryPrimitives.ReadInt16LittleEndian(data.Slice((second * channels + c) * 2, 2));
                sum += a + (b - a) * (source - first);
            }
            result[i] = (float)(sum / channels / 32768);
        }
        return result;
    }
    public int ReadCaptured(Span<float> output) {
        if (samples == null) return normal.ReadCaptured(output);
        output.Clear();
        if (!waiting() || remaining == 0) return output.Length;
        for (int i = 0; i < output.Length && remaining > 0; i++) {
            if (pauseRemaining > 0) { pauseRemaining--; continue; }
            output[i] = samples[offset++];
            if (offset == samples.Length) { offset = 0; remaining--; pauseRemaining = pauseSamples; }
        }
        return output.Length;
    }
    public void WriteReceived(ReadOnlySpan<float> input) => normal.WriteReceived(input);
    internal void Bridge() { if (samples != null) Array.Clear(samples); samples = null; generation++; }
    public void Dispose() => Bridge();
}
