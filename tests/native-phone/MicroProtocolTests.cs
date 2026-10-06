using System.Text;
using System.Text.Json;
using Ivy.PhoneBridge;

static partial class Program {
    static void MicroProtocolChecks() {
        static string Decode(byte[][] reports) => Encoding.UTF8.GetString(reports.SelectMany(r => r.Skip(3).Take(r[2])).ToArray());
        static byte[][] Send(MicroProtocol protocol, string json) => MicroProtocol.Frame(Encoding.UTF8.GetBytes(json)).SelectMany(r => protocol.Receive(r)).ToArray();
        var device = new MicroProtocol();
        Check(!device.SawStatus && !device.SawRgbConfiguration, "a new connection has no retained handshake");
        using (var reply = JsonDocument.Parse(Decode(Send(device, "{\"i\":\"17\",\"m\":\"device.status\"}")))) {
            Check(reply.RootElement.GetProperty("id").GetInt32() == 17, "compact numeric string ID is answered numerically");
            var status = reply.RootElement.GetProperty("result");
            Check(status.GetProperty("version").GetString() == "1.0.0" && status.GetProperty("battery").GetInt32() == 100 &&
                !status.GetProperty("is_charging").GetBoolean(), "required real firmware status fields");
        }
        foreach (var method in new[] { "v.oai.rgbcfg", "v.oai.thstatus", "lights.preview", "future.configuration" }) {
            using var reply = JsonDocument.Parse(Decode(Send(device, JsonSerializer.Serialize(new { id = 18, method, @params = new { keys = new { b = 50 } } }))));
            Check(reply.RootElement.GetProperty("result").GetBoolean(), "configuration is acknowledged without physical hardware: " + method);
        }
        Check(device.SawStatus && device.SawRgbConfiguration, "handshake requests recorded only after parsed RPC");
        Check(device.StatusRequests == 1, "other configuration calls do not refresh the status heartbeat");
        Check(device.DictationSequence == 0 && device.LightingState == "unknown", "unrelated lighting cannot impersonate Dictation");
        Send(device, "{\"id\":181,\"m\":\"v.oai.rgbcfg\",\"p\":{\"ambient\":{\"e\":2,\"c\":3050327}}}");
        Check(device.DictationSequence == 1 && device.LightingState == "dictation_recording", "the ui's exact recording light identifies Dictation");
        Send(device, "{\"id\":182,\"method\":\"v.oai.rgbcfg\",\"params\":{\"ambient\":{\"effect\":2,\"color\":3050327}}}");
        Check(device.DictationSequence == 1, "repeated recording lighting remains one transition");
        Send(device, "{\"id\":183,\"m\":\"v.oai.rgbcfg\",\"p\":{\"ambient\":{\"e\":2,\"c\":16777215}}}");
        Send(device, "{\"id\":184,\"m\":\"v.oai.rgbcfg\",\"p\":{\"ambient\":{\"e\":2,\"c\":3050327}}}");
        Check(device.DictationSequence == 2 && device.LightingState == "dictation_recording", "a later Dictation recording has a fresh sequence");
        using (var reply = JsonDocument.Parse(Decode(Send(device, "{\"id\":19,\"method\":\"sys.version\"}"))))
            Check(reply.RootElement.GetProperty("result").GetString() == "1.0.0", "version response has firmware shape");

        // Position a multibyte character across every possible HID payload boundary.
        for (int padding = 0; padding < 61; padding++) {
            var unicode = "probe." + new string('a', padding) + "🙂ä";
            var input = "{\"id\":20,\"method\":\"" + unicode + "\",\"params\":\"}\\\"{\"}";
            var reports = MicroProtocol.Frame(Encoding.UTF8.GetBytes(input));
            var replies = reports.SelectMany(r => device.Receive(r)).ToArray();
            Check(replies.Length > 0 && device.LastMethod == unicode, "UTF8 and escaped braces survive arbitrary report splits");
        }
        var combined = Decode(Send(device, "{\"id\":21,\"m\":\"sys.version\"}{\"id\":22,\"m\":\"device.status\"}\n"));
        Check(combined.Split('\n', StringSplitOptions.RemoveEmptyEntries).Length == 2, "unterminated inbound objects can be coalesced");
        Check(Send(device, "{\"m\":\"host.notification\"}").Length == 0, "host notification gets no unsolicited reply");
        foreach (bool pressed in new[] { true, false }) {
            using var notification = JsonDocument.Parse(Decode(MicroProtocol.Microphone(pressed)));
            Check(notification.RootElement.GetProperty("m").GetString() == "v.oai.hid" &&
                notification.RootElement.GetProperty("p").GetProperty("k").GetString() == "ACT10" &&
                notification.RootElement.GetProperty("p").GetProperty("act").GetInt32() == (pressed ? 1 : 0) &&
                !notification.RootElement.TryGetProperty("id", out _), "only microphone press/release, never task or approval keys");
        }
        Reject(() => device.Receive(new byte[63]), "truncated report refused");
        foreach (var header in new[] { new byte[] { 5, 2, 1 }, new byte[] { 6, 3, 1 }, new byte[] { 6, 2, 62 } }) {
            var report = new byte[64]; header.CopyTo(report, 0);
            Reject(() => device.Receive(report), "foreign interface/channel or oversized payload refused");
        }
        Reject(() => Send(new(), "{\"id\":1,\"m\":12}"), "non-string method refused");
        Reject(() => Send(new(), "{\"id\":9007199254740992,\"m\":\"x\"}"), "inexact ID refused");
        Reject(() => Send(new(), "{bad}"), "malformed JSON refused");
        Reject(() => MicroProtocol.Frame(new byte[MicroProtocol.MaximumMessageBytes + 1]), "outbound size bounded");
        var bounded = new MicroProtocol();
        Send(bounded, "{" + new string(' ', MicroProtocol.MaximumMessageBytes - 1));
        Reject(() => Send(bounded, " "), "unfinished inbound object cannot grow without bound");
        var fresh = new MicroProtocol();
        Check(!fresh.SawStatus && !fresh.SawRgbConfiguration && fresh.LastMethod == null, "replacement owns fresh protocol state");
    }
}
