namespace Ivy.PhoneBridge;

public sealed record CallAccessSettings(string Code, int TimeoutSeconds = 60, int MaxFailedAttempts = 5,
    int FailedAttemptWindowSeconds = 600, int FailureResponseDelayMilliseconds = 1000) {
    public void Validate() {
        if (Code == null || Code.Length is < 3 or > 64 || TimeoutSeconds is < 1 or > 120 || MaxFailedAttempts is < 1 or > 100 ||
            FailedAttemptWindowSeconds is < 1 or > 86400 || FailureResponseDelayMilliseconds is < 0 or > 5000)
            throw new ArgumentException("Bounded access challenge required.");
        _ = new DtmfAccessCodeGate(Code);
    }
    public override string ToString() => "CallAccessSettings { protected code }";
}

public sealed record CallFeatureObservation(string Access, bool DisableCodecUpgrade, string Screening, string Command, long CommandSequence,
    string CommandState = null, string Model = null, string ReasoningEffort = null,
    CallFeatureCommand[] Commands = null, bool CommandOverflow = false);
public sealed record CallFeatureCommand(string Command, long Sequence, string Model, string ReasoningEffort);

// Digits and access codes never cross the native pipe. Only completed, authorized semantic
// commands and challenge outcomes are exposed to Hive. SIP/RTP callbacks do no Desktop work.
public sealed class PhoneCallFeatures : IDisposable {
    private readonly object sync = new();
    private readonly DtmfInputDeduplicator deduplicator = new();
    private readonly DtmfAccessFailureLimiter limiter;
    private readonly TimeProvider time;
    private readonly Action hangup;
    private readonly CancellationTokenSource stop = new();
    private DtmfAccessCodeGate gate;
    private CallAccessSettings settings;
    private string access = "trusted", screening = "none", command, model, reasoningEffort;
    private readonly List<CallFeatureCommand> commands = [];
    private bool commandOverflow;
    private long sequence;
    private string buffer = "";
    private bool configured, connected, closed;
    private Task deadline = Task.CompletedTask;
    internal PhoneCallFeatures(DtmfAccessFailureLimiter limiter, Action hangup, TimeProvider time = null) {
        this.limiter = limiter; this.hangup = hangup; this.time = time ?? TimeProvider.System;
    }
    public CallFeatureObservation Observation { get { lock (sync) return new(access, gate?.DisableCodecRenegotiation == true, screening, command, sequence,
        command == null ? null : "pending", model, reasoningEffort, commands.ToArray(), commandOverflow); } }
    internal bool NeedsInBandDtmf { get { lock (sync) return !closed && configured &&
        (access == "awaiting_code" || screening == "waiting" || connected && Authenticated && screening == "none"); } }
    public bool Authenticated { get { lock (sync) return access is "trusted" or "authenticated"; } }
    public void Configure(CallAccessSettings challenge, bool incoming) {
        lock (sync) {
            if (configured || closed) throw new InvalidOperationException("Original call feature policy is immutable.");
            if (challenge != null && !incoming) throw new ArgumentException("Only incoming calls use access challenges.");
            challenge?.Validate(); configured = true;
            if (challenge == null) return;
            settings = challenge;
            access = "invalid_code"; // A refused challenge must never retain trusted authority.
            if (!limiter.Check(time.GetUtcNow(), settings.MaxFailedAttempts, TimeSpan.FromSeconds(settings.FailedAttemptWindowSeconds)).Allowed)
                throw new NativeRpcException("phone_access_limited");
            gate = new DtmfAccessCodeGate(settings.Code); access = "awaiting_code";
        }
    }
    public void Connected() {
        lock (sync) {
            connected = true;
            if (access == "awaiting_code" && !closed) deadline = ChallengeDeadlineAsync();
        }
    }
    private async Task ChallengeDeadlineAsync() {
        try {
            await Task.Delay(TimeSpan.FromSeconds(settings.TimeoutSeconds), time, stop.Token);
            lock (sync) { if (closed || access != "awaiting_code") return; FailAccess("timeout"); }
            hangup();
        } catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
    }
    private void FailAccess(string result) {
        access = result;
        limiter.RecordFailure(time.GetUtcNow(), settings.MaxFailedAttempts, TimeSpan.FromSeconds(settings.FailedAttemptWindowSeconds));
    }
    internal void Digit(char digit, DtmfInputSource source) {
        bool rejected = false;
        lock (sync) {
            if (closed || !configured || !deduplicator.IsNewInput(digit, source)) return;
            if (access == "awaiting_code") {
                var result = gate.ProcessDigit(digit);
                if (result == DtmfAccessCodeProgress.Accepted) access = "authenticated";
                else if (result == DtmfAccessCodeProgress.Rejected) { FailAccess("invalid_code"); rejected = true; }
            } else if (Authenticated) {
                if (screening == "waiting") {
                    if (digit == '1') screening = "accepted";
                    else if (digit == '2') { screening = "declined"; rejected = true; }
                } else if (screening == "none" && connected) {
                    if (digit == '*') buffer = "*";
                    else if (buffer.Length > 0 && buffer.Length < 32) buffer += digit;
                    else buffer = "";
                    if (digit == '#') {
                        string next = null, nextModel = null, nextEffort = null;
                        if (buffer == "*0#") next = "new_voice";
                        else if (buffer.Length == 5 && buffer[0] == '*' && buffer[1] == '1' && buffer[4] == '#') {
                            nextModel = buffer[2] switch { '1' => "luna", '2' => "sol", '3' => "astra", _ => null };
                            nextEffort = buffer[3] switch { '1' => "low", '2' => "medium", '3' => "high", '4' => "xhigh", '5' => "max", '6' => "ultra", _ => null };
                            if (nextModel != null && nextEffort != null && !(nextModel == "luna" && nextEffort == "ultra")) next = "select_voice";
                        }
                        buffer = "";
                        if (next != null) {
                            command = next;
                            if (next == "select_voice") { model = nextModel; reasoningEffort = nextEffort; }
                            sequence++;
                            commands.Add(new(next, sequence, model, reasoningEffort));
                            if (commands.Count > 32) { commands.RemoveAt(0); commandOverflow = true; }
                        }
                    }
                }
            }
        }
        if (rejected) _ = EndAfterDelayAsync();
    }
    private async Task EndAfterDelayAsync() {
        await Task.Yield(); // Never run SIP teardown inside the RTP/decoder callback, even with zero delay.
        try { await Task.Delay(settings?.FailureResponseDelayMilliseconds ?? 0, stop.Token); hangup(); }
        catch (OperationCanceledException) when (stop.IsCancellationRequested) { }
    }
    public void StartScreening() { lock (sync) { if (closed || !Authenticated || screening != "none") throw new InvalidOperationException("Screening requires an authenticated original call."); screening = "waiting"; } }
    public void BridgeScreening() { lock (sync) { if (closed || screening != "accepted") throw new InvalidOperationException("Only accepted screening can bridge."); screening = "bridged"; } }
    public void ScreeningTimeout() { lock (sync) { if (closed || screening != "waiting") return; screening = "timeout"; } hangup(); }
    public void Dispose() { lock (sync) { if (closed) return; closed = true; buffer = ""; stop.Cancel(); } }
}
