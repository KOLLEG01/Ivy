namespace Ivy.PhoneBridge;

// Local call-progress audio while the original incoming Voice call prepares.
// No Desktop capture or microphone input crosses this boundary until End.
internal sealed class CallWaitingAudio(IPcmAudioPort normal) : IPcmAudioPort {
    private static readonly float[] waitingSamples = LoadWaitingSamples();
    private readonly object sync = new();
    private readonly Queue<bool> feedback = new();
    private int state;
    private int pendingFeedback, feedbackPosition;
    private bool? feedbackCurrent;
    private long generation, sample;
    public bool IsOpen => Volatile.Read(ref state) == 1 || Volatile.Read(ref pendingFeedback) > 0 || normal.IsOpen;
    public long Generation => normal.Generation + Interlocked.Read(ref generation);
    public void Begin() {
        lock (sync) {
            if (state != 0) throw new InvalidOperationException("Original waiting signal already started.");
            Volatile.Write(ref state, 1); Interlocked.Increment(ref generation);
        }
    }
    public void End() {
        lock (sync) {
            if (state != 1) throw new InvalidOperationException("Original waiting signal is unavailable.");
            Volatile.Write(ref state, 2); Interlocked.Increment(ref generation);
        }
    }
    public void Acknowledge(bool success) {
        lock (sync) {
            if (feedback.Count >= 32) throw new InvalidOperationException("Waiting audio feedback capacity exceeded.");
            if (pendingFeedback == 0) Interlocked.Increment(ref generation);
            feedback.Enqueue(success); Volatile.Write(ref pendingFeedback, pendingFeedback + 1);
        }
    }
    public int ReadCaptured(Span<float> output) {
        lock (sync) {
            if (state != 1) {
                if (normal.IsOpen) normal.ReadCaptured(output); else output.Clear();
                for (int i = 0; i < output.Length && pendingFeedback > 0; i++) {
                    if (feedbackCurrent == null) { feedbackCurrent = feedback.Dequeue(); feedbackPosition = 0; }
                    int position = feedbackPosition++;
                    bool success = feedbackCurrent.Value;
                    int duration = AudioSettings.SampleRate * (success ? 160 : 420) / 1000;
                    bool sounding = success ? position < AudioSettings.SampleRate * 120 / 1000 :
                        position < AudioSettings.SampleRate * 120 / 1000 ||
                        position >= AudioSettings.SampleRate * 200 / 1000 && position < AudioSettings.SampleRate * 320 / 1000;
                    if (sounding) {
                        double envelope = Math.Min(1, Math.Min(position % (AudioSettings.SampleRate / 5),
                            AudioSettings.SampleRate * 120 / 1000 - position % (AudioSettings.SampleRate / 5)) /
                            (AudioSettings.SampleRate * .01));
                        output[i] = Math.Clamp(output[i] + (float)(.1 * envelope *
                            Math.Sin(2 * Math.PI * (success ? 880 : 330) * position / AudioSettings.SampleRate)), -1f, 1f);
                    }
                    if (feedbackPosition >= duration) {
                        feedbackCurrent = null; Volatile.Write(ref pendingFeedback, pendingFeedback - 1);
                    }
                }
                return output.Length;
            }
            // The bundled CC0 marimba note decays for six seconds, followed by a quiet pause.
            // It never reads Desktop audio.
            int cycle = waitingSamples.Length + AudioSettings.SampleRate * 2;
            for (int i = 0; i < output.Length; i++) {
                int position = (int)(sample++ % cycle);
                output[i] = position < waitingSamples.Length ? waitingSamples[position] : 0;
            }
            return output.Length;
        }
    }
    public void WriteReceived(ReadOnlySpan<float> input) {
        if (Volatile.Read(ref state) != 1) normal.WriteReceived(input);
    }
    private static float[] LoadWaitingSamples() {
        using var stream = typeof(CallWaitingAudio).Assembly.GetManifestResourceStream("Ivy.PhoneBridge.WaitingMarimba")
            ?? throw new InvalidOperationException("Bundled waiting audio is missing.");
        byte[] bytes = new byte[checked((int)stream.Length)]; stream.ReadExactly(bytes);
        return ScreeningAudio.Parse(bytes);
    }
}
