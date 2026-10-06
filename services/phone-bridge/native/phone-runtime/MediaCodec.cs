using NAudio.Dsp;
using SIPSorcery.Media;
using SIPSorceryMedia.Abstractions;

namespace Ivy.PhoneBridge;

public sealed record CodecSettings(string[] Preferences, int PacketMs = 20, int ReceiveReorderMs = 20) {
    public void Validate() {
        if (PacketMs != 20 || ReceiveReorderMs is < 0 or > 60 || Preferences == null || Preferences.Length is < 1 or > 5 ||
            Preferences.Distinct(StringComparer.Ordinal).Count() != Preferences.Length ||
            Preferences.Any(value => value is not ("G722" or "PCMA" or "PCMU" or "OPUS" or "EVS")))
            throw new ArgumentException("Explicit supported codec preferences and20ms packets required.");
    }
    public List<AudioFormat> Formats() {
        Validate(); using var encoder = new AudioEncoder(includeOpus: Preferences.Contains("OPUS"));
        return Preferences.Select(name => name == "EVS" ? EvsFormat.Offered :
            encoder.SupportedFormats.Single(format => string.Equals(format.FormatName, name, StringComparison.OrdinalIgnoreCase))).ToList();
    }
}

// One negotiated direction owns one encoder/decoder and streaming resampler. Never share codec
// state between calls. Packet duration is20ms; G722 PCM and RTP clocks intentionally differ.
public sealed class MediaCodec : IDisposable {
    public const int PacketSamples = 960;
    public AudioFormat Format { get; }
    public uint RtpDuration => checked((uint)(Format.RtpClockRate / 50));
    private AudioEncoder encoder;
    private EvsNativeCodec evsEncoder, evsDecoder;
    private int PcmRate => EvsFormat.IsEvs(Format) ? EvsNativeCodec.SampleRate : Format.ClockRate;
    private WdlResampler toCodec, fromCodec;
    private long generation = -1;
    private bool wasOpen, disposed;
    private readonly IPcmAudioPort audio;

    public MediaCodec(AudioFormat format, IPcmAudioPort audio) {
        ArgumentNullException.ThrowIfNull(audio); ValidateFormat(format);
        Format = format; this.audio = audio; Reset();
    }
    private static void ValidateFormat(AudioFormat format) {
        string name = format.FormatName?.ToUpperInvariant();
        bool valid = name switch {
            "G722" => format.ClockRate == 16000 && format.RtpClockRate == 8000 && format.ChannelCount == 1,
            "PCMA" or "PCMU" => format.ClockRate == 8000 && format.RtpClockRate == 8000 && format.ChannelCount == 1,
            "OPUS" => format.ClockRate == 48000 && format.RtpClockRate == 48000 && format.ChannelCount is 1 or 2,
            "EVS" => format.ClockRate is 16000 or 32000 && format.RtpClockRate == 16000 && format.ChannelCount == 1,
            _ => false
        };
        if (!valid || format.FormatID is < 0 or > 127) throw new ArgumentException("Unsupported negotiated audio format.");
    }
    private static WdlResampler Resampler(int sourceRate, int targetRate) {
        var value = new WdlResampler(); value.SetMode(false, 0, true, 64, 32);
        value.SetFeedMode(true); value.SetRates(sourceRate, targetRate); return value;
    }
    private void Reset() {
        encoder?.Dispose(); encoder = null; evsEncoder?.Dispose(); evsDecoder?.Dispose(); evsEncoder = null; evsDecoder = null;
        if (!EvsFormat.IsEvs(Format)) encoder = new AudioEncoder(includeOpus: Format.FormatName.Equals("opus", StringComparison.OrdinalIgnoreCase));
        toCodec = Resampler(AudioSettings.SampleRate, PcmRate);
        fromCodec = Resampler(PcmRate, AudioSettings.SampleRate);
    }
    private bool Current() {
        ObjectDisposedException.ThrowIf(disposed, this);
        bool open = audio.IsOpen; long current = audio.Generation;
        if (current != generation || open != wasOpen) { Reset(); generation = current; wasOpen = open; }
        return open;
    }
    private static int Convert(WdlResampler converter, ReadOnlySpan<float> input, Span<float> output) {
        int wanted = converter.ResamplePrepare(input.Length, 1, out Span<float> target);
        if (wanted != input.Length) throw new InvalidOperationException("Resampler input contract changed.");
        input.CopyTo(target);
        int count = converter.ResampleOut(output, input.Length, output.Length, 1);
        output[count..].Clear(); return count;
    }
    public byte[] Encode() {
        bool open = Current(); long expectedGeneration = generation;
        Span<float> captured = stackalloc float[PacketSamples]; captured.Clear();
        if (open) audio.ReadCaptured(captured);
        Span<float> mono = stackalloc float[PcmRate / 50];
        Convert(toCodec, captured, mono);
        // SIPSorcery's PCM API is mono even for the fixed opus/48000/2 SDP declaration.
        var pcm = new short[mono.Length];
        for (int index = 0; index < mono.Length; index++) {
            if (!float.IsFinite(mono[index])) throw new InvalidOperationException("Invalid PCM input.");
            short sample = (short)Math.Round(Math.Clamp(mono[index], -1f, 1f) * short.MaxValue);
            pcm[index] = sample;
        }
        try {
            byte[] result = EncodePcm(pcm);
            if (open && (!audio.IsOpen || audio.Generation != expectedGeneration)) {
                // Discard predictive codec/filter state as well as PCM if authority changed mid-frame.
                Array.Clear(result); Reset(); Array.Clear(pcm); wasOpen = false; result = EncodePcm(pcm);
            }
            return result;
        } finally { Array.Clear(pcm); captured.Clear(); mono.Clear(); }
    }
    private byte[] EncodePcm(short[] pcm) => EvsFormat.IsEvs(Format)
        ? (evsEncoder ??= new EvsNativeCodec(true)).Encode(pcm, EvsFormat.HeaderFull(Format.Parameters)) : encoder.EncodeAudio(pcm, Format);
    public void Decode(byte[] packet, long? requiredGeneration = null) {
        if (!Current()) return;
        if (requiredGeneration.HasValue && generation != requiredGeneration.Value) return;
        if (packet == null || packet.Length is < 1 or > 8192) throw new ArgumentException("Encoded packet size outside bounds.");
        long expectedGeneration = generation;
        var pcm = EvsFormat.IsEvs(Format) ? (evsDecoder ??= new EvsNativeCodec(false)).Decode(packet, EvsFormat.HeaderFull(Format.Parameters)) : encoder.DecodeAudio(packet, Format);
        if (pcm.Length == 0 || pcm.Length > PcmRate * 120 / 1000)
            throw new InvalidOperationException("Invalid decoded audio duration.");
        var mono = new float[pcm.Length];
        for (int index = 0; index < mono.Length; index++) {
            mono[index] = pcm[index] / 32768f;
        }
        Span<float> rendered = stackalloc float[5760];
        int count = Convert(fromCodec, mono, rendered);
        // Sinc interpolation can overshoot a valid full-scale decoded waveform.
        for (int index = 0; index < count; index++) {
            if (!float.IsFinite(rendered[index])) throw new InvalidOperationException("Invalid decoded PCM.");
            rendered[index] = Math.Clamp(rendered[index], -1f, 1f);
        }
        if (audio.IsOpen && audio.Generation == expectedGeneration) audio.WriteReceived(rendered[..count]);
        else { Reset(); wasOpen = false; }
    }
    public void Dispose() { if (disposed) return; disposed = true; encoder?.Dispose(); evsEncoder?.Dispose(); evsDecoder?.Dispose(); }
}
