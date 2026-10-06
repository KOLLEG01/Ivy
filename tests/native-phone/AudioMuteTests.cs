using Ivy.PhoneBridge;

static partial class Program {
    sealed class MuteEndpoint(string id) : IAudioMuteEndpoint {
        public string Id { get; set; } = id;
        public bool ActiveRender { get; set; } = true;
        public bool Muted { get; set; }
        public int Opens, Disposals;
        public void Dispose() { Disposals++; }
    }
    static async Task AudioMuteUnits() {
        var speaker = new MuteEndpoint("speaker"); var cable = new MuteEndpoint("cable") { Muted = true };
        IAudioMuteEndpoint Open(string id) { var endpoint = id == "speaker" ? speaker : cable; endpoint.Opens++; return endpoint; }
        int failed = 0;
        var policy = new AudioMutePolicy("speaker", true, 1);
        await using (var first = AudioMuteGuard.Start(policy, "cable", () => Interlocked.Increment(ref failed), Open)) {
            Check(speaker.Muted && !cable.Muted, "explicit endpoints are muted/unmuted before audio starts");
            await using (var second = AudioMuteGuard.Start(policy, "cable", () => { }, Open)) {
                Reject(() => AudioMuteGuard.Start(new(null, true), "speaker", () => { }, Open), "conflicting active-call mute policy refused before effects");
            }
            speaker.Muted = false; cable.Muted = true;
            await Until(() => speaker.Muted && !cable.Muted, "periodic enforcement repairs changed mute states");
            speaker.ActiveRender = false;
            await Until(() => Volatile.Read(ref failed) == 1, "lost endpoint fails the original audio route");
        }
        Check(speaker.Opens == speaker.Disposals && cable.Opens == cable.Disposals, "all per-observation COM endpoint owners are released");
        speaker.ActiveRender = true;
        await using (var replacement = AudioMuteGuard.Start(new(null, true), "speaker", () => { }, Open))
            Check(!speaker.Muted, "released guards do not retain conflicting leases");
        Check(!speaker.Muted, "cleanup does not restore stale mute state");
        Reject(() => new AudioMutePolicy("cable").Validate("cable"), "receive device cannot also be the muted speaker");
        speaker.Id = "replacement";
        Reject(() => AudioMuteGuard.Start(policy, "cable", () => { }, Open), "endpoint identity replacement refused");
        speaker.Id = "speaker";
        await using var afterFailure = AudioMuteGuard.Start(policy, "cable", () => { }, Open);
    }
}
