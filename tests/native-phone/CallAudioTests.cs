using System.Net;
using Ivy.PhoneBridge;

static partial class Program {
    sealed class WaitingFixture : IPcmAudioPort {
        public bool Open = true;
        public bool IsOpen => Open;
        public long Generation => 7;
        public int Reads, Writes;
        public int ReadCaptured(Span<float> output) { Reads++; output.Fill(.25f); return output.Length; }
        public void WriteReceived(ReadOnlySpan<float> input) { Writes++; }
    }
    sealed class RouteFixture : ICallAudioRoute {
        public Func<bool> Permitted = () => false;
        public string State = "suspended";
        public int Resumes, Suspends, Disposals;
        public bool FailDispose, FailSuspend;
        public Action? OnRead;
        public TaskCompletionSource<bool>? ReleaseDispose;
        public bool IsOpen => State == "open" && Permitted();
        public bool Failed => State == "failed";
        public long Generation { get; private set; }
        public MediaStatus Status => new(State, "desktop_process", 48000, 1, 0, 0, 0, 0, 10);
        public void Resume() { if (!Permitted() || State is "closed" or "failed") throw new InvalidOperationException("fixture authority or route unavailable"); State = "open"; Generation++; Resumes++; }
        public void Suspend() { Suspends++; if (FailSuspend) throw new InvalidOperationException("fixture suspend failure"); if (State == "open") State = "suspended"; }
        public int ReadCaptured(Span<float> output) { if (!IsOpen) { output.Clear(); return 0; } output.Fill(.25f); OnRead?.Invoke(); return output.Length; }
        public void WriteReceived(ReadOnlySpan<float> input) { Check(Permitted(), "no PCM transfer without the original permit"); }
        public async ValueTask DisposeAsync() {
            Disposals++; if (ReleaseDispose != null) await ReleaseDispose.Task;
            State = "closed"; if (FailDispose) throw new InvalidOperationException("fixture disposal failure");
        }
    }
    static readonly AudioSettings AudioFixtureSettings = new("capture-fixture", "render-fixture", "desktop_process", 20, 10, 60);
    static DesktopIdentity AudioFixtureDesktop() => new(123, 1, Path.GetFullPath("fixture-desktop.exe"), "Fixture.Package!UI");
    static async Task ThrowsAsync<T>(Task operation, string message) where T : Exception {
        bool rejected = false; try { await operation; } catch (T) { rejected = true; } Check(rejected, message);
    }
    static async Task CallAudioUnits() {
        var normal = new WaitingFixture(); var waiting = new CallWaitingAudio(normal);
        long before = waiting.Generation; waiting.Begin();
        var progress = new float[AudioSettings.SampleRate / 2]; waiting.ReadCaptured(progress); waiting.WriteReceived(progress);
        Check(waiting.Generation > before && progress.Any(value => Math.Abs(value) > .01f) &&
            normal.Reads == 0 && normal.Writes == 0, "local waiting signal is audible without forwarding Desktop or caller audio");
        long during = waiting.Generation; waiting.End();
        waiting.ReadCaptured(progress); waiting.WriteReceived(progress);
        Check(waiting.Generation > during && normal.Reads == 1 && normal.Writes == 1 && progress.All(value => value == .25f),
            "original audio resumes only after the explicit Voice handoff");
        normal.Open = false; waiting.Acknowledge(true);
        var accepted = new float[960]; waiting.ReadCaptured(accepted);
        Check(accepted.Any(value => Math.Abs(value) > .01f) && normal.Reads == 1 && waiting.IsOpen,
            "a completed keypad command has local feedback even when Desktop audio is unavailable");
        waiting.ReadCaptured(new float[AudioSettings.SampleRate * 160 / 1000 - accepted.Length]);
        waiting.Acknowledge(false);
        var rejected = new float[AudioSettings.SampleRate * 420 / 1000]; waiting.ReadCaptured(rejected);
        Check(rejected[..(AudioSettings.SampleRate * 120 / 1000)].Any(value => Math.Abs(value) > .01f) &&
            rejected[(AudioSettings.SampleRate * 120 / 1000)..(AudioSettings.SampleRate * 200 / 1000)].All(value => value == 0) &&
            rejected[(AudioSettings.SampleRate * 200 / 1000)..(AudioSettings.SampleRate * 320 / 1000)].Any(value => Math.Abs(value) > .01f),
            "failed keypad commands produce a distinct two-tone local response");
        await AudioMuteUnits();
        foreach (bool released in new[] { true, false }) {
            var failedPreparation = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, _, _) =>
                Task.FromException<ICallAudioRoute>(released ? new AudioPreparationReleasedException(new InvalidOperationException()) : new InvalidOperationException()));
            await ThrowsAsync<Exception>(failedPreparation.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop()), "failed preparation remains a failed original operation");
            var cleanup = failedPreparation.DisposeAsync().AsTask();
            if (released) { await cleanup; Check(failedPreparation.Status.State == "closed", "confirmed factory cleanup permits release after preparation failure"); }
            else { await ThrowsAsync<InvalidOperationException>(cleanup, "unclassified factory cleanup remains fenced"); Check(failedPreparation.Status.State == "failed", "unknown preparation resources cannot report closed"); }
        }
        var clock = new Clock(); var permit = new AudioPermit(clock); var route = new RouteFixture(); int preparations = 0;
        var port = new CallAudioPort(Guid.NewGuid().ToString(), permit, (settings, desktop, allowed, token) => {
            preparations++; Check(settings == AudioFixtureSettings && desktop.pid == 123, "original device/Desktop settings reach the factory");
            route.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(route);
        });
        Check(!port.IsOpen && port.Status.State == "unprepared", "new call owns no devices or audio authority");
        await port.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop());
        Check(port.Status.State == "suspended" && route.Resumes == 0, "real route contract starts suspended");
        Reject(port.Activate, "prepared devices do not authorize audio");
        permit.Renew(0, TimeSpan.FromMilliseconds(500)); port.Activate(); Check(port.IsOpen, "separate fresh authority permits activation");
        long generation = port.Generation; port.Suspend(); Check(!port.IsOpen, "suspension stops forwarding"); port.Activate();
        Check(port.Generation > generation, "route generation advances after resume");
        clock.Ticks = 500; Check(!port.IsOpen, "original permit expiry closes wrapper even without another controller command");
        permit.Renew(500, TimeSpan.FromMilliseconds(500)); port.Activate();
        long beforeGap = port.Generation; int previousSuspends = route.Suspends;
        clock.Ticks = 1000; permit.Renew(1000, TimeSpan.FromMilliseconds(500)); // No IsOpen/audio callback saw the expiry.
        Check(!port.IsOpen, "new permit cannot revive an old open device generation without activation");
        port.Activate();
        Check(port.Generation > beforeGap && route.Suspends > previousSuspends, "activation after an unobserved expiry suspends and clears the old route first");
        route.OnRead = port.Revoke; var samples = new float[16];
        Check(port.ReadCaptured(samples) == 0 && samples.All(value => value == 0), "mid-read revocation discards captured speech");
        Reject(port.Activate, "ended call cannot resume");
        Reject(() => port.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop()), "ended/prepared call cannot open another route");
        await port.DisposeAsync(); await port.DisposeAsync();
        Check(preparations == 1 && route.Disposals == 1 && port.Status.State == "closed", "one preparation and disposal per original call");

        foreach (bool failDuringRead in new[] { false, true }) {
            var faultClock = new Clock(); var faultPermit = new AudioPermit(faultClock); var faultRoute = new RouteFixture();
            await using var faultPort = new CallAudioPort(Guid.NewGuid().ToString(), faultPermit, (_, _, allowed, _) => {
                faultRoute.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(faultRoute);
            });
            await faultPort.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop());
            faultPermit.Renew(0, TimeSpan.FromMilliseconds(500)); faultPort.Activate();
            if (failDuringRead) faultRoute.OnRead = () => faultRoute.State = "failed";
            else faultRoute.State = "failed";
            var discarded = new float[32]; Array.Fill(discarded, .5f);
            Check(faultPort.ReadCaptured(discarded) == 0 && discarded.All(value => value == 0) && faultPort.Failed &&
                !faultPort.IsOpen && faultPort.Status.State == "failed", "device failure before or during capture discards PCM and is distinct from suspension");
            faultRoute.State = "open";
            Reject(faultPort.Activate, "a later provider state cannot clear a terminal failure of the original route");
            Check(faultPort.Failed && !faultPort.IsOpen, "terminal device failure remains latched until original resource cleanup");
        }

        var firstRoute = new RouteFixture(); var replacementRoute = new RouteFixture();
        int routeOpens = 0; var recoveryPermit = new AudioPermit();
        var recoveryDesktop = AudioFixtureDesktop();
        var recovering = new CallAudioPort(Guid.NewGuid().ToString(), recoveryPermit, (_, _, allowed, _) => {
            var selected = ++routeOpens == 1 ? firstRoute : replacementRoute;
            selected.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(selected);
        });
        await recovering.PrepareAsync(AudioFixtureSettings, recoveryDesktop);
        recovering.BindAuthority(AudioFixtureSettings.CaptureEndpointId);
        recovering.GrantAuthority(); recovering.ArmVoiceRecovery();
        firstRoute.State = "failed";
        Check(recovering.Failed && recovering.RecoveryPending && !recovering.IsOpen,
            "one bound Voice call may retain its SIP owner while the failed route is fenced");
        var repaired = await recovering.RebindAsync(recoveryDesktop, () => true);
        Check(repaired.CallId == recovering.CallId && repaired.State == "open" && recovering.IsOpen &&
            firstRoute.Disposals == 1 && replacementRoute.Resumes == 1 && routeOpens == 2,
            "confirmed old disposal permits exactly one replacement route on the same call");
        replacementRoute.State = "failed";
        Check(recovering.Failed && !recovering.RecoveryPending, "second device failure cannot trigger another repair");
        bool repeatRejected = false;
        try { await recovering.RebindAsync(recoveryDesktop, () => true); }
        catch (NativeRpcException) { repeatRejected = true; }
        Check(repeatRejected, "one same-call route repair is the fixed limit");
        await recovering.DisposeAsync();
        Check(replacementRoute.Disposals == 1, "replacement route is released with its original call");

        foreach (bool hangup in new[] { true, false }) {
            var entered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            var finish = new TaskCompletionSource<ICallAudioRoute>(TaskCreationOptions.RunContinuationsAsynchronously);
            using var callerStop = new CancellationTokenSource(); var lateRoute = new RouteFixture();
            var pendingPort = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (settings, desktop, allowed, token) => {
                lateRoute.Permitted = allowed; entered.TrySetResult(true); return finish.Task; // Intentionally ignores cancellation.
            });
            var pending = pendingPort.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop(), callerStop.Token);
            await entered.Task.WaitAsync(TimeSpan.FromSeconds(2));
            if (hangup) pendingPort.Revoke(); else callerStop.Cancel();
            var disposed = pendingPort.DisposeAsync().AsTask();
            Check(!disposed.IsCompleted && !pendingPort.IsOpen, "hangup returns without waiting, while disposal retains original pending preparation");
            finish.SetResult(lateRoute);
            await ThrowsAsync<OperationCanceledException>(pending, "late preparation is cancelled, never adopted"); await disposed;
            Check(lateRoute.Resumes == 0 && lateRoute.Disposals == 1 && pendingPort.Status.State == "closed", "late native route is disposed once after cancellation/hangup");
        }

        var blockedRoute = new RouteFixture { ReleaseDispose = new(TaskCreationOptions.RunContinuationsAsynchronously) };
        var blocked = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) => {
            blockedRoute.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(blockedRoute);
        });
        await blocked.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop());
        var first = blocked.DisposeAsync().AsTask(); var second = blocked.DisposeAsync().AsTask();
        Check(ReferenceEquals(first, second) && !first.IsCompleted, "concurrent callers wait for the same original device disposal");
        blockedRoute.ReleaseDispose.SetResult(true); await first; Check(blockedRoute.Disposals == 1, "blocked disposal is not repeated");

        foreach (bool suspendFailure in new[] { true, false }) {
            var brokenRoute = new RouteFixture { FailSuspend = suspendFailure, FailDispose = !suspendFailure };
            var broken = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) => {
                brokenRoute.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(brokenRoute);
            });
            await broken.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop()); broken.Revoke();
            Check(!broken.IsOpen, "revocation stays closed even when the device adapter fails");
            var failed = broken.DisposeAsync().AsTask(); await ThrowsAsync<InvalidOperationException>(failed, "cleanup failure is retained");
            Check(ReferenceEquals(failed, broken.DisposeAsync().AsTask()) && brokenRoute.Disposals == 1 && broken.Status.State == "failed",
                "failed disposal is neither repeated nor reported as closed");
        }

        var foreign = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit());
        Reject(() => new SipMediaSession(new CodecSettings(new[] { "PCMU" }), foreign, IPAddress.Loopback, Guid.NewGuid().ToString()),
            "another call's port cannot be adopted");
        Check(foreign.Status.State == "unprepared", "rejected foreign owner was not revoked or disposed"); await foreign.DisposeAsync();
        var invalidRoute = new RouteFixture();
        var admitted = new CallAudioPort(Guid.NewGuid().ToString(), new AudioPermit(), (_, _, allowed, _) => {
            invalidRoute.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(invalidRoute);
        });
        await admitted.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop());
        Reject(() => new SipMediaSession(new CodecSettings(new[] { "invalid" }), admitted, IPAddress.Loopback, admitted.CallId),
            "initialization failure closes the exact admitted audio owner");
        Check(admitted.Status.State == "closed" && invalidRoute.Disposals == 1, "failed media construction releases its audio resources");
    }
    sealed class OwnedPcmFixture(string callId) : ICallAudioPort {
        private readonly AudioPort inner = new();
        public string CallId { get; } = callId;
        public int Revocations, Disposals;
        public bool IsOpen => inner.IsOpen;
        public bool Failed => false;
        public long Generation => inner.Generation;
        public int ReadCaptured(Span<float> output) => inner.ReadCaptured(output);
        public void WriteReceived(ReadOnlySpan<float> input) => inner.WriteReceived(input);
        public void Revoke() { if (!inner.IsOpen) return; Revocations++; inner.IsOpen = false; inner.Generation++; }
        public ValueTask DisposeAsync() { Revoke(); Disposals++; return ValueTask.CompletedTask; }
    }
    static async Task DeviceFailureSipLoopback() {
        var route = new RouteFixture(); CallAudioPort? audio = null; int allocations = 0;
        var codec = new CodecSettings(["PCMA"]); var binding = new SipBinding("127.0.0.1", 0, "udp");
        await using var caller = new SipTransportHost(binding, codec, id => {
            allocations++;
            return audio = new CallAudioPort(id, new AudioPermit(), (_, _, allowed, _) => {
                route.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(route);
            });
        }, _ => false);
        await using var callee = new SipTransportHost(binding, codec, _ => new AudioPort(), _ => true);
        var invitation = Invitation(); callee.Incoming += call => invitation.TrySetResult(call);
        var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString());
        await outgoing.Media.PrepareAudioAsync(AudioFixtureSettings, AudioFixtureDesktop());
        var dial = outgoing.DialAsync($"sip:fixture@127.0.0.1:{callee.LocalEndpoint.Port}", null, null, 5);
        var incoming = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5));
        var answer = incoming.AnswerAsync();
        Check((await dial).State == "connected" && (await answer).State == "connected", "device failure fixture establishes an actual localhost SIP call");
        await Until(() => outgoing.Media.SentPackets >= 3, "ordinary permission suspension keeps the same SIP session alive with silence");
        Check(outgoing.Observation.State == "connected" && audio!.Status.State == "suspended" && !audio.Failed,
            "suspended audio is not a terminal device failure");
        route.State = "failed";
        await Until(() => incoming.Observation.State == "local_ended" && outgoing.Observation.State == "local_ended",
            "terminal device failure sends BYE on the original SIP dialog");
        Check(outgoing.Observation.Error == "media_failed" && outgoing.Media.Failed && audio!.Failed && !audio.IsOpen && allocations == 1,
            "device failure stays visible, revokes audio and never allocates a replacement call");
        await caller.ReleaseAsync(outgoing); await callee.ReleaseAsync(incoming);
        Check(route.Disposals == 1, "failed original device route is disposed exactly once");
    }
    static async Task DeviceRecoverySipLoopback() {
        var firstRoute = new RouteFixture(); var nextRoute = new RouteFixture();
        CallAudioPort? audio = null; int openings = 0;
        var codec = new CodecSettings(["PCMA"]); var binding = new SipBinding("127.0.0.1", 0, "udp");
        await using var caller = new SipTransportHost(binding, codec, id => audio = new CallAudioPort(id, new AudioPermit(), (_, _, allowed, _) => {
            var selected = ++openings == 1 ? firstRoute : nextRoute;
            selected.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(selected);
        }), _ => false);
        await using var callee = new SipTransportHost(binding, codec, _ => new AudioPort(), _ => true);
        var invitation = Invitation(); callee.Incoming += call => invitation.TrySetResult(call);
        var outgoing = caller.PrepareOutgoing(Guid.NewGuid().ToString());
        var originalDesktop = AudioFixtureDesktop();
        await outgoing.Media.PrepareAudioAsync(AudioFixtureSettings, originalDesktop);
        var dial = outgoing.DialAsync($"sip:fixture@127.0.0.1:{callee.LocalEndpoint.Port}", null, null, 5);
        var incoming = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5)); var answer = incoming.AnswerAsync();
        Check((await dial).State == "connected" && (await answer).State == "connected", "repair fixture establishes a SIP call");
        audio!.BindAuthority(AudioFixtureSettings.CaptureEndpointId);
        audio.GrantAuthority(); audio.ArmVoiceRecovery();
        firstRoute.State = "failed";
        await Until(() => audio.Failed, "failed route is visible to the original media owner");
        long before = outgoing.Media.SentPackets;
        await Until(() => outgoing.Media.SentPackets >= before + 3, "the SIP dialog keeps transmitting during bounded repair");
        Check(outgoing.Observation.State == "connected" && incoming.Observation.State == "connected" && !outgoing.Media.Failed,
            "one failed Voice route does not tear down the original SIP dialog before repair");
        var repaired = await audio.RebindAsync(originalDesktop, () => true);
        Check(repaired.State == "open" && audio.IsOpen && openings == 2 && firstRoute.Disposals == 1,
            "one replacement route resumes the original call after confirmed old cleanup");
        outgoing.Hangup();
        await Until(() => incoming.Observation.State == "local_ended", "original dialog ends normally after repair");
        await caller.ReleaseAsync(outgoing); await callee.ReleaseAsync(incoming);
        Check(nextRoute.Disposals == 1, "replacement route is disposed with the same SIP call");
    }
    static async Task CallAudioSipLoopback() {
        await DeviceFailureSipLoopback();
        await DeviceRecoverySipLoopback();
        OwnedPcmFixture? a = null, b = null; var settings = new CodecSettings(new[] { "PCMU" }); var binding = new SipBinding("127.0.0.1", 0, "udp");
        await using var first = new SipTransportHost(binding, settings, id => a = new OwnedPcmFixture(id), _ => false);
        await using var second = new SipTransportHost(binding, settings, id => b = new OwnedPcmFixture(id), _ => true);
        var invitation = Invitation(); second.Incoming += call => invitation.TrySetResult(call);
        var outgoing = first.PrepareOutgoing(Guid.NewGuid().ToString());
        var dial = outgoing.DialAsync($"sip:fixture@127.0.0.1:{second.LocalEndpoint.Port}", null, null, 5);
        var incoming = await invitation.Task.WaitAsync(TimeSpan.FromSeconds(5)); var answer = incoming.AnswerAsync();
        Check((await dial).State == "connected" && (await answer).State == "connected", "owned-audio SIP peers connect");
        Check(a != null && b != null && a.CallId == outgoing.Observation.Id && b.CallId == incoming.Observation.Id, "both audio factories receive their exact admitted call IDs");
        await Until(() => outgoing.Media.ReceivedPackets >= 2 && incoming.Media.ReceivedPackets >= 2, "owned synthetic ports carry real local RTP");
        outgoing.Hangup(); Check(!a!.IsOpen && a.Revocations == 1, "local hangup revokes audio synchronously");
        await Until(() => incoming.Observation.State == "local_ended", "remote BYE ends original call");
        Check(!b!.IsOpen && b.Revocations == 1, "remote hangup revokes incoming audio");
        await first.ReleaseAsync(outgoing); await second.ReleaseAsync(incoming);
        await outgoing.Media.DisposeAsync(); await incoming.Media.DisposeAsync();
        Check(a.Disposals == 1 && b.Disposals == 1, "media owns exactly one disposal on each end");
    }
}
