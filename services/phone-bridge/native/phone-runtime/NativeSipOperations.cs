using System.Net;
using System.Globalization;
using System.Text.Json;

namespace Ivy.PhoneBridge;

public sealed record NativeSipConfiguration(SipBinding Binding, CodecSettings Codecs, string[] IncomingPeers,
    SipRegistrationSettings Registration, string Password, MicroSettings Micro = null);
public sealed record NativeCallIdentity(string CallId);
public sealed record NativeAnswer(string CallId, bool Waiting);
public sealed record NativeCommandFeedback(string CallId, long CommandSequence, bool Success);
public sealed record NativeDesktopIdentity(int Pid, string StartTimeUtcTicks, string ImagePath, string AppUserModelId) {
    public DesktopIdentity ToIdentity() {
        if (StartTimeUtcTicks == null || StartTimeUtcTicks.Length is < 1 or > 19 || StartTimeUtcTicks[0] == '0' ||
            StartTimeUtcTicks.Any(c => c is < '0' or > '9') ||
            !long.TryParse(StartTimeUtcTicks, NumberStyles.None, CultureInfo.InvariantCulture, out long ticks) ||
            ImagePath == null || ImagePath.Length > 32767) throw new ArgumentException("Exact Desktop identity required.");
        return new DesktopIdentity(Pid, ticks, ImagePath, AppUserModelId);
    }
}
public sealed record NativePrepareAudio(string CallId, AudioSettings Settings, NativeDesktopIdentity Desktop);
public sealed record NativeRebindAudio(string CallId, NativeDesktopIdentity Desktop, string ThreadId);
public sealed record NativeDial(string CallId, string Destination, string Username, string Password, int RingSeconds, bool Waiting);

// SIP command ownership shared by the real pipe executable and isolated synthetic-PCM fixtures.
// Audio ports start closed; the separate Desktop/media owner must grant their live audio authority.
public sealed class NativeSipOperations : IAsyncDisposable {
    private readonly object sync = new();
    private readonly Func<string, IPcmAudioPort> audio;
    private readonly PhoneDesktopRuntime desktopRuntime;
    private SipTransportHost host;
    private SipRegistration registration;
    private bool closed, configured;
    private Task disposal;
    private SipRegistrationSettings registrationSettings;
    private string registrationPassword;
    private Task<PhoneCodecTestResult> codecTest;
    private readonly CancellationTokenSource probeStop = new();
    private Task<PhoneLoopbackProbeResult> loopbackProbe;
    public int ActiveCalls { get { lock (sync) return host?.Calls.Length ?? 0; } }
    public NativeSipOperations(Func<string, IPcmAudioPort> audio, PhoneDesktopRuntime desktopRuntime = null) {
        ArgumentNullException.ThrowIfNull(audio); this.audio = audio; this.desktopRuntime = desktopRuntime ?? new PhoneDesktopRuntime();
    }
    internal static T Read<T>(JsonElement args, params string[] required) {
        if (!args.EnumerateObject().Select(p => p.Name).ToHashSet(StringComparer.Ordinal).SetEquals(required)) throw new ArgumentException("Exact command fields required.");
        return args.Deserialize<T>(NativeRpc.Json);
    }
    private SipCall Original(JsonElement args) {
        var identity = Read<NativeCallIdentity>(args, "callId");
        var call = host?.FindCall(identity.CallId);
        if (call == null || call.Observation.Id != identity.CallId) throw new NativeRpcException("runtime_not_ready");
        return call;
    }
    public async Task<object> InvokeAsync(string method, JsonElement args) {
        if (method == "audio.probe") {
            Read<JsonElement>(args);
            Task<PhoneLoopbackProbeResult> probe;
            bool Idle() { lock (sync) return !closed && (host?.Calls.Length ?? 0) == 0; }
            lock (sync) {
                if (!Idle()) throw new NativeRpcException("runtime_not_ready");
                if (loopbackProbe == null || loopbackProbe.IsCompleted) loopbackProbe = PhoneLoopbackProbe.RunAsync(Idle, probeStop.Token);
                probe = loopbackProbe;
            }
            return await probe;
        }
        if (method == "codec.test") {
            Read<JsonElement>(args);
            Task<PhoneCodecTestResult> diagnosticWork;
            lock (sync) {
                if (closed) throw new NativeRpcException("runtime_stopping");
                if ((host?.Calls.Length ?? 0) != 0) throw new NativeRpcException("runtime_not_ready");
                // One bounded run per immutable native process/artifact. Concurrent readers share it.
                diagnosticWork = codecTest ??= Task.Run(PhoneCodecDiagnostics.Run);
            }
            return await diagnosticWork;
        }
        if (method == "inventory") {
            Read<JsonElement>(args);
            lock (sync) if (closed) throw new NativeRpcException("runtime_stopping");
            return await Task.Run(PhoneInventory.Read);
        }
        if (method == "desktop.launch") {
            var application = Read<NativeDesktopApplication>(args, "appUserModelId", "startIfMissing");
            lock (sync) if (closed) throw new NativeRpcException("runtime_stopping");
            return await Task.Run<object>(() => desktopRuntime.Launch(application, () => { lock (sync) return !closed; }));
        }
        if (method is "desktop.observe" or "desktop.capture" or "desktop.captureOwner" or "desktop.process" or "desktop.controls") {
            lock (sync) if (closed) throw new NativeRpcException("runtime_stopping");
            if (method == "desktop.captureOwner") {
                Read<JsonElement>(args, "desktop", "endpointId");
                var desktop = Read<NativeDesktopIdentity>(args.GetProperty("desktop"), "pid", "startTimeUtcTicks", "imagePath", "appUserModelId").ToIdentity();
                return await desktopRuntime.CaptureOwnerAsync(desktop, args.GetProperty("endpointId").GetString());
            }
            if (method == "desktop.controls") {
                Read<JsonElement>(args, "desktop", "labels");
                var desktop = Read<NativeDesktopIdentity>(args.GetProperty("desktop"), "pid", "startTimeUtcTicks", "imagePath", "appUserModelId").ToIdentity();
                return await desktopRuntime.ControlsAsync(desktop, args.GetProperty("labels").Deserialize<string[]>(NativeRpc.Json));
            }
            if (method == "desktop.capture") {
                Read<JsonElement>(args, "endpointId"); return await desktopRuntime.CaptureAsync(args.GetProperty("endpointId").GetString());
            }
            if (method == "desktop.process") {
                Read<JsonElement>(args, "desktop", "pid");
                var desktop = Read<NativeDesktopIdentity>(args.GetProperty("desktop"), "pid", "startTimeUtcTicks", "imagePath", "appUserModelId").ToIdentity();
                return await desktopRuntime.ProcessAsync(desktop, args.GetProperty("pid").GetInt32());
            }
            Read<JsonElement>(args, "appUserModelId");
            return await desktopRuntime.ObserveAsync(args.GetProperty("appUserModelId").GetString());
        }
        Task<SipCallObservation> pending = null;
        Task<object> desktopWork = null;
        Task preparing = null; SipMediaSession preparingMedia = null;
        SipTransportHost releaseHost = null; SipCall released = null;
        lock (sync) {
            if (closed) throw new NativeRpcException("runtime_stopping");
            var call = args.TryGetProperty("callId", out var selectedCall) ? host?.FindCall(selectedCall.GetString()) : host?.CurrentCall;
            if (method is "status" or "heartbeat") {
                if (args.EnumerateObject().Any(property => property.Name != "callId") || method == "heartbeat" && args.EnumerateObject().Any()) throw new ArgumentException("Exact status arguments required.");
                return new { configured, call = call?.Observation, registration = registration?.Observation, micro = desktopRuntime.MicroStatus,
                    callIds = host?.Calls.Select(value => value.Observation.Id).ToArray() ?? [],
                    endpoint = host?.LocalEndpoint.ToString(), media = call == null ? null : new { closed = call.Media.IsClosed, failed = call.Media.Failed,
                        sentPackets = call.Media.SentPackets, receivedPackets = call.Media.ReceivedPackets, receive = call.Media.ReceiveStatus,
                        audio = call.Media.AudioStatus, diagnostics = call.Media.Diagnostics, signalling = call.Signalling.Observation } };
            }
            if (method == "configure") {
                if (configured) throw new NativeRpcException("operation_conflict");
                var fields = new List<string> { "binding", "codecs", "incomingPeers", "registration", "password" };
                if (args.TryGetProperty("micro", out _)) fields.Add("micro");
                var value = Read<NativeSipConfiguration>(args, fields.ToArray());
                if (value.Binding == null || value.Codecs == null || value.IncomingPeers == null || value.IncomingPeers.Length > 32 ||
                    value.IncomingPeers.Distinct(StringComparer.Ordinal).Count() != value.IncomingPeers.Length) throw new ArgumentException("Exact SIP configuration required.");
                var peers = value.IncomingPeers.Where(peer => peer != "registered").Select(IPAddress.Parse).ToHashSet();
                value.Binding.Endpoint(); value.Codecs.Validate();
                value.Micro?.Validate();
                if (value.Registration != null) {
                    value.Registration.Validate(Enum.Parse<SIPSorcery.SIP.SIPProtocolsEnum>(value.Binding.Transport));
                    if (value.Password == null || value.Password.Length > 4096 || value.Password.Any(char.IsControl)) throw new ArgumentException("Protected SIP password required.");
                } else if (value.Password != null) throw new ArgumentException("Password without registration.");
                configured = true; // Even a partial native start cannot be repeated under another operation.
                host = new SipTransportHost(value.Binding, value.Codecs, audio, peer => peers.Contains(IPAddress.Parse(peer.PeerAddress)) || value.IncomingPeers.Contains("registered") && peer.RegistrationPeer);
                if (value.Micro != null) desktopRuntime.PrepareMicro(value.Micro);
                if (value.Registration != null) {
                    registrationSettings = value.Registration; registrationPassword = value.Password;
                    registration = host.StartRegistration(value.Registration, value.Password);
                }
                return new { configured, endpoint = host.LocalEndpoint.ToString() };
            }
            if (!configured || host == null) throw new NativeRpcException("runtime_not_ready");
            switch (method) {
                case "registration.reconnect":
                    Read<JsonElement>(args);
                    if (registrationSettings == null) throw new NativeRpcException("runtime_not_ready");
                    desktopWork = ReconnectAsync(); break;
                case "call.screening.prepare": {
                    Read<JsonElement>(args, "callId", "settings");
                    if (call == null || call.Observation.Id != args.GetProperty("callId").GetString()) throw new NativeRpcException("runtime_not_ready");
                    return call.PrepareScreening(args.GetProperty("settings").Deserialize<ScreeningSettings>(NativeRpc.Json));
                }
                case "call.screening.bridge": {
                    Read<JsonElement>(args, "callId", "settings");
                    if (call == null || call.Observation.Id != args.GetProperty("callId").GetString()) throw new NativeRpcException("runtime_not_ready");
                    desktopWork = call.BridgeScreeningAsync(args.GetProperty("settings").Deserialize<AudioSettings>(NativeRpc.Json)); break;
                }
                case "call.features": {
                    Read<JsonElement>(args, "callId", "challenge");
                    if (call == null || call.Observation.Id != args.GetProperty("callId").GetString()) throw new NativeRpcException("runtime_not_ready");
                    return call.ConfigureFeatures(args.GetProperty("challenge").Deserialize<CallAccessSettings>(NativeRpc.Json));
                }
                case "call.windows.connect": {
                    Read<JsonElement>(args, "callId", "settings");
                    if (call == null || call.Observation.Id != args.GetProperty("callId").GetString()) throw new NativeRpcException("runtime_not_ready");
                    desktopWork = call.ConnectWindowsAsync(args.GetProperty("settings").Deserialize<AudioSettings>(NativeRpc.Json)); break;
                }
                case "call.codec.upgrade": desktopWork = Original(args).UpgradeCodecAsync(); break;
                case "call.prepare": {
                    var value = Read<NativeCallIdentity>(args, "callId");
                    if (host.FindCall(value.CallId) != null) throw new NativeRpcException("operation_conflict");
                    try { return host.PrepareOutgoing(value.CallId).Observation; }
                    catch (SipCallBusyException) { throw new NativeRpcException("operation_conflict"); }
                }
                case "call.dial": {
                    var value = Read<NativeDial>(args, "callId", "destination", "username", "password", "ringSeconds", "waiting");
                    if (call == null || call.Observation.Id != value.CallId) throw new NativeRpcException("runtime_not_ready");
                    pending = call.DialAsync(value.Destination, value.Username, value.Password, value.RingSeconds, registrationSettings, value.Waiting); break;
                }
                case "call.audio.prepare": {
                    var value = Read<NativePrepareAudio>(args, "callId", "settings", "desktop");
                    if (call == null || call.Observation.Id != value.CallId) throw new NativeRpcException("runtime_not_ready");
                    if (value.Settings == null || value.Desktop == null) throw new ArgumentException("Exact audio preparation required.");
                    preparingMedia = call.Media;
                    preparing = preparingMedia.PrepareAudioAsync(value.Settings, value.Desktop.ToIdentity()); break;
                }
                case "call.audio.rebind": {
                    var value = Read<NativeRebindAudio>(args, "callId", "desktop", "threadId");
                    if (call == null || call.Observation.Id != value.CallId || value.Desktop == null ||
                        !Guid.TryParseExact(value.ThreadId, "D", out _)) throw new NativeRpcException("runtime_not_ready");
                    preparingMedia = call.Media;
                    preparing = call.RebindVoiceAudioAsync(value.Desktop.ToIdentity()); break;
                }
                case "call.claim": {
                    var value = Read<NativeCallIdentity>(args, "callId");
                    if (call == null || call.Observation.Id != value.CallId) throw new NativeRpcException("phone_offer_expired");
                    return call.ClaimOffer();
                }
                case "call.answer": {
                    var value = Read<NativeAnswer>(args, "callId", "waiting");
                    if (call == null || call.Observation.Id != value.CallId) throw new NativeRpcException("runtime_not_ready");
                    pending = call.AnswerAsync(value.Waiting); break;
                }
                case "call.waiting.end": return Original(args).EndWaiting();
                case "call.command.feedback": {
                    var value = Read<NativeCommandFeedback>(args, "callId", "commandSequence", "success");
                    if (call == null || call.Observation.Id != value.CallId) throw new NativeRpcException("runtime_not_ready");
                    return call.CommandFeedback(value.CommandSequence, value.Success);
                }
                case "call.hangup": return Original(args).Hangup();
                case "call.release":
                    released = Original(args); releaseHost = host; break;
                case "call.desktop.launch":
                case "call.desktop.startVoice":
                case "call.desktop.pauseVoice":
                case "call.desktop.resumeVoice":
                case "call.desktop.stopVoice": {
                    string id = args.GetProperty("callId").GetString();
                    if (!Guid.TryParseExact(id, "D", out _)) throw new ArgumentException("Original call UUID required.");
                    if (call == null || call.Observation.Id != id) throw new NativeRpcException("runtime_not_ready");
                    desktopWork = call.DesktopAsync(desktopRuntime, method, args); break;
                }
                default: throw new NativeRpcException("invalid_request");
            }
        }
        if (pending != null) return await pending; // Never hold command ownership while a peer rings.
        if (desktopWork != null) return await desktopWork;
        if (preparing != null) { await preparing; return preparingMedia.AudioStatus; }
        await releaseHost.ReleaseAsync(released);
        return new { callId = released.Observation.Id, released = true };
    }
    private async Task<object> ReconnectAsync() {
        var next = await host.ReconnectRegistrationAsync(registrationSettings, registrationPassword);
        lock (sync) registration = next;
        return next.Observation;
    }
    public ValueTask DisposeAsync() {
        lock (sync) { disposal ??= DisposeCoreAsync(); return new ValueTask(disposal); }
    }
    private async Task DisposeCoreAsync() {
        SipTransportHost current;
        lock (sync) { closed = true; current = host; }
        probeStop.Cancel();
        try { if (current != null) await current.DisposeAsync(); }
        finally {
            try { if (loopbackProbe != null) { try { await loopbackProbe; } catch { } } }
            finally { probeStop.Dispose(); await desktopRuntime.DisposeAsync(); }
        }
    }
}
