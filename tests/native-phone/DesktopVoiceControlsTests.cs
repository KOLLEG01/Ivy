using Ivy.PhoneBridge;

static partial class Program {
    static void VoiceControlClassification() {
        var mic = new DesktopControl([42, 1], [42, 0], "Mute microphone", true, false, 0);
        var end = new DesktopControl([42, 2], [42, 0], "Stop voice chat", true, false, null);
        var start = new DesktopControl([42, 3], [42, 8], "Start new voice chat", true, false, null);
        var window = new DesktopControlWindow("123", false, [mic, end, start]);
        VoiceControlObservation Read(params DesktopControlWindow[] windows) => DesktopVoiceControls.Classify(new("observed", windows));
        void State(string expected, params DesktopControlWindow[] windows) {
            var result = Read(windows);
            Check(result.State == expected && (result.Identity != null) == (expected is "active" or "muted"), "Voice state and positive identity: " + expected);
        }
        State("active", window);
        var result = Read(window);
        Check(result.Identity.Window == "123" && result.Identity.ParentId.SequenceEqual([42, 0]) &&
            result.Identity.MicrophoneId.SequenceEqual(mic.Id) && result.Identity.StopId.SequenceEqual(end.Id), "positive preserves exact control grouping");
        mic.Id[1] = 999; mic.ParentId[1] = 999;
        Check(result.Identity.MicrophoneId[1] == 1 && result.Identity.ParentId[1] == 0, "classification owns original identity arrays");
        mic.Id[1] = 1; mic.ParentId[1] = 0;
        foreach (var names in new[] {
            new[] { "Mute microphone", "Unmute microphone", "Stop voice chat", "Start voice chat" },
            new[] { "Mikrofon stummschalten", "Mikrofonstummschaltung aufheben", "Sprachchat stoppen", "Sprachchat starten" }
        }) {
            State("active", window with { Controls = [mic with { Name = names[0] }, end with { Name = names[2] }] });
            State("muted", window with { Controls = [mic with { Name = names[1], Toggle = 1 }, end with { Name = names[2] }] });
            State("idle", window with { Controls = [start with { Name = names[3] }] });
        }
        foreach (int? toggle in new int?[] { null, 1, 2 })
            State("unavailable", window with { Controls = [mic with { Toggle = toggle }, end] });
        State("unavailable", window with { Controls = [mic with { Name = "Unmute microphone", Toggle = 0 }, end] });
        State("unavailable", window with { Controls = [mic, end with { Toggle = 1 }] });
        State("transition", window with { Controls = [mic with { Enabled = false }, end] });
        State("transition", window with { Controls = [mic, end with { Enabled = false }] });
        State("transition", window with { Controls = [mic, end, start with { Name = "Cancel voice chat" }] });
        State("transition", window with { Controls = [end with { Name = "Ending voice chat…" }] });
        State("unavailable", window with { Minimized = true });
        State("unavailable", window with { Controls = [mic with { Offscreen = true }, end, start] });
        State("unavailable", window with { Controls = [end, start] });
        State("unavailable", window with { Controls = [start with { Enabled = false }] });
        State("unavailable", window with { Controls = [] });
        State("ambiguous", window with { Controls = [mic, end with { ParentId = [42, 8] }] });
        State("ambiguous", window with { Controls = [mic] }, new("456", false, [end]));
        State("ambiguous", window with { Controls = [mic, end, end with { Id = [42, 9], Offscreen = true }] });
        State("ambiguous", window with { Controls = [mic, end, mic with { Id = [42, 9], Name = "Unmute microphone", Toggle = 1 }] });
        State("unavailable", window with { Controls = [mic, end, end] });
        State("unavailable", window, window);
        State("unavailable", window with { Controls = [mic with { Name = "message says Mute microphone" }, end] });
        Check(DesktopVoiceControls.Classify(new("unavailable", [window])).Identity == null, "unavailable cannot retain a last positive");
        State("idle", window with { Controls = [start] });
        State("active", window);
        State("unavailable", window with { Controls = [] });
        var labels = DesktopVoiceControls.Labels; labels[0] = "altered";
        Check(!DesktopVoiceControls.Labels.Contains("altered"), "caller cannot alter supported labels");
    }
}
