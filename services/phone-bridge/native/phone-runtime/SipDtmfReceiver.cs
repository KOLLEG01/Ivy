using SIPSorceryMedia.Abstractions;
namespace Ivy.PhoneBridge;

// A private analysis sink, never a Windows device: unauthenticated media can reach only the
// tone detector. No PCM or digit observations are retained in diagnostics or public receipts.
internal sealed class SipDtmfReceiver : IPcmAudioPort, IDisposable {
    private readonly InBandDtmfDetector detector = new();
    private MediaCodec decoder;
    public bool IsOpen { get; private set; } = true;
    public long Generation => 0;
    internal object Diagnostics => detector.Diagnostics;
    internal SipDtmfReceiver(Action<char, DtmfInputSource> digit) {
        detector.ToneDetected += value => digit(value.Digit, DtmfInputSource.InBandAudio);
    }
    internal void Receive(AudioFormat format, byte[] payload) {
        if (!IsOpen) return;
        if (decoder == null || !decoder.Format.Equals(format)) { decoder?.Dispose(); decoder = new MediaCodec(format, this); }
        decoder.Decode(payload);
    }
    public int ReadCaptured(Span<float> output) { output.Clear(); return 0; }
    public void WriteReceived(ReadOnlySpan<float> input) {
        var pcm = new short[input.Length];
        try { for (int i = 0; i < input.Length; i++) pcm[i] = (short)Math.Clamp(input[i] * 32767, short.MinValue, short.MaxValue); detector.Process(pcm, 48000); }
        finally { Array.Clear(pcm); }
    }
    public void Dispose() { IsOpen = false; decoder?.Dispose(); }
}
