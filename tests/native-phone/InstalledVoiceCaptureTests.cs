using Ivy.PhoneBridge;
using NAudio.CoreAudioApi;

static partial class Program {
    // Read-only target acceptance: enumerate device metadata and existing capture sessions only.
    // Never creates an audio stream or changes endpoints, volume, defaults or routing policy.
    static void ObserveInstalledVoiceCapture(string applicationId) {
        var desktop = new WindowsDesktopApplication(applicationId).Observe();
        Check(desktop.state == "ready" && desktop.identity != null, "one current installed Desktop required");
        var identity = desktop.identity!;
        var inventory = new List<(string Id, string Name, DesktopCaptureCorrelation Capture)>();
        using (var devices = new MMDeviceEnumerator()) {
            using var endpoints = devices.EnumerateAudioEndPoints(DataFlow.Capture, DeviceState.Active);
            Check(endpoints.Count is > 0 and <= 32, "bounded active capture endpoint inventory required");
            for (int index = 0; index < endpoints.Count; index++) {
                using var endpoint = endpoints[index];
                inventory.Add((endpoint.ID, endpoint.FriendlyName, DesktopCapture.Observe(identity, endpoint.ID)));
            }
        }
        var matches = inventory.Where(value => value.Capture.State == "matched").ToArray();
        var pairs = new List<object>(); bool allCurrent = matches.Length == 1;
        double? discoveryMs = null;
        if (matches.Length == 1) {
            var selected = matches[0];
            var discovery = System.Diagnostics.Stopwatch.StartNew();
            using var voice = WindowsDesktopControls.OpenVoice(identity);
            discovery.Stop(); discoveryMs = discovery.Elapsed.TotalMilliseconds;
            for (int index = 0; index < 3; index++) {
                var controls = voice.Read(); long capturedAt = TimeProvider.System.GetTimestamp();
                var capture = DesktopCapture.Observe(identity, selected.Id);
                double pairAgeMs = TimeProvider.System.GetElapsedTime(Math.Min(controls.Timestamp, capturedAt)).TotalMilliseconds;
                pairs.Add(new { controls, capturedAt, capture, pairAgeMs });
                allCurrent &= controls.Observation.State == "active" && capture.State == "matched" &&
                    capture.Identity == selected.Capture.Identity && pairAgeMs >= 0 && pairAgeMs < 500;
            }
        }
        object Loaded(System.Reflection.Assembly assembly) => new { name = assembly.GetName().Name, path = assembly.Location,
            sha256 = Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(File.ReadAllBytes(assembly.Location))) };
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new {
            schemaVersion = 1, readOnly = true, loadedAssemblies = new[] { Loaded(typeof(Program).Assembly), Loaded(typeof(DesktopCapture).Assembly) },
            framework = System.Runtime.InteropServices.RuntimeInformation.FrameworkDescription,
            desktop = new { identity.pid, identity.startTimeUtcTicks, identity.appUserModelId },
            inventory = inventory.Select(value => new { endpointId = value.Id, name = value.Name, capture = value.Capture }).ToArray(),
            discoveryMs, pairs
        }, NativeRpc.Json));
        Check(allCurrent, "one real Desktop capture must remain paired with original active Voice controls within500ms");
    }
}
