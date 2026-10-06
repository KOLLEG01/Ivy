using System;
using Ivy.PhoneBridge;

internal static class DesktopLaunchTests {
    sealed class Application : IDesktopApplication {
        public DesktopObservation Observation = DesktopObservation.Select(false, new DesktopIdentity[0]);
        public int Activations, Result = 42;
        public bool ThrowObservation, ThrowActivation;
        public DesktopObservation Observe() {
            if (ThrowObservation) throw new Exception("metadata unavailable");
            return Observation;
        }
        public int Activate() {
            Activations++;
            if (ThrowActivation) throw new Exception("lost OS result");
            return Result;
        }
    }
    static void Check(bool condition, string name) { if (!condition) throw new Exception(name); }
    public static void Run() {
        var identity = new DesktopIdentity(17, 639000000000000001, @"C:\fixture\ChatGPT.exe", "Example_package!UI");
        Check(identity.startTimeUtcTicks == "639000000000000001", "ticks retain precision beyond JavaScript integers");
        var application = new Application { Observation = DesktopObservation.Select(true, new[] { identity }) };
        var result = DesktopLaunch.Dispatch(application, true);
        Check(result.phase == "ready" && result.identity == identity && result.activationPid == null && application.Activations == 0,
            "running Desktop is reused without activation");
        application.Observation = DesktopObservation.Select(true, new DesktopIdentity[0]);
        Check(DesktopLaunch.Dispatch(application, true).phase == "waiting" && application.Activations == 0,
            "starting or windowless Desktop is not launched again");
        application.Observation = DesktopObservation.Select(true, new[] { identity, new DesktopIdentity(18, 639000000000000002, @"C:\fixture\ChatGPT.exe", "Example_package!UI") });
        Check(DesktopLaunch.Dispatch(application, true).errorCode == "desktop_ambiguous" && application.Activations == 0,
            "multiple windows never select the first owner");
        application = new Application();
        Check(DesktopLaunch.Dispatch(application, false).errorCode == "desktop_missing" && application.Activations == 0, "launch disabled");
        result = DesktopLaunch.Dispatch(application, true);
        Check(result.phase == "submitted" && result.identity == null && result.activationPid == 42 && application.Activations == 1,
            "one submitted activation is not window or Voice readiness");
        foreach (var state in new[] { DesktopObservation.Unavailable(), null }) {
            application = new Application { Observation = state };
            Check(DesktopLaunch.Dispatch(application, true).errorCode == "desktop_observation_unavailable" && application.Activations == 0, "unavailable observation blocks launch");
        }
        application = new Application { ThrowObservation = true };
        Check(DesktopLaunch.Dispatch(application, true).phase == "not_submitted" && application.Activations == 0, "observation failure has no effects");
        foreach (var pid in new[] { 0, -1 }) {
            application = new Application { Result = pid };
            result = DesktopLaunch.Dispatch(application, true);
            Check(result.phase == "outcome_unknown" && result.activationPid == null && application.Activations == 1, "invalid activation result is unknown without replay");
        }
        application = new Application { ThrowActivation = true };
        Check(DesktopLaunch.Dispatch(application, true).phase == "outcome_unknown" && application.Activations == 1, "lost activation result is not retried");
        foreach (var id in new[] { null, "", "ChatGPT", "Example!UI --debug", "Example!UI\n", new String('a', 129) + "!UI" }) {
            bool rejected = false;
            try { DesktopLaunch.ValidateApplication(id); } catch (ArgumentException) { rejected = true; }
            Check(rejected, "application config is an identity, never command text");
        }
        Console.WriteLine("desktop_launch_unit_passed: fake application only; existing/waiting/ambiguous/unknown/admission cases");
    }
}
