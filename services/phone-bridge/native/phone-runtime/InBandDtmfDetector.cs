#nullable enable


namespace Ivy.PhoneBridge;

internal sealed record InBandDtmfDetection(
    char Digit,
    int SampleRate,
    int ConfirmedDurationMilliseconds,
    double Rms,
    double ToneEnergyRatio,
    double LowGroupAmplitude,
    double HighGroupAmplitude);

internal sealed class InBandDtmfDetector
{
    private const int WindowMilliseconds = 40;
    private const int HopMilliseconds = 10;
    private const int RequiredCandidateWindows = 2;
    private const int RequiredReleaseWindows = 2;
    private const double MinimumRms = 120d;
    private const double MinimumAbsoluteToneAmplitude = 150d;
    private const double MinimumRelativeToneAmplitude = 0.30d;
    private const double MinimumDominanceRatio = 2.0d;
    private const double MinimumToneEnergyRatio = 0.68d;
    private const double MinimumTwistRatio = 0.28d;
    private const double MaximumTwistRatio = 3.55d;

    private static readonly double[] LowGroupFrequencies = [697d, 770d, 852d, 941d];
    private static readonly double[] HighGroupFrequencies = [1209d, 1336d, 1477d, 1633d];
    private static readonly double[] FrequencyScales = [0.985d, 1d, 1.015d];
    private static readonly char[,] Digits =
    {
        { '1', '2', '3', 'A' },
        { '4', '5', '6', 'B' },
        { '7', '8', '9', 'C' },
        { '*', '0', '#', 'D' }
    };

    private readonly object _sync = new();
    private readonly List<short> _samples = [];
    private int _sampleRate;
    private int _windowSamples;
    private int _hopSamples;
    private double[] _windowWeights = [];
    private double _windowWeightSum;
    private char? _candidate;
    private int _candidateWindows;
    private char? _emittedDigit;
    private int _releaseWindows;
    private long _analyzedWindows;
    private long _audibleWindows;
    private long _pairedToneWindows;
    private long _candidateToneWindows;
    private double _maximumPairedToneEnergyRatio;

    // Aggregate signal-quality counters only. Audio samples and individual tones are never retained.
    internal object Diagnostics
    {
        get
        {
            lock (_sync)
            {
                return new
                {
                    analyzedWindows = _analyzedWindows,
                    audibleWindows = _audibleWindows,
                    pairedToneWindows = _pairedToneWindows,
                    candidateToneWindows = _candidateToneWindows,
                    maximumPairedToneEnergyRatio = _maximumPairedToneEnergyRatio
                };
            }
        }
    }

    public event Action<InBandDtmfDetection>? ToneDetected;

    public void Process(short[] samples, int sampleRate)
    {
        ArgumentNullException.ThrowIfNull(samples);
        if (sampleRate < 4_000 || samples.Length == 0)
        {
            return;
        }

        List<InBandDtmfDetection>? detections = null;
        lock (_sync)
        {
            if (_sampleRate != sampleRate)
            {
                ResetForSampleRate(sampleRate);
            }

            _samples.AddRange(samples);
            while (_samples.Count >= _windowSamples)
            {
                var analysis = AnalyzeWindow(_samples, _windowSamples, _sampleRate);
                var detection = AdvanceState(analysis);
                if (detection is not null)
                {
                    detections ??= [];
                    detections.Add(detection);
                }

                _samples.RemoveRange(0, _hopSamples);
            }
        }

        if (detections is null)
        {
            return;
        }

        foreach (var detection in detections)
        {
            ToneDetected?.Invoke(detection);
        }
    }

    private void ResetForSampleRate(int sampleRate)
    {
        _sampleRate = sampleRate;
        _windowSamples = Math.Max(1, sampleRate * WindowMilliseconds / 1_000);
        _hopSamples = Math.Max(1, sampleRate * HopMilliseconds / 1_000);
        _windowWeights = new double[_windowSamples];
        _windowWeightSum = 0d;
        for (var index = 0; index < _windowWeights.Length; index++)
        {
            var weight = Hamming(index, _windowWeights.Length);
            _windowWeights[index] = weight;
            _windowWeightSum += weight;
        }

        _samples.Clear();
        _candidate = null;
        _candidateWindows = 0;
        _emittedDigit = null;
        _releaseWindows = 0;
    }

    private InBandDtmfDetection? AdvanceState(DtmfWindowAnalysis analysis)
    {
        if (analysis.Digit is not { } digit)
        {
            _candidate = null;
            _candidateWindows = 0;
            if (_emittedDigit is not null && ++_releaseWindows >= RequiredReleaseWindows)
            {
                _emittedDigit = null;
                _releaseWindows = 0;
            }

            return null;
        }

        _releaseWindows = 0;
        if (_candidate == digit)
        {
            _candidateWindows++;
        }
        else
        {
            _candidate = digit;
            _candidateWindows = 1;
        }

        if (_candidateWindows < RequiredCandidateWindows || _emittedDigit == digit)
        {
            return null;
        }

        _emittedDigit = digit;
        return new InBandDtmfDetection(
            digit,
            _sampleRate,
            WindowMilliseconds +
            (RequiredCandidateWindows - 1) * HopMilliseconds,
            analysis.Rms,
            analysis.ToneEnergyRatio,
            analysis.LowGroupAmplitude,
            analysis.HighGroupAmplitude);
    }

    private DtmfWindowAnalysis AnalyzeWindow(
        IReadOnlyList<short> samples,
        int sampleCount,
        int sampleRate)
    {
        _analyzedWindows++;
        var mean = 0d;
        for (var index = 0; index < sampleCount; index++)
        {
            mean += samples[index];
        }

        mean /= sampleCount;
        var sumSquares = 0d;
        for (var index = 0; index < sampleCount; index++)
        {
            var centered = samples[index] - mean;
            sumSquares += centered * centered;
        }

        var rms = Math.Sqrt(sumSquares / sampleCount);
        if (rms < MinimumRms)
        {
            return DtmfWindowAnalysis.None(rms);
        }
        _audibleWindows++;

        var lowAmplitudes = MeasureGroup(
            samples,
            sampleCount,
            sampleRate,
            mean,
            LowGroupFrequencies);
        var highAmplitudes = MeasureGroup(
            samples,
            sampleCount,
            sampleRate,
            mean,
            HighGroupFrequencies);
        var lowIndex = IndexOfMaximum(lowAmplitudes);
        var highIndex = IndexOfMaximum(highAmplitudes);
        var lowAmplitude = lowAmplitudes[lowIndex];
        var highAmplitude = highAmplitudes[highIndex];
        var lowSecond = SecondLargest(lowAmplitudes, lowIndex);
        var highSecond = SecondLargest(highAmplitudes, highIndex);
        var minimumAmplitude = Math.Max(
            MinimumAbsoluteToneAmplitude,
            rms * MinimumRelativeToneAmplitude);
        var twist = lowAmplitude / Math.Max(1d, highAmplitude);
        var toneEnergyRatio =
            (lowAmplitude * lowAmplitude + highAmplitude * highAmplitude) /
            Math.Max(1d, 2d * rms * rms);

        if (lowAmplitude >= minimumAmplitude && highAmplitude >= minimumAmplitude)
        {
            _pairedToneWindows++;
            _maximumPairedToneEnergyRatio = Math.Max(_maximumPairedToneEnergyRatio, toneEnergyRatio);
        }

        if (lowAmplitude < minimumAmplitude ||
            highAmplitude < minimumAmplitude ||
            lowAmplitude / Math.Max(1d, lowSecond) < MinimumDominanceRatio ||
            highAmplitude / Math.Max(1d, highSecond) < MinimumDominanceRatio ||
            toneEnergyRatio < MinimumToneEnergyRatio ||
            twist is < MinimumTwistRatio or > MaximumTwistRatio)
        {
            return DtmfWindowAnalysis.None(rms);
        }

        _candidateToneWindows++;
        return new DtmfWindowAnalysis(
            Digits[lowIndex, highIndex],
            rms,
            toneEnergyRatio,
            lowAmplitude,
            highAmplitude);
    }

    private double[] MeasureGroup(
        IReadOnlyList<short> samples,
        int sampleCount,
        int sampleRate,
        double mean,
        IReadOnlyList<double> frequencies)
    {
        var amplitudes = new double[frequencies.Count];
        for (var frequencyIndex = 0; frequencyIndex < frequencies.Count; frequencyIndex++)
        {
            var maximumAmplitude = 0d;
            foreach (var frequencyScale in FrequencyScales)
            {
                var angularFrequency =
                    2d * Math.PI * frequencies[frequencyIndex] * frequencyScale / sampleRate;
                var coefficient = 2d * Math.Cos(angularFrequency);
                var previous = 0d;
                var previousPrevious = 0d;
                for (var index = 0; index < sampleCount; index++)
                {
                    var current =
                        (samples[index] - mean) * _windowWeights[index] +
                        coefficient * previous -
                        previousPrevious;
                    previousPrevious = previous;
                    previous = current;
                }

                var power =
                    previous * previous +
                    previousPrevious * previousPrevious -
                    coefficient * previous * previousPrevious;
                var amplitude =
                    2d * Math.Sqrt(Math.Max(0d, power)) /
                    _windowWeightSum;
                maximumAmplitude = Math.Max(maximumAmplitude, amplitude);
            }

            amplitudes[frequencyIndex] = maximumAmplitude;
        }

        return amplitudes;
    }

    private static double Hamming(int index, int sampleCount) =>
        sampleCount <= 1
            ? 1d
            : 0.54d - 0.46d * Math.Cos(2d * Math.PI * index / (sampleCount - 1));

    private static int IndexOfMaximum(IReadOnlyList<double> values)
    {
        var maximumIndex = 0;
        for (var index = 1; index < values.Count; index++)
        {
            if (values[index] > values[maximumIndex])
            {
                maximumIndex = index;
            }
        }

        return maximumIndex;
    }

    private static double SecondLargest(IReadOnlyList<double> values, int maximumIndex)
    {
        var second = 0d;
        for (var index = 0; index < values.Count; index++)
        {
            if (index != maximumIndex)
            {
                second = Math.Max(second, values[index]);
            }
        }

        return second;
    }

    private sealed record DtmfWindowAnalysis(
        char? Digit,
        double Rms,
        double ToneEnergyRatio,
        double LowGroupAmplitude,
        double HighGroupAmplitude)
    {
        public static DtmfWindowAnalysis None(double rms) =>
            new(null, rms, 0d, 0d, 0d);
    }
}
