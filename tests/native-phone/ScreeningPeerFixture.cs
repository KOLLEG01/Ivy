using Ivy.PhoneBridge;
using System.Collections.Concurrent;
using System.Text.Json;

static partial class Program {
    static async Task ScreeningPeerFixture() {
        string digits = " 1";
        var audio = new ConcurrentDictionary<string, KeypadPort>();
        var work = new ConcurrentBag<Task>();
        object output = new();
        void Write(object value) { lock (output) Console.WriteLine(JsonSerializer.Serialize(value)); }
        await using var host = new SipTransportHost(new("127.0.0.1", 0, "udp"), new(["PCMA"]), id => {
            var port = new KeypadPort { Digits = Volatile.Read(ref digits) }; audio[id] = port; return port;
        }, _ => true);
        host.Incoming += call => work.Add(Handle(call));
        async Task Handle(SipCall call) {
            try {
                call.ConfigureFeatures(null); await call.AnswerAsync();
                Write(new { state = "connected", id = call.Observation.SipCallId });
                long until = Environment.TickCount64 + 30000;
                while (call.Observation.State == "connected" && Environment.TickCount64 < until) await Task.Delay(20);
                if (call.Observation.State == "connected") call.Hangup();
                var port = audio[call.Observation.Id];
                await host.ReleaseAsync(call);
                Write(new { state = "ended", samples = port.Received, energy = port.Energy });
            } catch (Exception error) { Write(new { state = "failed", error = error.GetType().Name }); }
        }
        Write(new { state = "ready", port = host.LocalEndpoint.Port });
        while (await Console.In.ReadLineAsync() is { } command) {
            digits = command switch { "accept" => " 1", "decline" => " 2", "silent" => "", _ => throw new ArgumentException("Unknown fixture mode.") };
            Write(new { state = "mode", mode = command });
        }
        foreach (var call in host.Calls) call.Hangup();
        await Task.WhenAll(work);
    }
}
