using System.Net;
using System.Text.Json;
using SIPSorcery.SIP;
using SIPSorcery.SIP.App;

namespace Ivy.PhoneBridge;

public sealed record SipBinding(string Address, int Port, string Transport, int IncomingRingSeconds = 30, string OutgoingOfferMode = "initial",
    int RtpPortRangeStart = 30000, int RtpPortRangeEnd = 31000, string OutboundProxy = null, int MaxConcurrentCalls = 1) {
    public IPEndPoint Endpoint() {
        if (!IPAddress.TryParse(Address, out var address) || Port is < 0 or > 65535 || IncomingRingSeconds is < 1 or > 120 ||
            Transport is not ("udp" or "tcp" or "tls") || OutgoingOfferMode is not ("initial" or "delayed" or "auto") ||
            RtpPortRangeStart is < 1024 or > 65534 || RtpPortRangeEnd < RtpPortRangeStart || RtpPortRangeEnd > 65534 ||
            RtpPortRangeStart % 2 != 0 || RtpPortRangeEnd % 2 != 0 || MaxConcurrentCalls is < 1 or > 32)
            throw new ArgumentException("Exact SIP bind IP, port and transport required.");
        if (OutboundProxy != null) {
            if (OutboundProxy.Length > 256 || OutboundProxy.Any(char.IsControl)) throw new ArgumentException("Invalid outbound proxy.");
            var proxy = SIPEndPoint.ParseSIPEndPoint(OutboundProxy);
            if (proxy.Protocol.ToString() != Transport || proxy.Port is < 1 or > 65535) throw new ArgumentException("Proxy transport must match binding.");
        }
        return new IPEndPoint(address, Port);
    }
}
public sealed record SipIncoming(string SipCallId, string FromUri, string PeerAddress, int PeerPort, string Transport, string[] AssertedNumbers = null, bool RegistrationPeer = false);
public sealed record SipCallObservation(string Id, string Direction, string State, string SipCallId, string Error, SipIncoming Incoming,
    CallFeatureObservation Features = null);
public sealed class SipCallBusyException() : InvalidOperationException("Previous call must be released before another admission.");

// Ephemeral SIP resources only. The service persists admission and original intent before Dial,
// Answer or Hangup. Lost command results cannot be replayed by creating another host/call object.
public sealed class SipTransportHost : IAsyncDisposable {
    private readonly object sync = new();
    private readonly SIPTransport transport;
    private readonly Func<SipIncoming, bool> admit;
    private readonly CodecSettings codecs;
    private readonly Func<string, IPcmAudioPort> audio;
    private readonly IPAddress mediaAddress;
    private readonly int incomingRingSeconds;
    private readonly bool delayedOffer;
    private readonly SIPSorcery.Sys.PortRange rtpPorts;
    private readonly DtmfAccessFailureLimiter accessLimiter = new();
    private readonly SIPEndPoint outboundProxy;
    private readonly SIPProtocolsEnum protocol;
    private readonly Dictionary<string, SipCall> calls = new();
    private readonly int maxConcurrentCalls;
    private SipRegistration registration;
    private bool closed;
    private bool reconnecting;
    public IPEndPoint LocalEndpoint { get; }
    public SipCall CurrentCall { get { lock (sync) return calls.Values.FirstOrDefault(); } }
    public SipCall[] Calls { get { lock (sync) return calls.Values.ToArray(); } }
    public SipCall FindCall(string id) { lock (sync) return calls.GetValueOrDefault(id); }
    public event Action<SipCall> Incoming;

    public SipTransportHost(SipBinding binding, CodecSettings codecs, Func<string, IPcmAudioPort> audio, Func<SipIncoming, bool> admit) {
        var endpoint = binding.Endpoint(); codecs.Validate(); ArgumentNullException.ThrowIfNull(audio); ArgumentNullException.ThrowIfNull(admit);
        this.codecs = codecs; this.audio = audio; this.admit = admit; mediaAddress = endpoint.Address;
        incomingRingSeconds = binding.IncomingRingSeconds;
        maxConcurrentCalls = binding.MaxConcurrentCalls;
        delayedOffer = binding.OutgoingOfferMode == "delayed" || binding.OutgoingOfferMode == "auto" && codecs.Preferences[0] == "EVS";
        rtpPorts = new SIPSorcery.Sys.PortRange(binding.RtpPortRangeStart, binding.RtpPortRangeEnd);
        outboundProxy = binding.OutboundProxy == null ? null : SIPEndPoint.ParseSIPEndPoint(binding.OutboundProxy);
        protocol = Enum.Parse<SIPProtocolsEnum>(binding.Transport);
        SIPChannel channel = binding.Transport switch {
            "udp" => new SIPUDPChannel(endpoint), "tcp" => new SIPTCPChannel(endpoint), "tls" => new SIPTLSChannel(endpoint),
            _ => throw new ArgumentException("Unsupported SIP transport.")
        };
        transport = new SIPTransport();
        try { transport.AddSIPChannel(channel); LocalEndpoint = channel.ListeningEndPoint; transport.SIPTransportRequestReceived += Requested; }
        catch { channel.Close(); transport.Shutdown(); throw; }
    }
    public SipRegistration StartRegistration(SipRegistrationSettings settings, string protectedPassword) {
        lock (sync) {
            ObjectDisposedException.ThrowIf(closed, this);
            if (registration != null) throw new InvalidOperationException("This host already owns its registration.");
            registration = new SipRegistration(transport, protocol, settings, protectedPassword, outboundProxy: outboundProxy);
            return registration;
        }
    }
    public async Task<SipRegistration> ReconnectRegistrationAsync(SipRegistrationSettings settings, string password) {
        SipRegistration previous;
        lock (sync) {
            if (closed || reconnecting || calls.Count > 0) throw new NativeRpcException("operation_conflict");
            reconnecting = true; previous = registration;
        }
        try {
            if (previous != null) await previous.DisposeAsync();
            lock (sync) { registration = null; return StartRegistration(settings, password); }
        } finally { lock (sync) reconnecting = false; }
    }
    public SipCall PrepareOutgoing(string id) {
        if (!Guid.TryParseExact(id, "D", out _)) throw new ArgumentException("Original call UUID required.");
        lock (sync) {
            ObjectDisposedException.ThrowIf(closed, this);
            if (reconnecting || calls.Count >= maxConcurrentCalls || calls.ContainsKey(id)) throw new SipCallBusyException();
            var call = new SipCall(id, transport, new SipMediaSession(codecs, audio(id), mediaAddress, id, rtpPorts), null, null, incomingRingSeconds, delayedOffer, accessLimiter: accessLimiter, outboundProxy: outboundProxy);
            calls.Add(id, call);
            return call;
        }
    }
    private async Task Requested(SIPEndPoint local, SIPEndPoint remote, SIPRequest request) {
        // The original call's SIP controller owns in-dialog transactions, CANCEL, ACK and BYE.
        if (request.Header.From?.FromTag != null && request.Header.To?.ToTag != null) {
            if (request.Method == SIPMethodsEnum.INFO && Calls.Any(call => call.HandleInfo(request)))
                await transport.SendResponseAsync(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.Ok, null));
            return;
        }
        if (request.Method != SIPMethodsEnum.INVITE) {
            if (request.Method == SIPMethodsEnum.OPTIONS) await transport.SendResponseAsync(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.Ok, null));
            else if (request.Method is SIPMethodsEnum.NOTIFY or SIPMethodsEnum.MESSAGE)
                await transport.SendResponseAsync(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.MethodNotAllowed, null));
            return;
        }
        string sipId = request.Header.CallId;
        SipIncoming context = null;
        bool authorized = false;
        try {
            string from = request.Header.From?.FromURI?.ToString();
            if (!string.IsNullOrWhiteSpace(sipId) && sipId.Length <= 1024 && !sipId.Any(char.IsControl) &&
                !string.IsNullOrWhiteSpace(from) && from.Length <= 2048 && !from.Any(char.IsControl) && remote.Port is > 0 and <= 65535) {
                var asserted = (request.Header.PassertedIdentity ?? []).Take(16)
                    .SelectMany(identity => System.Text.RegularExpressions.Regex.Matches(identity.ToString(), @"(?<!\d)\+[1-9]\d{6,14}(?!\d)")
                        .Select(match => match.Value)).Distinct(StringComparer.Ordinal).Take(16).ToArray();
                context = new SipIncoming(sipId, from, remote.Address.ToString(), remote.Port, remote.Protocol.ToString(), asserted,
                    registration?.MatchesPeer(remote) == true);
                authorized = admit(context);
            }
        } catch { /* Admission failure refuses the invitation. */ }
        SipCall created = null; bool duplicate = false;
        lock (sync) {
            duplicate = sipId != null && calls.Values.Any(call => call.Observation.SipCallId == sipId);
            if (!duplicate && !closed && !reconnecting && authorized && calls.Count < maxConcurrentCalls) {
                try {
                    string id = Guid.NewGuid().ToString();
                    created = new SipCall(id, transport, new SipMediaSession(codecs, audio(id), mediaAddress, id, rtpPorts), request, context, incomingRingSeconds, offerEnded: ExpiredOffer, accessLimiter: accessLimiter, outboundProxy: outboundProxy);
                    calls.Add(id, created);
                }
                catch { authorized = false; }
            }
        }
        if (duplicate) return; // Existing SIP transaction owns retransmission of this invitation.
        if (created == null) {
            await transport.SendResponseAsync(SIPResponse.GetResponse(request,
                authorized ? SIPResponseStatusCodesEnum.BusyHere : SIPResponseStatusCodesEnum.Forbidden, null));
            return;
        }
        // Admission retains the call in this host. Subscribers are notifications, not a second
        // owner whose delayed handoff can lose or reject an already admitted call.
        try { Incoming?.Invoke(created); } catch { created.Hangup(); }
    }
    public async Task ReleaseAsync(SipCall expected) {
        string id = expected.Observation.Id;
        lock (sync) if (!ReferenceEquals(calls.GetValueOrDefault(id), expected)) throw new InvalidOperationException("Original call owner required.");
        await expected.DisposeAsync();
        lock (sync) if (ReferenceEquals(calls.GetValueOrDefault(id), expected)) calls.Remove(id);
    }
    private void ExpiredOffer(SipCall original) { _ = ReleaseExpiredOfferAsync(original); }
    private async Task ReleaseExpiredOfferAsync(SipCall original) {
        await Task.Yield(); // Never acquire the host lock inside a SIP callback's call lock.
        lock (sync) if (!ReferenceEquals(calls.GetValueOrDefault(original.Observation.Id), original) || !original.TryRetireOffer()) return;
        try { await ReleaseAsync(original); }
        catch { /* Keep the exact owner on failed cleanup; no replacement may bypass disposal. */ }
    }
    public async ValueTask DisposeAsync() {
        SipCall[] current;
        lock (sync) { if (closed) return; closed = true; current = calls.Values.ToArray(); }
        transport.SIPTransportRequestReceived -= Requested;
        try {
            await Task.WhenAll(current.Select(call => call.DisposeAsync().AsTask()));
        } finally {
            try { if (registration != null) await registration.DisposeAsync(); } finally { transport.Shutdown(); }
        }
    }
}

public sealed class SipCall : IAsyncDisposable {
    private const string TelekomAnonymousAuthUsername = "anonymous@t-online.de";
    private const string TelekomAccessLineDigestPlaceholder = "ivy-access-line-auth";
    private readonly object sync = new();
    private readonly SIPUserAgent agent;
    private readonly LateOfferSipCall delayed;
    private readonly SIPServerUserAgent incoming;
    private readonly string id, direction;
    private readonly SipIncoming incomingContext;
    private string state, sipCallId, error;
    private bool attempted, ending, outcomeUnknown, claimed, retiringOffer;
    private readonly Action<SipCall> offerEnded;
    private Timer ringDeadline;
    private Task disposal;
    private CallDesktopSession desktop;
    public SipMediaSession Media { get; }
    public PhoneCallFeatures Features { get; }
    public SipSignallingDiagnostics Signalling { get; }
    private readonly SipInfoRequestTracker infoRequests = new();
    private bool windowsConnected;
    private Timer screeningDeadline;
    private ScreeningSettings screeningSettings;
    public object PrepareScreening(ScreeningSettings settings) {
        lock (sync) {
            if (attempted || ending || direction != "outgoing" || screeningSettings != null) throw new InvalidOperationException("Prepare original outgoing screening before dial.");
            Media.PrepareScreening(settings, () => Features.Observation.Screening == "waiting"); screeningSettings = settings;
            return new { prepared = true };
        }
    }
    public async Task<object> BridgeScreeningAsync(AudioSettings settings) {
        lock (sync) if (Features.Observation.Screening != "accepted") throw new InvalidOperationException("Screening has not accepted.");
        var result = await ConnectWindowsAsync(settings);
        lock (sync) { Features.BridgeScreening(); Media.BridgeScreening(); screeningDeadline?.Dispose(); }
        return result;
    }
    public async Task<object> ConnectWindowsAsync(AudioSettings settings) {
        lock (sync) {
            if (ending || state != "connected" || !Features.Authenticated || windowsConnected || desktop != null)
                throw new InvalidOperationException("Original authenticated connected Windows call required.");
        }
        await Media.PrepareAudioAsync(settings, null);
        lock (sync) {
            if (ending) throw new InvalidOperationException("Call ended during audio preparation.");
            Media.CallAudio.BindAuthority(settings.CaptureEndpointId);
            windowsConnected = true; Media.CallAudio.GrantAuthority();
            return Media.AudioStatus;
        }
    }
    public Task<AudioAttachmentStatus> RebindVoiceAudioAsync(DesktopIdentity originalDesktop) {
        CallDesktopSession selected;
        lock (sync) {
            if (ending || outcomeUnknown || state != "connected" || Media.IsClosed || Media.Failed ||
                originalDesktop == null || desktop == null || Media.CallAudio == null)
                throw new NativeRpcException("runtime_not_ready");
            selected = desktop;
        }
        bool SameVoice() {
            lock (sync) if (ending || outcomeUnknown || state != "connected" || desktop != selected || Media.IsClosed || Media.Failed) return false;
            return selected.HasOriginalVoiceCapture(originalDesktop);
        }
        return Media.CallAudio.RebindAsync(originalDesktop, SameVoice);
    }
    public async Task<object> UpgradeCodecAsync() {
        lock (sync) {
            if (ending || state != "connected" || !Features.Authenticated) throw new InvalidOperationException("Authenticated connected call required.");
            if (Features.Observation.DisableCodecUpgrade || Features.Observation.Access == "trusted") return new { state = "skipped", codec = Media.SendCodec, payload = Media.SendPayload };
        }
        var negotiated = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        void Ready(List<SIPSorceryMedia.Abstractions.AudioFormat> _) => negotiated.TrySetResult();
        Media.OnAudioFormatsNegotiated += Ready;
        try {
            Media.ChallengeCodecs(false); agent.TakeOffHold();
            await negotiated.Task.WaitAsync(TimeSpan.FromSeconds(5));
            long before = Media.Transmission.Packets;
            var deadline = Environment.TickCount64 + 1000;
            while (Media.Transmission.Packets <= before && Environment.TickCount64 < deadline && Observation.State == "connected") await Task.Delay(20);
            var sent = Media.Transmission;
            return new { state = sent.Packets > before && string.Equals(sent.Codec, Media.PreferredCodec, StringComparison.OrdinalIgnoreCase) ? "upgraded" : "fallback",
                codec = sent.Codec, payload = sent.Payload };
        } catch (TimeoutException) { return new { state = "fallback", codec = Media.SendCodec, payload = Media.SendPayload }; }
        finally { Media.OnAudioFormatsNegotiated -= Ready; }
    }
    public SipCallObservation Observation { get { lock (sync) return new(id, direction, state, sipCallId, error, incomingContext, Features?.Observation); } }
    internal bool HandleInfo(SIPRequest request) {
        var dialogue = agent?.Dialogue ?? delayed?.Dialogue;
        if (dialogue == null || request.Header.CallId != sipCallId || request.Header.From.FromTag != dialogue.RemoteTag ||
            request.Header.To.ToTag != dialogue.LocalTag || request.Body?.Length > 1024 ||
            !SipInfoDtmfParser.TryParse(request.Header.ContentType, request.Body, out char digit)) return false;
        if (infoRequests.IsNewRequest(request.Header.CSeq)) Features.Digit(digit, DtmfInputSource.SipInfo);
        return true;
    }
    public object ConfigureFeatures(CallAccessSettings challenge) {
        lock (sync) {
            if (attempted || ending) throw new InvalidOperationException("Configure features before connecting.");
            Features.Configure(challenge, direction == "incoming");
            if (challenge != null) Media.ChallengeCodecs(true);
            return Features.Observation;
        }
    }
    public SipCallObservation ClaimOffer() {
        lock (sync) {
            if (incoming == null || ending || retiringOffer || disposal != null) throw new NativeRpcException("phone_offer_expired");
            claimed = true; return Observation;
        }
    }
    internal bool TryRetireOffer() {
        lock (sync) {
            if (incoming == null || claimed || attempted || !ending || retiringOffer) return false;
            retiringOffer = true; return true;
        }
    }
    public Task<object> DesktopAsync(PhoneDesktopRuntime runtime, string method, JsonElement args) {
        lock (sync) {
            if (disposal != null) throw new NativeRpcException("runtime_stopping");
            desktop ??= new CallDesktopSession(id, runtime, cleanup => {
                lock (sync) return cleanup || disposal == null && !ending && !outcomeUnknown && !Media.IsClosed;
            }, Media.CallAudio);
            return desktop.Invoke(method, args);
        }
    }
    internal SipCall(string id, SIPTransport transport, SipMediaSession media, SIPRequest request, SipIncoming context, int ringSeconds, bool delayedOffer = false, Action<SipCall> offerEnded = null, DtmfAccessFailureLimiter accessLimiter = null, SIPEndPoint outboundProxy = null) {
        if (!Guid.TryParseExact(id, "D", out _)) throw new ArgumentException("Original call UUID required.");
        this.id = id; Media = media; direction = request == null ? "outgoing" : "incoming"; state = request == null ? "prepared" : "ringing";
        incomingContext = context; this.offerEnded = offerEnded;
        Features = new PhoneCallFeatures(accessLimiter ?? new DtmfAccessFailureLimiter(), () => Hangup());
        Media.EnableDtmf(Features.Digit, () => Features.NeedsInBandDtmf);
        sipCallId = request?.Header.CallId ?? id;
        Signalling = new SipSignallingDiagnostics(transport, sipCallId);
        if (request == null && delayedOffer) delayed = new LateOfferSipCall(transport, Media, Ended, () => { Unknown(); StopOriginal(); }, outboundProxy);
        else {
            agent = new SIPUserAgent(transport, outboundProxy, false);
            agent.OnCallHungup += dialogue => Ended();
            agent.ServerCallCancelled += (server, request) => Ended();
        }
        Media.MediaFailed += () => { lock (sync) if (!outcomeUnknown) error = "media_failed"; Hangup(); };
        try {
            if (request != null) {
                sipCallId = request.Header.CallId; incoming = agent.AcceptCall(request);
                incoming.Progress(SIPResponseStatusCodesEnum.Ringing, "Ringing", null, null, null);
                ringDeadline = new Timer(_ => ExpireRinging(), null, TimeSpan.FromSeconds(ringSeconds), Timeout.InfiniteTimeSpan);
            }
        } catch { Signalling.Dispose(); agent?.Close(); Media.DisposeAsync().AsTask().GetAwaiter().GetResult(); throw; }
    }
    internal static SIPCallDescriptor BuildCallDescriptor(string destination, string username, string password,
        SipRegistrationSettings registration = null) {
        var destinationUri = SIPURI.ParseSIPURI(destination);
        string descriptorUsername = registration?.ContactUser ?? username ?? SIPConstants.SIP_DEFAULT_USERNAME;
        string authUsername = registration?.AuthUsername ?? username;
        string from = registration?.AccountUri ?? (string.IsNullOrWhiteSpace(username) ? SIPConstants.SIP_DEFAULT_FROMURI :
            new SIPURI(username, destinationUri.Host, null, destinationUri.Scheme, destinationUri.Protocol).ToParameterlessString());
        string authPassword = string.IsNullOrEmpty(password) && string.Equals(authUsername, TelekomAnonymousAuthUsername, StringComparison.OrdinalIgnoreCase)
            ? TelekomAccessLineDigestPlaceholder : password;
        return new SIPCallDescriptor(descriptorUsername, authPassword,
            destinationUri.ToString(), from, destinationUri.CanonicalAddress, null, null, authUsername,
            SIPCallDirection.Out, SIPSorcery.Net.SDP.SDP_MIME_CONTENTTYPE, null, null) { CallId = null };
    }
    public async Task<SipCallObservation> DialAsync(string destination, string username, string password, int ringSeconds,
        SipRegistrationSettings registration = null, bool waiting = false) {
        if (destination == null || destination.Length > 2048 || destination.Any(char.IsControl) || ringSeconds is < 1 or > 120)
            throw new ArgumentException("Bounded original SIP destination and ring timeout required.");
        if ((username != null && (username.Length > 256 || username.Any(char.IsControl))) ||
            (password != null && (password.Length > 4096 || password.Any(char.IsControl))))
            throw new ArgumentException("Bounded protected SIP credentials required.");
        var descriptor = BuildCallDescriptor(destination, username, password, registration);
        descriptor.CallId = sipCallId;
        Task<bool> pending;
        lock (sync) {
            if (attempted || ending || incoming != null) throw new InvalidOperationException("Original outgoing call already dispatched or ended.");
            attempted = true; state = "dialing";
            if (waiting) Media.BeginWaiting();
            try { pending = delayed != null ? delayed.StartAsync(descriptor, ringSeconds) : agent.Call(descriptor, Media, ringSeconds); }
            catch { Unknown(); return Observation; }
        }
        return await CompleteAsync(pending, ringSeconds + 15, "sip_dial_not_connected");
    }
    private async Task<SipCallObservation> CompleteAsync(Task<bool> pending, int timeoutSeconds, string notConnected) {
        try {
            bool answered = await pending.WaitAsync(TimeSpan.FromSeconds(timeoutSeconds));
            bool stopOriginal;
            lock (sync) {
                sipCallId ??= agent?.Dialogue?.CallId;
                stopOriginal = ending || outcomeUnknown;
                if (!stopOriginal) {
                    bool connected = answered && (delayed?.IsActive ?? agent.IsCallActive);
                    state = connected ? "connected" : "local_ended"; error = connected ? null : notConnected;
                    if (!connected) ending = true;
                    else {
                        Features.Connected();
                        if (screeningSettings != null) {
                            Features.StartScreening();
                            screeningDeadline = new Timer(_ => Features.ScreeningTimeout(), null,
                                TimeSpan.FromSeconds(screeningSettings.TimeoutSeconds), Timeout.InfiniteTimeSpan);
                        }
                    }
                }
            }
            if (stopOriginal) StopOriginal();
            else if (Observation.State != "connected") Media.Close("not_connected");
        } catch (TimeoutException) {
            Unknown(); StopOriginal();
            // Observe this same attempt if it completes late; never create a replacement call.
            _ = FinishLateAsync(pending);
        } catch { Unknown(); StopOriginal(); }
        return Observation;
    }
    private async Task FinishLateAsync(Task<bool> pending) {
        try { await pending; } catch { /* Unknown remains latched. */ }
        StopOriginal();
    }
    public async Task<SipCallObservation> AnswerAsync(bool waiting = false) {
        Task<bool> pending;
        lock (sync) {
            if (attempted || ending || incoming == null) throw new InvalidOperationException("Original incoming call already answered or ended.");
            attempted = true; ringDeadline?.Dispose();
            if (waiting) Media.BeginWaiting();
            try { pending = agent.Answer(incoming, Media); }
            catch { Unknown(); return Observation; }
        }
        return await CompleteAsync(pending, 15, "sip_answer_not_connected");
    }
    public object EndWaiting() {
        lock (sync) {
            if (state != "connected" || ending || outcomeUnknown) throw new NativeRpcException("runtime_not_ready");
            Media.EndWaiting();
            return new { callId = Observation.Id, ended = true };
        }
    }
    private readonly HashSet<long> feedbackSequences = [];
    public object CommandFeedback(long sequence, bool success) {
        lock (sync) {
            if (state != "connected" || ending || outcomeUnknown ||
                !Features.Observation.Commands.Any(value => value.Sequence == sequence) ||
                !feedbackSequences.Add(sequence)) throw new NativeRpcException("runtime_not_ready");
            Media.CommandFeedback(success);
            return new { callId = Observation.Id, commandSequence = sequence, success };
        }
    }
    private void Ended() {
        lock (sync) { ending = true; ringDeadline?.Dispose(); if (!outcomeUnknown) state = "local_ended"; }
        Media.Close("ended");
        offerEnded?.Invoke(this);
    }
    private void ExpireRinging() {
        lock (sync) {
            if (attempted || ending) return;
            ending = true; state = "local_ended";
        }
        Media.Close("ring_expired"); StopOriginal();
        offerEnded?.Invoke(this);
    }
    private void Unknown() {
        lock (sync) { outcomeUnknown = true; ending = true; state = "outcome_unknown"; error = "sip_operation_outcome_unknown"; ringDeadline?.Dispose(); }
        Media.Close("unknown");
    }
    public SipCallObservation Hangup() {
        lock (sync) { if (ending) return Observation; ending = true; ringDeadline?.Dispose(); }
        Media.Close("hangup");
        StopOriginal();
        lock (sync) if (!outcomeUnknown) state = "local_ended";
        offerEnded?.Invoke(this);
        return Observation;
    }
    private void StopOriginal() {
        try {
            if (delayed != null) delayed.Hangup();
            else if (agent.IsCallActive) agent.Hangup();
            else if (incoming != null && !attempted) incoming.Reject(SIPResponseStatusCodesEnum.Decline, "Declined");
            else agent.Cancel();
        } catch { Unknown(); }
    }
    public ValueTask DisposeAsync() {
        lock (sync) { disposal ??= DisposeCoreAsync(); return new ValueTask(disposal); }
    }
    private async Task DisposeCoreAsync() {
        await Task.Yield(); Hangup();
        Signalling.Dispose();
        if (outcomeUnknown) StopOriginal();
        Task desktopClosing;
        lock (sync) desktopClosing = desktop?.DisposeAsync().AsTask() ?? Task.CompletedTask;
        try {
            if (delayed != null) await delayed.DisposeAsync(); else agent.Close();
        } catch { Unknown(); throw; }
        finally {  screeningDeadline?.Dispose(); Features.Dispose(); await Task.WhenAll(Media.DisposeAsync().AsTask(), desktopClosing); }
    }
}
