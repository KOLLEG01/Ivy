#nullable enable
using System.Diagnostics;
using System.Text.RegularExpressions;

namespace Ivy.PhoneBridge;

internal enum DtmfInputSource
{
    Rfc4733,
    Rfc4733AlternatePayload,
    SipInfo,
    InBandAudio
}

internal enum SipInfoDtmfHandlingResult
{
    Handled,
    NoMatchingCall,
    UnsupportedContentType,
    InvalidBody
}

internal static partial class SipInfoDtmfParser
{
    public static bool IsSupportedContentType(string? contentType)
    {
        var mediaType = contentType?.Split(';', 2)[0].Trim();
        return string.Equals(
                   mediaType,
                   "application/dtmf-relay",
                   StringComparison.OrdinalIgnoreCase) ||
               string.Equals(
                   mediaType,
                   "application/dtmf",
                   StringComparison.OrdinalIgnoreCase);
    }

    public static bool TryParse(
        string? contentType,
        string? body,
        out char digit)
    {
        digit = default;
        if (!IsSupportedContentType(contentType) || string.IsNullOrWhiteSpace(body))
        {
            return false;
        }

        var mediaType = contentType!.Split(';', 2)[0].Trim();
        var candidate = string.Equals(
            mediaType,
            "application/dtmf",
            StringComparison.OrdinalIgnoreCase)
            ? body.Trim()
            : SignalLineRegex().Match(body).Groups[1].Value;

        return candidate.Length == 1 &&
               DtmfAccessCodeGate.TryNormalizeDigit(candidate[0], out digit);
    }

    [GeneratedRegex(
        @"(?im)^\s*Signal\s*[:=]\s*([0-9*#A-D])\s*$",
        RegexOptions.CultureInvariant | RegexOptions.IgnoreCase)]
    private static partial Regex SignalLineRegex();
}

internal static partial class SdpRtpEventPayloadParser
{
    public static IReadOnlyCollection<int> Parse(string? sdp)
    {
        if (string.IsNullOrWhiteSpace(sdp))
        {
            return [];
        }

        return RtpMapRegex()
            .Matches(sdp)
            .Select(match => match.Groups[1].Value)
            .Select(value => int.TryParse(value, out var payloadType) ? payloadType : -1)
            .Where(payloadType => payloadType is >= 0 and <= 127)
            .Distinct()
            .ToArray();
    }

    [GeneratedRegex(
        @"(?im)^\s*a=rtpmap:(\d{1,3})\s+telephone-event(?:/\d+){1,2}\s*$",
        RegexOptions.CultureInvariant | RegexOptions.IgnoreCase)]
    private static partial Regex RtpMapRegex();
}

internal sealed class RtpDtmfEventTracker
{
    private const int MaximumRememberedEvents = 64;

    private readonly object _sync = new();
    private readonly HashSet<RtpDtmfEventKey> _events = [];
    private readonly Queue<RtpDtmfEventKey> _eventOrder = [];

    public bool IsNewEvent(uint ssrc, uint timestamp, byte eventId)
    {
        lock (_sync)
        {
            var key = new RtpDtmfEventKey(ssrc, timestamp, eventId);
            if (!_events.Add(key))
            {
                return false;
            }

            _eventOrder.Enqueue(key);
            while (_eventOrder.Count > MaximumRememberedEvents)
            {
                _events.Remove(_eventOrder.Dequeue());
            }

            return true;
        }
    }

    private readonly record struct RtpDtmfEventKey(
        uint Ssrc,
        uint Timestamp,
        byte EventId);
}

internal sealed class SipInfoRequestTracker
{
    private const int MaximumRememberedRequests = 64;

    private readonly object _sync = new();
    private readonly HashSet<int> _sequences = [];
    private readonly Queue<int> _sequenceOrder = [];

    public bool IsNewRequest(int cSeq)
    {
        lock (_sync)
        {
            if (!_sequences.Add(cSeq))
            {
                return false;
            }

            _sequenceOrder.Enqueue(cSeq);
            while (_sequenceOrder.Count > MaximumRememberedRequests)
            {
                _sequences.Remove(_sequenceOrder.Dequeue());
            }

            return true;
        }
    }
}

internal sealed class DtmfInputDeduplicator
{
    private static readonly TimeSpan CrossSourceWindow = TimeSpan.FromMilliseconds(400);

    private readonly object _sync = new();
    private char _lastDigit;
    private DtmfInputSource _lastSource;
    private long _lastTimestamp;
    private bool _hasLastInput;

    public bool IsNewInput(char digit, DtmfInputSource source)
    {
        var timestamp = Stopwatch.GetTimestamp();
        lock (_sync)
        {
            if (_hasLastInput &&
                _lastDigit == digit &&
                _lastSource != source &&
                Stopwatch.GetElapsedTime(_lastTimestamp, timestamp) <= CrossSourceWindow)
            {
                return false;
            }

            _lastDigit = digit;
            _lastSource = source;
            _lastTimestamp = timestamp;
            _hasLastInput = true;
            return true;
        }
    }
}
