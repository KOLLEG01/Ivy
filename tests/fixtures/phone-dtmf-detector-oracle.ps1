$ErrorActionPreference = 'Stop'
$sourcePath = Join-Path $PSScriptRoot '../../services/phone-bridge/native/phone-runtime/InBandDtmfDetector.cs'
$imports = "using System;`nusing System.Collections.Generic;`nusing System.Linq;`nusing System.Text;`nusing System.Text.Json;`nusing System.Globalization;`n"
$harness = @'
public static class DetectorOracle
{
    public static string Run(string input)
    {
        var output = new StringBuilder();
        foreach (var scenario in JsonSerializer.Deserialize<int[][][]>(input)!)
        {
            var detector = new InBandDtmfDetector();
            detector.ToneDetected += d => output.AppendLine(string.Join(",", new[] {
                "d", ((int)d.Digit).ToString(), d.SampleRate.ToString(),
                d.ConfirmedDurationMilliseconds.ToString(),
                d.Rms.ToString("R", CultureInfo.InvariantCulture),
                d.ToneEnergyRatio.ToString("R", CultureInfo.InvariantCulture),
                d.LowGroupAmplitude.ToString("R", CultureInfo.InvariantCulture),
                d.HighGroupAmplitude.ToString("R", CultureInfo.InvariantCulture)
            }));
            foreach (var action in scenario)
            {
                try { detector.Process(action.Skip(1).Select(x => checked((short)x)).ToArray(), action[0]); }
                catch (ArgumentException) { output.AppendLine("e"); }
                var status = JsonSerializer.SerializeToElement(detector.Diagnostics);
                output.AppendLine("s," + string.Join(",", new[] {
                    "analyzedWindows", "audibleWindows", "pairedToneWindows", "candidateToneWindows",
                    "maximumPairedToneEnergyRatio"
                }.Select(name => status.GetProperty(name).GetRawText())));
            }
        }
        return output.ToString();
    }
}
'@
Add-Type -TypeDefinition ($imports + [IO.File]::ReadAllText($sourcePath) + $harness)
[Console]::Out.Write([Ivy.PhoneBridge.DetectorOracle]::Run([Console]::In.ReadToEnd()))
