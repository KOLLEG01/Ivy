using NAudio.Dsp;
using Concentus;
using Concentus.Structs;
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
    private IOpusDecoder opusDecoder;
    private EvsNativeCodec evsEncoder, evsDecoder;
    private bool IsOpus => Format.FormatName.Equals("opus", StringComparison.OrdinalIgnoreCase);
    private uint? nextReceiveTimestamp;
    public long ConcealedSamples { get; private set; }
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
        encoder?.Dispose(); encoder = null; opusDecoder?.Dispose(); opusDecoder = null;
        evsEncoder?.Dispose(); evsDecoder?.Dispose(); evsEncoder = null; evsDecoder = null;
        nextReceiveTimestamp = null;
        // Opus already uses the call's 48 kHz PCM clock. Identity resampling adds
        // filter delay that consumes the jitter reserve without changing the rate.
        toCodec = PcmRate == AudioSettings.SampleRate ? null : Resampler(AudioSettings.SampleRate, PcmRate);
        fromCodec = PcmRate == AudioSettings.SampleRate ? null : Resampler(PcmRate, AudioSettings.SampleRate);
    }
    private bool Current() {
        ObjectDisposedException.ThrowIf(disposed, this);
        bool open = audio.IsOpen; long current = audio.Generation;
        if (current != generation || open != wasOpen) { Reset(); generation = current; wasOpen = open; }
        return open;
    }
    private static int Convert(WdlResampler converter, ReadOnlySpan<float> input, Span<float> output) {
        if (converter == null) { input.CopyTo(output); output[input.Length..].Clear(); return input.Length; }
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
        ? (evsEncoder ??= new EvsNativeCodec(true)).Encode(pcm, EvsFormat.HeaderFull(Format.Parameters))
        : (encoder ??= new AudioEncoder(includeOpus: IsOpus)).EncodeAudio(pcm, Format);
    public void DecodeRtp(RtpAudioPacket packet) {
        ArgumentNullException.ThrowIfNull(packet);
        if (!Current() || generation != packet.Generation) return;
        if (!IsOpus) { Decode(packet.Payload, packet.Generation); return; }
        if (packet.Payload == null || packet.Payload.Length is < 1 or > 8192) throw new ArgumentException("Encoded packet size outside bounds.");
        int duration = OpusPacketInfo.GetNumSamples(packet.Payload, PcmRate);
        if (duration is < 1 or > 5760) throw new InvalidOperationException("Invalid Opus packet duration.");
        if (nextReceiveTimestamp.HasValue) {
            int missing = unchecked((int)(packet.Timestamp - nextReceiveTimestamp.Value));
            if (missing > 0 && missing <= 5760 && missing % 480 == 0) {
                // Preserve Opus overlap/filter history and the RTP timeline. Recover the
                // last lost packet with in-band FEC when available; earlier losses use PLC.
                int fec = duration % 480 == 0 && missing >= duration ? duration : 0;
                int remaining = missing - fec;
                while (remaining > 0) {
                    int length = Math.Min(960, remaining);
                    RenderOpus(ReadOnlySpan<byte>.Empty, length, false, packet.Generation);
                    ConcealedSamples += length; remaining -= length;
                }
                if (fec > 0) { RenderOpus(packet.Payload, fec, true, packet.Generation); ConcealedSamples += fec; }
            } else if (missing != 0) Reset(); // New timestamp domain or a long silence, not recoverable backlog.
        }
        Decode(packet.Payload, packet.Generation);
        if (audio.IsOpen && audio.Generation == generation) nextReceiveTimestamp = unchecked(packet.Timestamp + (uint)duration);
    }
    private void RenderOpus(ReadOnlySpan<byte> packet, int length, bool fec, long expectedGeneration) {
        if (!audio.IsOpen || audio.Generation != expectedGeneration) return;
        opusDecoder ??= OpusCodecFactory.CreateDecoder(PcmRate, 1);
        Span<short> pcm = stackalloc short[5760];
        int count = opusDecoder.Decode(packet, pcm, length, fec);
        if (count is < 1 or > 5760) throw new InvalidOperationException("Invalid decoded Opus duration.");
        Render(pcm[..count], expectedGeneration);
    }
    public void Decode(byte[] packet, long? requiredGeneration = null) {
        if (!Current()) return;
        if (requiredGeneration.HasValue && generation != requiredGeneration.Value) return;
        if (packet == null || packet.Length is < 1 or > 8192) throw new ArgumentException("Encoded packet size outside bounds.");
        long expectedGeneration = generation;
        if (IsOpus) { RenderOpus(packet, 5760, false, expectedGeneration); return; }
        var pcm = EvsFormat.IsEvs(Format) ? (evsDecoder ??= new EvsNativeCodec(false)).Decode(packet, EvsFormat.HeaderFull(Format.Parameters))
            : (encoder ??= new AudioEncoder()).DecodeAudio(packet, Format);
        Render(pcm, expectedGeneration);
    }
    private void Render(ReadOnlySpan<short> pcm, long expectedGeneration) {
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
    public void Dispose() { if (disposed) return; disposed = true; encoder?.Dispose(); opusDecoder?.Dispose(); evsEncoder?.Dispose(); evsDecoder?.Dispose(); }
}
