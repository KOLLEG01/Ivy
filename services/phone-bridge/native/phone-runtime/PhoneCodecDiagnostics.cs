namespace Ivy.PhoneBridge;

public sealed record PhoneCodecCheck(string Name, bool Passed, int Frames, long EncodedBytes,
    long DecodedSamples, double Rms, string Error);
public sealed record PhoneCodecTestResult(bool Passed, PhoneCodecCheck[] Codecs);

// Bounded synthetic roundtrips use the shipped codecs and the production 48kHz resamplers.
// The port never touches Windows devices, Desktop, credentials or network transports.
public static class PhoneCodecDiagnostics {
    public static PhoneCodecTestResult Run() {
        var results = new List<PhoneCodecCheck>();
        foreach (string name in new[] { "G722", "PCMA", "PCMU", "OPUS", "EVS" }) {
            var source = new SyntheticPort(); var sink = new SyntheticPort();
            long bytes = 0; int frames = 0; string error = null;
            try {
                var format = new CodecSettings([name]).Formats().Single();
                using var encoder = new MediaCodec(format, source);
                using var decoder = new MediaCodec(format, sink);
                for (int frame = 0; frame < 25; frame++) {
                    byte[] packet = encoder.Encode();
                    try {
                        if (packet.Length is < 1 or > 8192) throw new InvalidOperationException();
                        bytes += packet.Length; decoder.Decode(packet); frames++;
                    } finally { Array.Clear(packet); }
                }
                if (sink.Samples is < 23000 or > 24000 || sink.Rms is < .1 or > .55 || !double.IsFinite(sink.Rms)) error = "roundtrip_invalid";
            } catch (DllNotFoundException) { error = "codec_library_missing"; }
              catch (BadImageFormatException) { error = "codec_library_incompatible"; }
              catch (EntryPointNotFoundException) { error = "codec_library_incompatible"; }
              catch (Exception) { error = "codec_roundtrip_failed"; }
            results.Add(new(name, error == null, frames, bytes, sink.Samples, sink.Rms, error));
        }
        return new(results.All(result => result.Passed), results.ToArray());
    }
    private sealed class SyntheticPort : IPcmAudioPort {
        private long captured;
        private double energy;
        public long Samples { get; private set; }
        public double Rms => Samples == 0 ? 0 : Math.Sqrt(energy / Samples);
        public bool IsOpen => true;
        public long Generation => 1;
        public int ReadCaptured(Span<float> output) {
            for (int i = 0; i < output.Length; i++) output[i] = (float)(.4 * Math.Sin(2 * Math.PI * 440 * captured++ / 48000));
            return output.Length;
        }
        public void WriteReceived(ReadOnlySpan<float> input) {
            foreach (float sample in input) {
                if (!float.IsFinite(sample) || Math.Abs(sample) > 1) throw new InvalidOperationException();
                energy += sample * sample;
            }
            Samples += input.Length;
        }
    }
}
