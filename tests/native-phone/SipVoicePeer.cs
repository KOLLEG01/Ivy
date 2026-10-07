using System.Diagnostics;
using System.Text.Json;
using Ivy.PhoneBridge;

static partial class Program {
    // Local installed acceptance peer. Carries actual SIP/RTP using PCM memory only.
    sealed class VoicePeerAudio(string callId) : IPcmAudioPort {
        private readonly object sync = new();
        private readonly Stopwatch elapsed = Stopwatch.StartNew();
        private float[] speech = [];
        private int offset;
        private bool heard;
        private long quiet;
        private long samples;
        public bool IsOpen => true;
        public long Generation => 0;
        public void Speak(string wavPath) {
            Check(Path.IsPathFullyQualified(wavPath), "fixture WAV requires an absolute path");
            using var file = File.OpenRead(wavPath);
            Check(file.Length is >= 44 and <= 10485760, "fixture WAV is bounded");
            var bytes = new byte[file.Length]; file.ReadExactly(bytes);
            var pcm = ScreeningAudio.Parse(bytes);
            lock (sync) { speech = pcm; offset = 0; }
        }
        public int ReadCaptured(Span<float> output) {
            lock (sync) {
                output.Clear();
                int length = Math.Min(output.Length, speech.Length - offset);
                speech.AsSpan(offset, length).CopyTo(output); offset += length;
                return output.Length;
            }
        }
        public void WriteReceived(ReadOnlySpan<float> input) {
            lock (sync) {
                samples += input.Length;
                float peak = 0; foreach (float sample in input) peak = Math.Max(peak, Math.Abs(sample));
                if (peak <= .01f) { quiet += input.Length; return; }
                if (!heard || quiet >= 12000)
                    EmitPeer(new { @event = heard ? "audioSegment" : "firstAudio", callId, afterAdmissionMs = elapsed.Elapsed.TotalMilliseconds, peak });
                heard = true; quiet = 0;
            }
        }
        public object Status { get { lock (sync) return new { callId, heard, receivedSamples = samples }; } }
    }
    static void EmitPeer(object value) => Console.WriteLine(JsonSerializer.Serialize(value));
    static async Task SipVoicePeer() {
        var ports = new Dictionary<string, VoicePeerAudio>();
        var monitors = new List<Task>();
        await using var host = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp", RtpPortRangeStart: 32000, RtpPortRangeEnd: 32200),
            new CodecSettings(["EVS", "G722", "PCMA", "PCMU"]), id => ports[id] = new VoicePeerAudio(id), _ => true);
        void Monitor(SipCall call, bool incoming, string caller = "fixture") {
            monitors.Add(Task.Run(async () => {
                try {
                    if (incoming) await call.AnswerAsync();
                    else {
                        var observed = await call.DialAsync("sip:fixture@127.0.0.1:5070", caller, null, 30);
                        Check(observed.State == "connected", "fixture connects only to the local PhoneBridge");
                    }
                    EmitPeer(new { @event = "connected", callId = call.Observation.Id });
                    var began = Stopwatch.StartNew();
                    while (call.Observation.State == "connected" && began.Elapsed < TimeSpan.FromSeconds(600)) await Task.Delay(20);
                    call.Hangup(); await host.ReleaseAsync(call);
                    EmitPeer(new { @event = "ended", audio = ports[call.Observation.Id].Status });
                } catch (Exception error) { EmitPeer(new { @event = "failure", message = error.Message }); }
            }));
        }
        host.Incoming += call => Monitor(call, true);
        EmitPeer(new { @event = "ready", port = host.LocalEndpoint.Port });
        string? line;
        while ((line = await Console.In.ReadLineAsync()) != null) {
            Check(line.Length <= 4096, "fixture command is bounded");
            using var document = JsonDocument.Parse(line);
            var command = document.RootElement;
            if (command.GetProperty("action").GetString() == "stop") break;
            if (command.GetProperty("action").GetString() == "dial") {
                Check(host.CurrentCall == null, "fixture redial waits for its own release");
                string caller = command.TryGetProperty("caller", out var name) ? name.GetString()! : "fixture";
                Check(System.Text.RegularExpressions.Regex.IsMatch(caller, @"^fixture[0-9]{0,2}$"), "only local fixture caller identities allowed");
                Monitor(host.PrepareOutgoing(Guid.NewGuid().ToString()), false, caller); continue;
            }
            var call = host.CurrentCall ?? throw new Exception("fixture has no active call");
            if (command.GetProperty("action").GetString() == "speak") ports[call.Observation.Id].Speak(command.GetProperty("wavPath").GetString()!);
            else if (command.GetProperty("action").GetString() == "hangup") call.Hangup();
        }
        foreach (var call in host.Calls) call.Hangup();
        await Task.WhenAll(monitors);
    }
}
