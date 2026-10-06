using System;
using System.Collections.Generic;
using Ivy.PhoneBridge;

// No real input driver is constructed. Compiled with the same source as the shipped library.
internal static class VoiceHotkeyTests {
    sealed class Owner : IDesktopOwner {
        public bool Current = true, FocusAllowed = true, Focused = true;
        public int Checks, FocusCalls;
        public bool IsCurrent() { Checks++; return Current; }
        public bool TryFocus() { FocusCalls++; return FocusAllowed; }
        public bool IsFocused() { return Focused; }
    }
    sealed class Keyboard : IKeyboardInput {
        public bool Idle = true;
        public Action OnIdle;
        public Queue<int> Outcomes = new Queue<int>();
        public List<KeyStroke[]> Calls = new List<KeyStroke[]>();
        public bool IsIdle() { if (OnIdle != null) OnIdle(); return Idle; }
        public int Send(KeyStroke[] keys) {
            Calls.Add(keys); int result = Outcomes.Count == 0 ? keys.Length : Outcomes.Dequeue();
            if (result == -1) throw new InvalidOperationException("simulated lost input result");
            return result;
        }
    }
    static void Check(bool value, string name) { if (!value) throw new Exception(name); }
    static HotkeyResult Dispatch(Owner owner, Keyboard keyboard, bool focus = false) {
        return VoiceHotkey.Dispatch(owner, keyboard, new[] { "control", "shift" }, 32, focus);
    }
    static void ForegroundActivation() {
        foreach (var scenario in new[] { "async", "already", "denied", "unresponsive", "replaced", "other-foreground", "restore-failed" }) {
            bool current = true, foreground = scenario == "already";
            int activations = 0, waits = 0;
            bool result = DesktopProcessOwner.ConfirmActivation(() => current, () => foreground,
                () => scenario != "restore-failed", () => { activations++; return scenario != "denied"; },
                () => {
                    waits++;
                    Check(!foreground, "accepted activation is not yet observed foreground");
                    if (scenario == "replaced") current = false;
                    foreground = scenario != "other-foreground";
                    return scenario != "unresponsive";
                });
            Check(result == (scenario == "async" || scenario == "already"), "only confirmed original foreground can proceed: " + scenario);
            Check(activations == (scenario == "already" || scenario == "restore-failed" ? 0 : 1), "no activation retry: " + scenario);
            Check(waits == (scenario == "already" || scenario == "denied" || scenario == "restore-failed" ? 0 : 1), "bounded confirmation only after accepted activation: " + scenario);
        }
        Check(!DesktopProcessOwner.ConfirmActivation(() => false, () => { throw new Exception("stale owner was queried"); },
            () => { throw new Exception("stale owner was restored"); }, () => false, () => false), "stale owner refuses before any window action");
    }
    static int Main() {
        try {
            ForegroundActivation();
            using (var self = System.Diagnostics.Process.GetCurrentProcess()) {
                Check(DesktopProcessOwner.SameSession(self.Id), "real current process shares the caller session");
                Check(!DesktopProcessOwner.SameSession(0) && !DesktopProcessOwner.SameSession(-1) &&
                    !DesktopProcessOwner.SameSession(Int32.MaxValue), "invalid or unavailable process cannot pass native session comparison");
            }
            var owner = new Owner(); var keyboard = new Keyboard();
            var accepted = Dispatch(owner, keyboard);
            Check(accepted.phase == "submitted" && accepted.submitted == 6 && accepted.keyUpSubmitted && keyboard.Calls.Count == 1, "single complete chord");
            Check(owner.FocusCalls == 0, "focus disabled by default");
            var plan = keyboard.Calls[0];
            Check(plan[0].Key == 0x11 && !plan[0].Up && plan[1].Key == 0x10 && !plan[1].Up && plan[2].Key == 32 && !plan[2].Up, "ordered chord");
            Check(plan[3].Key == 32 && plan[3].Up && plan[4].Key == 0x10 && plan[4].Up && plan[5].Key == 0x11 && plan[5].Up, "reverse release");
            foreach (var outcome in new[] { 1, 3, 5, -1, 999 }) {
                keyboard = new Keyboard(); keyboard.Outcomes.Enqueue(outcome);
                var unknown = Dispatch(new Owner(), keyboard);
                Check(unknown.phase == "outcome_unknown" && unknown.keyUpSubmitted && keyboard.Calls.Count == 2, "retain uncertainty");
                Check(keyboard.Calls[1].Length == 3, "bounded cleanup");
                foreach (var stroke in keyboard.Calls[1]) Check(stroke.Up, "cleanup never retries a key-down");
                Check(outcome > 0 && outcome < 6 ? unknown.submitted == outcome : unknown.submitted == null, "unknown count is not invented");
            }
            keyboard = new Keyboard(); keyboard.Outcomes.Enqueue(2); keyboard.Outcomes.Enqueue(-1);
            Check(!Dispatch(new Owner(), keyboard).keyUpSubmitted && keyboard.Calls.Count == 2, "failed cleanup remains unknown without retries");
            keyboard = new Keyboard(); keyboard.Outcomes.Enqueue(0);
            Check(Dispatch(new Owner(), keyboard).phase == "not_submitted" && keyboard.Calls.Count == 1, "zero insertion");
            keyboard = new Keyboard { Idle = false };
            Check(Dispatch(new Owner(), keyboard).errorCode == "keyboard_busy" && keyboard.Calls.Count == 0, "held user keys");
            owner = new Owner { Current = false }; keyboard = new Keyboard();
            Check(Dispatch(owner, keyboard, true).errorCode == "desktop_owner_changed" && keyboard.Calls.Count == 0 && owner.FocusCalls == 0, "wrong owner has no effects");
            owner = new Owner(); keyboard = new Keyboard { OnIdle = () => owner.Current = false };
            Check(Dispatch(owner, keyboard).errorCode == "desktop_owner_changed" && keyboard.Calls.Count == 0, "owner recheck before input");
            owner = new Owner(); keyboard = new Keyboard { OnIdle = () => Check(owner.FocusCalls == 1, "focus precedes keyboard check") };
            Check(Dispatch(owner, keyboard, true).phase == "submitted" && owner.FocusCalls == 1, "explicit focus");
            owner = new Owner { FocusAllowed = false }; keyboard = new Keyboard();
            Check(Dispatch(owner, keyboard, true).errorCode == "desktop_focus_failed" && keyboard.Calls.Count == 0, "failed focus stops input");
            owner = new Owner(); keyboard = new Keyboard { OnIdle = () => owner.Focused = false };
            Check(Dispatch(owner, keyboard, true).errorCode == "desktop_focus_failed" && keyboard.Calls.Count == 0, "lost focus stops input");
            owner = new Owner(); keyboard = new Keyboard();
            Check(VoiceHotkey.Dispatch(owner, keyboard, new[] { "control", "shift" }, 32, true, true).phase == "submitted" &&
                owner.FocusCalls == 0, "prepared focus skips activation while retaining the final foreground check");
            owner = new Owner { Focused = false }; keyboard = new Keyboard();
            Check(VoiceHotkey.Dispatch(owner, keyboard, new[] { "control", "shift" }, 32, true, true).errorCode == "desktop_focus_failed" &&
                keyboard.Calls.Count == 0 && owner.FocusCalls == 0, "prepared focus cannot silently refocus or send to a changed foreground");
            foreach (var modifiers in new[] { new string[0], new[] { "shift" }, new[] { "control", "control" }, new[] { "control", "unknown" } }) {
                bool rejected = false; try { VoiceHotkey.Plan(modifiers, 32); } catch (ArgumentException) { rejected = true; }
                Check(rejected, "invalid modifier configuration");
            }
            foreach (var key in new[] { 0, 16, 31, 33, 91, 111, 136, 255, 65536 }) {
                bool rejected = false; try { VoiceHotkey.Plan(new[] { "control" }, key); } catch (ArgumentException) { rejected = true; }
                Check(rejected, "invalid key configuration");
            }
            Console.WriteLine("voice_hotkey_unit_passed: fake input only; partial/unknown/focus/owner/admission cases");
            DesktopLaunchTests.Run();
            CaptureObservationTests.Run();
            AudioProcessOwnershipTests.Run();
            return 0;
        } catch (Exception error) { Console.Error.WriteLine(error.Message); return 1; }
    }
}
