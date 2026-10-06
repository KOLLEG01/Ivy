using System.Net;
using System.Net.Sockets;
using SIPSorcery.SIP;
using SIPSorcery.SIP.App;

namespace Ivy.PhoneBridge;

// Credentials are a separate protected runtime input and never part of a printable settings record.
public sealed record SipRegistrationSettings(string AccountUri, string RegistrarUri, string ContactUser,
    string AuthUsername, string Realm, int ExpirySeconds, int RetrySeconds, int KeepAliveIntervalSeconds = 20) {
    public void Validate(SIPProtocolsEnum protocol) {
        foreach (string value in new[] { AccountUri, RegistrarUri, ContactUser, AuthUsername })
            if (string.IsNullOrWhiteSpace(value) || value.Length > 2048 || value.Any(char.IsControl)) throw new ArgumentException("Invalid SIP registration settings.");
        if (ContactUser.Length > 256 || AuthUsername.Length > 256 ||
            (Realm != null && (string.IsNullOrWhiteSpace(Realm) || Realm.Length > 256 || Realm.Any(char.IsControl))) ||
            ExpirySeconds is < 10 or > 7200 || RetrySeconds is < 10 or > 600 || KeepAliveIntervalSeconds is < 0 or > 300)
            throw new ArgumentException("Unbounded SIP registration settings.");
        var account = SIPURI.ParseSIPURI(AccountUri); var registrar = SIPURI.ParseSIPURI(RegistrarUri);
        if (string.IsNullOrWhiteSpace(account.User) || registrar.Protocol != protocol)
            throw new ArgumentException("Account identity and matching registrar transport required.");
    }
}
public sealed record SipRegistrationObservation(string State, int? ResponseCode, bool? RemoteRemoved,
    string LocalEndpoint = null, string RemoteEndpoint = null, long? GrantedExpirySeconds = null,
    DateTimeOffset? LastKeepAliveAt = null, string LastKeepAliveError = null);

public sealed class SipRegistration : IAsyncDisposable {
    private readonly object sync = new();
    private readonly SIPRegistrationUserAgent agent;
    private readonly TimeProvider time;
    private readonly int expirySeconds;
    private readonly TaskCompletionSource removed = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private string state = "registering";
    private int? responseCode;
    private bool stopped;
    private long confirmedAt;
    private TimeSpan validFor;
    private Task disposal;
    private readonly SIPTransport transport;
    private readonly CancellationTokenSource keepAliveStop = new();
    private readonly Task keepAlive;
    private SIPEndPoint local, remote;
    private DateTimeOffset? lastKeepAliveAt;
    private string lastKeepAliveError;
    private long? grantedExpiry;
    internal bool MatchesPeer(SIPEndPoint endpoint) {
        lock (sync) return Observation.State == "registered" && remote != null &&
            remote.Protocol == endpoint.Protocol && remote.Address.Equals(endpoint.Address);
    }
    public SipRegistrationObservation Observation {
        get {
            lock (sync) {
                if (state == "registered" && (time.GetElapsedTime(confirmedAt) < TimeSpan.Zero || time.GetElapsedTime(confirmedAt) >= validFor)) state = "failed";
                return new(state, responseCode, removed.Task.IsCompletedSuccessfully ? true : null,
                    local?.ToString(), remote?.ToString(), grantedExpiry, lastKeepAliveAt, lastKeepAliveError);
            }
        }
    }
    internal SipRegistration(SIPTransport transport, SIPProtocolsEnum protocol, SipRegistrationSettings settings, string password, TimeProvider time = null, SIPEndPoint outboundProxy = null) {
        settings.Validate(protocol);
        if (password == null || password.Length > 4096 || password.Any(char.IsControl)) throw new ArgumentException("Protected bounded SIP password required.");
        this.time = time ?? TimeProvider.System; expirySeconds = settings.ExpirySeconds; this.transport = transport;
        var account = SIPURI.ParseSIPURI(settings.AccountUri);
        var contact = new SIPURI(account.Scheme, IPAddress.Any, 0) { User = settings.ContactUser, Protocol = protocol };
        agent = new SIPRegistrationUserAgent(transport, outboundProxy, account, settings.AuthUsername, password, settings.Realm,
            settings.RegistrarUri, contact, settings.ExpirySeconds, [], maxRegistrationAttemptTimeout: 10,
            registerFailureRetryInterval: settings.RetrySeconds, maxRegisterAttempts: 3, exitOnUnequivocalFailure: false);
        agent.RegistrationSuccessful += Success;
        agent.RegistrationFailed += Failure;
        agent.RegistrationTemporaryFailure += Failure;
        agent.RegistrationRemoved += Removed;
        try { agent.Start(); } catch { agent.Stop(false); Unsubscribe(); keepAliveStop.Dispose(); throw; }
        keepAlive = protocol == SIPProtocolsEnum.udp && settings.KeepAliveIntervalSeconds > 0
            ? KeepAliveAsync(TimeSpan.FromSeconds(settings.KeepAliveIntervalSeconds)) : Task.CompletedTask;
    }
    private void Success(SIPURI uri, SIPResponse response) {
        lock (sync) {
            if (stopped) return;
            long expires = EffectiveExpiry(response.Header, expirySeconds);
            confirmedAt = time.GetTimestamp(); validFor = TimeSpan.FromSeconds(expires);
            responseCode = response.StatusCode; state = expires > 0 ? "registered" : "failed";
            local = response.LocalSIPEndPoint; remote = response.RemoteSIPEndPoint; grantedExpiry = expires;
        }
    }
    internal async Task SendKeepAliveAsync() {
        SIPEndPoint source, target;
        lock (sync) {
            if (stopped || Observation.State != "registered" || local == null || remote == null) return;
            source = local; target = remote;
        }
        string error = null;
        try {
            var result = await transport.SendRawAsync(source, target, new byte[] { 13, 10, 13, 10 });
            if (result != SocketError.Success) error = result.ToString();
        } catch { error = "keepalive_send_failed"; }
        lock (sync) {
            if (stopped || !ReferenceEquals(local, source) || !ReferenceEquals(remote, target)) return;
            lastKeepAliveError = error;
            if (error == null) lastKeepAliveAt = time.GetUtcNow();
        }
    }
    private async Task KeepAliveAsync(TimeSpan interval) {
        using var timer = new PeriodicTimer(interval);
        try { while (await timer.WaitForNextTickAsync(keepAliveStop.Token)) await SendKeepAliveAsync(); }
        catch (OperationCanceledException) when (keepAliveStop.IsCancellationRequested) { }
    }
    internal static long EffectiveExpiry(SIPHeader header, int requestedExpiry) {
        // A Contact's expires parameter overrides the general Expires header. The registrar
        // may grant a longer lifetime than requested; the registration agent renews on that schedule.
        long fallback = header.Expires >= 0 ? header.Expires : requestedExpiry;
        return header.Contact is { Count: > 0 }
            ? header.Contact.Min(contact => contact.Expires >= 0 ? contact.Expires : fallback)
            : fallback;
    }
    private void Failure(SIPURI uri, SIPResponse response, string ignoredRemoteText) {
        lock (sync) { if (stopped) return; state = "failed"; responseCode = response?.StatusCode; }
    }
    private void Removed(SIPURI uri, SIPResponse response) {
        lock (sync) { if (!stopped) return; responseCode = response?.StatusCode; removed.TrySetResult(); }
    }
    private void Unsubscribe() {
        agent.RegistrationSuccessful -= Success; agent.RegistrationFailed -= Failure;
        agent.RegistrationTemporaryFailure -= Failure; agent.RegistrationRemoved -= Removed;
    }
    public ValueTask DisposeAsync() {
        lock (sync) { disposal ??= StopCoreAsync(); return new ValueTask(disposal); }
    }
    private async Task StopCoreAsync() {
        bool wasRegistered;
        lock (sync) { stopped = true; state = "stopped"; wasRegistered = agent.IsRegistered; }
        await Task.Yield(); keepAliveStop.Cancel();
        await keepAlive; keepAliveStop.Dispose(); agent.Stop(true);
        if (wasRegistered) {
            try { await removed.Task.WaitAsync(TimeSpan.FromSeconds(3)); } catch (TimeoutException) { /* Local stop does not prove remote removal. */ }
        }
        Unsubscribe();
    }
}
