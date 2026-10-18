$ErrorActionPreference = 'Stop'
$sourcePath = Join-Path $PSScriptRoot '../../services/phone-bridge/native/phone-runtime/DtmfAccessFailureLimiter.cs'
$imports = "using System;`nusing System.Collections.Generic;`nusing System.Text;`nusing System.Text.Json;`n"
$harness = @'
public static class AccessLimiterOracle
{
    public static string Run(string input)
    {
        var output = new StringBuilder();
        foreach (var scenario in JsonSerializer.Deserialize<long[][][]>(input)!)
        {
            var limiter = new DtmfAccessFailureLimiter();
            foreach (var action in scenario)
            {
                try
                {
                    var now = new DateTimeOffset(action[1], TimeSpan.Zero);
                    var maximum = checked((int)action[2]);
                    var window = TimeSpan.FromTicks(action[3]);
                    if (action[0] == 1)
                    {
                        limiter.RecordFailure(now, maximum, window);
                        output.AppendLine("ok");
                    }
                    else
                    {
                        var result = limiter.Check(now, maximum, window);
                        output.AppendLine($"{(result.Allowed ? "allow" : "deny")}:{result.RetryAfter.Ticks}");
                    }
                }
                catch (ArgumentOutOfRangeException error)
                {
                    output.AppendLine("error:" + (error.ParamName switch
                    {
                        "maximumFailures" => "maximum",
                        "window" => "window",
                        _ => "timestamp"
                    }));
                }
            }
        }
        return output.ToString();
    }
}
'@
Add-Type -TypeDefinition ($imports + [IO.File]::ReadAllText($sourcePath) + $harness)
[Console]::Out.Write([Ivy.PhoneBridge.AccessLimiterOracle]::Run([Console]::In.ReadToEnd()))
