#nullable enable
using System.Security.Cryptography;

namespace Ivy.PhoneBridge;

internal enum DtmfAccessCodeProgress
{
    Unsupported,
    Ignored,
    Progress,
    Accepted,
    Rejected,
    AlreadyCompleted
}

internal enum DtmfAccessCodeResult
{
    Accepted,
    AcceptedWithoutCodecRenegotiation,
    Rejected
}

internal sealed class DtmfAccessCodeGate
{
    private readonly object _sync = new();
    private readonly byte[] _standardAccessCode;
    private readonly byte[] _noCodecRenegotiationAccessCode;
    private readonly byte[] _attempt;
    private readonly TaskCompletionSource<DtmfAccessCodeResult> _completed = new(
        TaskCreationOptions.RunContinuationsAsynchronously);
    private int _attemptLength;
    private bool _attemptStarted;
    private bool _isCompleted;
    private bool _isAccepted;
    private bool _disableCodecRenegotiation;

    public DtmfAccessCodeGate(string accessCode)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(accessCode);
        var normalizedAccessCode = accessCode.ToUpperInvariant();
        if (normalizedAccessCode.Length < 3 ||
            normalizedAccessCode[0] != '*' ||
            normalizedAccessCode[^1] != '#' ||
            normalizedAccessCode.AsSpan(1, normalizedAccessCode.Length - 2)
                .IndexOfAnyExcept("0123456789ABCD") >= 0)
        {
            throw new ArgumentException(
                "The DTMF access code must be framed by '*' and '#' and contain only " +
                "digits or DTMF letters inside the frame.",
                nameof(accessCode));
        }

        var accessCodeBytes = normalizedAccessCode
            .Select(character => (byte)character)
            .ToArray();

        // Compare both accepted forms using equally sized buffers. The regular code is
        // zero-padded by one byte; the opt-out form prepends one additional '*'. This
        // keeps the comparison independent of the first incorrect digit while allowing
        // callers to disable codec renegotiation for this call only.
        _standardAccessCode = new byte[accessCodeBytes.Length + 1];
        accessCodeBytes.CopyTo(_standardAccessCode, 0);
        _noCodecRenegotiationAccessCode = new byte[accessCodeBytes.Length + 1];
        _noCodecRenegotiationAccessCode[0] = (byte)'*';
        accessCodeBytes.CopyTo(_noCodecRenegotiationAccessCode, 1);
        _attempt = new byte[accessCodeBytes.Length + 1];
    }

    public bool IsAccepted
    {
        get
        {
            lock (_sync)
            {
                return _isAccepted;
            }
        }
    }

    public bool DisableCodecRenegotiation
    {
        get
        {
            lock (_sync)
            {
                return _isAccepted && _disableCodecRenegotiation;
            }
        }
    }

    public DtmfAccessCodeProgress ProcessRtpEvent(byte eventId)
    {
        if (!TryConvertRtpEvent(eventId, out var digit))
        {
            return DtmfAccessCodeProgress.Unsupported;
        }

        return ProcessDigit(digit);
    }

    public DtmfAccessCodeProgress ProcessDigit(char digit)
    {
        if (!TryNormalizeDigit(digit, out digit))
        {
            return DtmfAccessCodeProgress.Unsupported;
        }

        DtmfAccessCodeResult? completedNow = null;
        DtmfAccessCodeProgress result;
        lock (_sync)
        {
            if (_isCompleted)
            {
                return DtmfAccessCodeProgress.AlreadyCompleted;
            }

            if (!_attemptStarted)
            {
                if (digit != '*')
                {
                    return DtmfAccessCodeProgress.Ignored;
                }

                _attemptStarted = true;
                AppendAttemptDigit(digit);
                result = DtmfAccessCodeProgress.Progress;
            }
            else
            {
                AppendAttemptDigit(digit);
                if (digit != '#')
                {
                    result = DtmfAccessCodeProgress.Progress;
                }
                else
                {
                    _isCompleted = true;
                    var standardLength =
                        _attemptLength == _standardAccessCode.Length - 1;
                    var noCodecRenegotiationLength =
                        _attemptLength == _noCodecRenegotiationAccessCode.Length;
                    var standardMatch = CryptographicOperations.FixedTimeEquals(
                        _attempt,
                        _standardAccessCode);
                    var noCodecRenegotiationMatch =
                        CryptographicOperations.FixedTimeEquals(
                            _attempt,
                            _noCodecRenegotiationAccessCode);
                    _disableCodecRenegotiation =
                        noCodecRenegotiationLength & noCodecRenegotiationMatch;
                    _isAccepted =
                        (standardLength & standardMatch) |
                        _disableCodecRenegotiation;
                    completedNow = _isAccepted
                        ? _disableCodecRenegotiation
                            ? DtmfAccessCodeResult.AcceptedWithoutCodecRenegotiation
                            : DtmfAccessCodeResult.Accepted
                        : DtmfAccessCodeResult.Rejected;
                    result = _isAccepted
                        ? DtmfAccessCodeProgress.Accepted
                        : DtmfAccessCodeProgress.Rejected;
                }
            }
        }

        if (completedNow is not null)
        {
            _completed.TrySetResult(completedNow.Value);
        }

        return result;
    }

    private void AppendAttemptDigit(char digit)
    {
        if (_attemptLength < _attempt.Length)
        {
            _attempt[_attemptLength] = (byte)digit;
        }

        _attemptLength = Math.Min(_attemptLength + 1, _attempt.Length + 1);
    }

    public Task<DtmfAccessCodeResult> WaitForCompletionAsync(
        TimeSpan timeout,
        CancellationToken cancellationToken)
    {
        if (timeout <= TimeSpan.Zero)
        {
            throw new ArgumentOutOfRangeException(nameof(timeout));
        }

        return _completed.Task.WaitAsync(timeout, cancellationToken);
    }

    internal static bool TryConvertRtpEvent(byte eventId, out char digit)
    {
        digit = eventId switch
        {
            <= 9 => (char)('0' + eventId),
            10 => '*',
            11 => '#',
            12 => 'A',
            13 => 'B',
            14 => 'C',
            15 => 'D',
            _ => default
        };

        return eventId <= 15;
    }

    internal static bool TryNormalizeDigit(char value, out char digit)
    {
        digit = char.ToUpperInvariant(value);
        return digit is (>= '0' and <= '9') or '*' or '#' or
            (>= 'A' and <= 'D');
    }

}
