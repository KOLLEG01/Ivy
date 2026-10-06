namespace Ivy.PhoneBridge;

// Audio remains granted for the owned call until suspension or revocation. Diagnostic
// observation permits can still expire; neither kind survives native restart.
public sealed class AudioPermit {
    private readonly object sync = new();
    private readonly TimeProvider time;
    private long observedAt;
    private TimeSpan validity;
    private bool granted, revoked, observed;
    private bool sessionOwned;
    private long generation;
    public AudioPermit(TimeProvider time = null) { this.time = time ?? TimeProvider.System; }
    public long Generation { get { lock (sync) return generation; } }
    public void Grant() {
        lock (sync) {
            if (revoked) throw new InvalidOperationException("Revoked call cannot regain audio.");
            if (!granted) generation++;
            sessionOwned = true; granted = true;
        }
    }
    public void Renew(long observationTimestamp, TimeSpan maximumAge) {
        if (maximumAge <= TimeSpan.Zero || maximumAge > TimeSpan.FromMilliseconds(500))
            throw new ArgumentOutOfRangeException(nameof(maximumAge));
        lock (sync) {
            if (revoked) throw new InvalidOperationException("This call's audio permit has been revoked.");
            long now = time.GetTimestamp(); var elapsed = time.GetElapsedTime(observationTimestamp, now);
            if (elapsed < TimeSpan.Zero || elapsed >= maximumAge || (observed && observationTimestamp < observedAt))
                throw new InvalidOperationException("A fresh, ordered audio observation is required.");
            var previousAge = time.GetElapsedTime(observedAt, now);
            if (!granted || previousAge < TimeSpan.Zero || previousAge >= validity) generation++;
            observedAt = observationTimestamp; validity = maximumAge; granted = true; observed = true; sessionOwned = false;
        }
    }
    public bool IsCurrent() {
        lock (sync) {
            if (!granted || revoked) return false;
            if (sessionOwned) return true;
            var elapsed = time.GetElapsedTime(observedAt, time.GetTimestamp());
            if (elapsed >= TimeSpan.Zero && elapsed < validity) return true;
            granted = false; return false;
        }
    }
    public void Revoke() { lock (sync) { revoked = true; granted = false; } }
    public void Suspend() { lock (sync) granted = false; }
}
