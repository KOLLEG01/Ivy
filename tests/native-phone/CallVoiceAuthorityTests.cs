using Ivy.PhoneBridge;

static partial class Program {
    static async Task VoiceAuthorityUnits() {
        var clock = new Clock { Ticks = 1000 }; var route = new RouteFixture();
        var permit = new AudioPermit(clock); string id = Guid.NewGuid().ToString(); bool current = true;
        await using var port = new CallAudioPort(id, permit, (_, _, allowed, _) => {
            route.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(route);
        });
        var voiceId = new VoiceControlIdentity("123", [42, 0], [42, 1], [42, 2]);
        var captureId = new DesktopCaptureIdentity("original-session", 123, "1");
        var binding = new VoiceAudioBinding(id, AudioFixtureSettings.CaptureEndpointId, voiceId, captureId);
        Reject(() => new CallVoiceAuthority(port, binding, () => current, clock), "unprepared devices cannot bind Voice authority");
        await port.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop());
        Reject(() => new CallVoiceAuthority(port, binding with { CallId = Guid.NewGuid().ToString() }, () => current, clock), "foreign call refused");
        Reject(() => new CallVoiceAuthority(port, binding with { EndpointId = "other-endpoint" }, () => current, clock), "foreign endpoint refused");
        var authority = new CallVoiceAuthority(port, binding, () => current, clock);
        Reject(() => new CallVoiceAuthority(port, binding, () => true, clock), "second authority cannot replace original binding");
        Check(!port.IsOpen, "binding itself never opens audio");
        voiceId.MicrophoneId[1] = 99;
        var expected = new VoiceControlIdentity("123", [42, 0], [42, 1], [42, 2]);
        TimedVoiceControlObservation Voice(long timestamp, string state = "active", VoiceControlIdentity? identity = null) =>
            new(timestamp, new(state, identity ?? expected));
        var capture = new DesktopCaptureCorrelation(binding.EndpointId, "matched", captureId);
        Check(authority.Update(Voice(900), 950, capture) && port.IsOpen, "fresh original UI/capture observations permit prepared call");
        clock.Ticks = 1400;
        Check(!port.IsOpen, "audio expires500ms from earlier UI start, not capture time or completion");
        Check(authority.Update(Voice(1400), 1400, capture), "fresh original identities renew after expiry");
        int suspends = route.Suspends; long generation = port.Generation;
        clock.Ticks = 1410;
        Check(!authority.Update(Voice(1410, "muted"), 1410, capture) && !permit.IsCurrent() && !port.IsOpen,
            "mute invalidates permit immediately before previous lease expiry");
        Check(!authority.Update(Voice(1405), 1405, capture), "older positive cannot undo newer mute");
        clock.Ticks = 1420;
        Check(authority.Update(Voice(1420), 1420, capture) && route.Suspends > suspends && port.Generation > generation,
            "same original session resumes only with new observations and clean audio generation");
        Check(!authority.Update(Voice(1420), 1420, capture) && !port.IsOpen, "replaying identical observation cannot renew or preserve authority");
        foreach (var changed in new[] {
            capture with { State = "none", Identity = null! }, capture with { State = "ambiguous", Identity = null! },
            capture with { State = "unavailable", Identity = null! }, capture with { EndpointId = "other" },
            capture with { Identity = captureId with { InstanceId = "replacement-session" } },
            capture with { Identity = captureId with { Pid = 124 } },
            capture with { Identity = captureId with { ProcessStartTimeUtcTicks = "2" } }
        }) {
            clock.Ticks += 10;
            Check(!authority.Update(Voice(clock.Ticks), clock.Ticks, changed) && !permit.IsCurrent(), "foreign/missing capture never adopts another session");
        }
        clock.Ticks += 10;
        Check(!authority.Update(Voice(clock.Ticks, identity: expected with { StopId = [42, 9] }), clock.Ticks, capture), "replacement Voice group refused");
        clock.Ticks += 10;
        Check(!authority.Update(Voice(clock.Ticks - 500), clock.Ticks, capture), "slow App observation is not refreshed by fast capture");
        Check(!authority.Update(Voice(clock.Ticks), clock.Ticks - 500, capture), "slow capture is not refreshed by fast UI");
        Check(!authority.Update(Voice(clock.Ticks + 1), clock.Ticks, capture), "future UI clock refused");
        Check(!authority.Update(Voice(clock.Ticks), clock.Ticks + 1, capture), "future capture clock refused");
        Check(authority.Update(Voice(clock.Ticks), clock.Ticks, capture), "unchanged original binding remains usable with new actual observations");
        current = false; clock.Ticks += 10;
        Check(!authority.Update(Voice(clock.Ticks), clock.Ticks, capture) && !port.IsOpen, "original call loss revokes immediately");
        current = true; clock.Ticks += 10;
        Check(!authority.Update(Voice(clock.Ticks), clock.Ticks, capture), "revoked owner can never resume or bind another call");

        route = new RouteFixture(); permit = new AudioPermit(clock);
        await using var racingPort = new CallAudioPort(Guid.NewGuid().ToString(), permit, (_, _, allowed, _) => {
            route.Permitted = allowed; return Task.FromResult<ICallAudioRoute>(route);
        });
        await racingPort.PrepareAsync(AudioFixtureSettings, AudioFixtureDesktop());
        int reads = 0;
        var racing = new CallVoiceAuthority(racingPort, binding with { CallId = racingPort.CallId, Voice = expected }, () => ++reads < 3, clock);
        Check(!racing.Update(Voice(clock.Ticks), clock.Ticks, capture) && !racingPort.IsOpen && !permit.IsCurrent(),
            "call loss during activation cannot leave audio open");
    }
}
