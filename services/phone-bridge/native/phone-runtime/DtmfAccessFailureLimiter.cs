#nullable enable
namespace Ivy.PhoneBridge;

internal readonly record struct DtmfAccessRateLimitDecision(
    bool Allowed,
    TimeSpan RetryAfter);

internal sealed class DtmfAccessFailureLimiter
{
    private readonly object _sync = new();
    private readonly Queue<DateTimeOffset> _failures = [];

    public DtmfAccessRateLimitDecision Check(
        DateTimeOffset nowUtc,
        int maximumFailures,
        TimeSpan window)
    {
        Validate(maximumFailures, window);

        lock (_sync)
        {
            RemoveExpired(nowUtc, window);
            if (_failures.Count < maximumFailures)
            {
                return new DtmfAccessRateLimitDecision(true, TimeSpan.Zero);
            }

            var retryAfter = _failures.Peek() + window - nowUtc;
            return new DtmfAccessRateLimitDecision(
                false,
                retryAfter > TimeSpan.Zero ? retryAfter : TimeSpan.Zero);
        }
    }

    public void RecordFailure(
        DateTimeOffset nowUtc,
        int maximumFailures,
        TimeSpan window)
    {
        Validate(maximumFailures, window);

        lock (_sync)
        {
            RemoveExpired(nowUtc, window);
            _failures.Enqueue(nowUtc);
        }
    }

    private void RemoveExpired(DateTimeOffset nowUtc, TimeSpan window)
    {
        var cutoff = nowUtc - window;
        while (_failures.TryPeek(out var failure) && failure <= cutoff)
        {
            _failures.Dequeue();
        }
    }

    private static void Validate(int maximumFailures, TimeSpan window)
    {
        if (maximumFailures < 1)
        {
            throw new ArgumentOutOfRangeException(nameof(maximumFailures));
        }

        if (window <= TimeSpan.Zero)
        {
            throw new ArgumentOutOfRangeException(nameof(window));
        }
    }
}
