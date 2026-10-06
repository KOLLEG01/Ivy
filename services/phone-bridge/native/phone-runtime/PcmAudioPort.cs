namespace Ivy.PhoneBridge;

public interface IPcmAudioPort {
    bool IsOpen { get; }
    long Generation { get; }
    int ReadCaptured(Span<float> output);
    void WriteReceived(ReadOnlySpan<float> input);
}
