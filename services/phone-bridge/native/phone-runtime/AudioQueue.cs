namespace Ivy.PhoneBridge;

// A bounded mono PCM queue shared by the actual WASAPI route and its codec endpoint. Overflow
// drops the oldest complete samples, so device/network stalls cannot turn into growing latency.
public sealed class AudioQueue {
    private readonly object sync = new();
    private readonly float[] samples;
    private int head, count;
    private long dropped, underruns;
    private readonly int prebuffer;
    private readonly bool rebufferAfterUnderrun;
    private readonly bool smoothDiscontinuities;
    private bool smoothPending;
    private float lastSample;
    private bool started;
    public AudioQueue(int capacity, int prebuffer = 0, bool rebufferAfterUnderrun = false, bool smoothDiscontinuities = false) {
        if (capacity < 1 || capacity > AudioSettings.SampleRate / 5) throw new ArgumentOutOfRangeException(nameof(capacity));
        if (prebuffer < 0 || prebuffer > capacity) throw new ArgumentOutOfRangeException(nameof(prebuffer));
        this.prebuffer = prebuffer;
        this.rebufferAfterUnderrun = rebufferAfterUnderrun;
        this.smoothDiscontinuities = smoothDiscontinuities;
        samples = new float[capacity];
    }
    public int Count { get { lock (sync) return count; } }
    public long Dropped { get { lock (sync) return dropped; } }
    public long Underruns { get { lock (sync) return underruns; } }
    public void Clear() { lock (sync) { Array.Clear(samples); count = 0; head = 0; started = false; lastSample = 0; smoothPending = false; } }
    public void Write(ReadOnlySpan<float> input) {
        lock (sync) {
            if (input.Length >= samples.Length) {
                dropped += (long)count + input.Length - samples.Length;
                if (count > 0 || input.Length > samples.Length) smoothPending = true;
                input[^samples.Length..].CopyTo(samples); head = 0; count = samples.Length;
                return;
            }
            int discard = Math.Max(0, count + input.Length - samples.Length);
            if (discard > 0) smoothPending = true;
            head = (head + discard) % samples.Length; count -= discard; dropped += discard;
            int tail = (head + count) % samples.Length, first = Math.Min(input.Length, samples.Length - tail);
            input[..first].CopyTo(samples.AsSpan(tail)); input[first..].CopyTo(samples); count += input.Length;
        }
    }
    public int Read(Span<float> output) {
        lock (sync) {
            if (output.IsEmpty) return 0;
            if (!started) {
                if (count < prebuffer) { output.Clear(); return 0; }
                started = true; smoothPending = true;
            }
            int length = Math.Min(count, output.Length), first = Math.Min(length, samples.Length - head);
            samples.AsSpan(head, first).CopyTo(output); samples.AsSpan(0, length - first).CopyTo(output[first..]);
            // Clear consumed audio; a later gate reopening cannot replay it.
            samples.AsSpan(head, first).Clear(); samples.AsSpan(0, length - first).Clear();
            head = (head + length) % samples.Length; count -= length;
            if (length < output.Length) { underruns++; if (rebufferAfterUnderrun) started = false; }
            output[length..].Clear();
            if (smoothDiscontinuities) Smooth(output, length);
            return length;
        }
    }
    private void Smooth(Span<float> output, int length) {
        // Only blend an actual queue cut or a new utterance. Continuous PCM is
        // untouched; the 5 ms blend occupies existing samples and adds no delay.
        const int blend = AudioSettings.SampleRate / 200;
        if (smoothPending && length > 0) {
            int limit = Math.Min(blend, length);
            for (int index = 0; index < limit; index++) {
                float weight = (index + 1f) / limit;
                output[index] = lastSample * (1 - weight) + output[index] * weight;
            }
            smoothPending = false;
        }
        if (length < output.Length) {
            if (length > 0) {
                int limit = Math.Min(blend, length);
                for (int index = 0; index < limit; index++)
                    output[length - limit + index] *= 1 - (index + 1f) / limit;
            } else {
                int limit = Math.Min(blend, output.Length);
                for (int index = 0; index < limit; index++)
                    output[index] = lastSample * (1 - (index + 1f) / limit);
            }
        }
        lastSample = output[^1];
    }
}
