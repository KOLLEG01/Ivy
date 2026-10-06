using Ivy.PhoneBridge;

static partial class Program {
    static void Check(bool passed, string message) { if (!passed) throw new Exception(message); }
    static void Reject(Action operation, string message) {
        bool failed = false; try { operation(); } catch (ArgumentException) { failed = true; } catch (InvalidOperationException) { failed = true; }
        Check(failed, message);
    }
    sealed class Clock : TimeProvider {
        public long Ticks;
        public override long TimestampFrequency => 1000;
        public override long GetTimestamp() => Ticks;
    }
    static void Queues() {
        var queue = new AudioQueue(23); var model = new List<float>(); long dropped = 0;
        var random = new Random(17072);
        for (int iteration = 0; iteration < 2000; iteration++) {
            if (random.Next(10) == 0) { queue.Clear(); model.Clear(); }
            else if (random.Next(2) == 0) {
                var input = Enumerable.Range(0, random.Next(50)).Select(i => (float)(iteration * 50 + i)).ToArray();
                model.AddRange(input);
                if (model.Count > 23) { int excess = model.Count - 23; dropped += excess; model.RemoveRange(0, excess); }
                queue.Write(input);
            } else {
                var output = Enumerable.Repeat(-1f, random.Next(50)).ToArray();
                int expected = Math.Min(model.Count, output.Length), actual = queue.Read(output);
                Check(expected == actual && output.Take(actual).SequenceEqual(model.Take(expected)), "PCM remains FIFO across wrap/overflow");
                Check(output.Skip(actual).All(value => value == 0), "underrun emits silence");
                model.RemoveRange(0, expected);
            }
            Check(queue.Count == model.Count && queue.Dropped == dropped, "bounded backlog and accurate drop count");
        }
        queue.Write(new float[] { 1, 2, 3 }); queue.Clear();
        var cleared = new float[23]; Check(queue.Read(cleared) == 0 && cleared.All(value => value == 0), "suspension cannot replay buffered audio");
        Reject(() => new AudioQueue(0), "empty queue rejected");
        Reject(() => new AudioQueue(9601), "unbounded latency rejected");
        var buffered = new AudioQueue(8, 4);
        var block = new float[3];
        buffered.Write(new float[] { 1, 2, 3 });
        Check(buffered.Read(block) == 0 && block.All(x => x == 0) && buffered.Count == 3, "prebuffer waits without consuming audio");
        buffered.Write(new float[] { 4 });
        Check(buffered.Read(block) == 3 && block.SequenceEqual(new float[] { 1, 2, 3 }), "threshold opens FIFO playback");
        Check(buffered.Read(block) == 1 && block.SequenceEqual(new float[] { 4, 0, 0 }), "underrun pads silence");
        buffered.Write(new float[] { 5 });
        Check(buffered.Read(block) == 1 && block[0] == 5, "ordinary underrun does not restart initial prebuffer");
        buffered.Write(new float[] { 6, 7, 8 }); buffered.Clear();
        buffered.Write(new float[] { 9 });
        Check(buffered.Read(block) == 0 && buffered.Count == 1, "session reset discards old audio and rearms prebuffer");
        Reject(() => new AudioQueue(8, 9), "unreachable threshold refused");
    }
    static void Permits() {
        var clock = new Clock { Ticks = 100 }; var permit = new AudioPermit(clock);
        Check(!permit.IsCurrent(), "startup has no audio authority");
        permit.Renew(100, TimeSpan.FromMilliseconds(100));
        clock.Ticks = 199; Check(permit.IsCurrent(), "fresh observation");
        clock.Ticks = 200; Check(!permit.IsCurrent(), "exact deadline closes gate");
        Reject(() => permit.Renew(100, TimeSpan.FromMilliseconds(100)), "delayed observation cannot acquire a fresh lease");
        Reject(() => permit.Renew(201, TimeSpan.FromMilliseconds(100)), "future observation rejected");
        permit.Renew(200, TimeSpan.FromMilliseconds(500));
        clock.Ticks = 210; permit.Renew(210, TimeSpan.FromMilliseconds(500));
        Reject(() => permit.Renew(205, TimeSpan.FromMilliseconds(500)), "out-of-order observation rejected");
        clock.Ticks = 209; Check(!permit.IsCurrent(), "backwards monotonic clock closes gate");
        clock.Ticks = 211; Check(!permit.IsCurrent(), "invalidated permit does not reactivate by time alone");
        permit.Renew(211, TimeSpan.FromMilliseconds(50)); permit.Revoke();
        Check(!permit.IsCurrent(), "hangup revokes immediately");
        Reject(() => permit.Renew(211, TimeSpan.FromMilliseconds(50)), "revoked call cannot regain audio");
        Reject(() => new AudioPermit(clock).Renew(211, TimeSpan.Zero), "zero lease rejected");
        Reject(() => new AudioPermit(clock).Renew(211, TimeSpan.FromMilliseconds(501)), "unbounded lease rejected");
        clock = new Clock(); permit = new AudioPermit(clock); permit.Renew(0, TimeSpan.FromMilliseconds(100));
        long generation = permit.Generation; clock.Ticks = 50; permit.Renew(50, TimeSpan.FromMilliseconds(100));
        Check(permit.Generation == generation, "continuous timely renewal retains audio generation");
        clock.Ticks = 150; permit.Renew(150, TimeSpan.FromMilliseconds(100));
        Check(permit.Generation > generation, "renewal across an unobserved expiry creates a new audio generation");
        clock.Ticks = 250; Check(!permit.IsCurrent(), "renewed permit expires");
        Reject(() => permit.Renew(140, TimeSpan.FromMilliseconds(500)), "expiry cannot erase original observation ordering");
    }
    static void Settings() {
        var valid = new AudioSettings("exact-capture", "exact-render", "system_excluding_runtime", 20, 10, 60);
        valid.Validate(); (valid with { SourceMode = "desktop_process" }).Validate();
        const string audioJson = """
            {"captureEndpointId":"capture","renderEndpointId":"render","sourceMode":"desktop_process",
             "captureBufferMs":20,"renderLatencyMs":20,"queueMs":80}
            """;
        var original = System.Text.Json.JsonSerializer.Deserialize<AudioSettings>(audioJson, NativeRpc.Json) ?? throw new Exception("missing settings");
        Check(original.EnableWasapiLowLatency && original.PlaybackPrebufferMs == 20, "audio defaults use V1 low-latency and initial prebuffer");
        var configured = System.Text.Json.JsonSerializer.Deserialize<AudioSettings>(audioJson.TrimEnd().TrimEnd('}') +
            ",\"playbackPrebufferMs\":20,\"enableWasapiLowLatency\":true}", NativeRpc.Json) ?? throw new Exception("missing settings");
        configured.Validate();
        Check(configured.EnableWasapiLowLatency && configured.PlaybackPrebufferMs == 20, "new IPC audio settings reach the native route");
        foreach (var settings in new[] { valid with { CaptureEndpointId = "" }, valid with { RenderEndpointId = "a\0b" },
            valid with { CaptureEndpointId = "exact-render" }, valid with { SourceMode = "default" },
            valid with { CaptureBufferMs = 100, QueueMs = 20 }, valid with { QueueMs = 201 }, valid with { RenderLatencyMs = 1 },
            valid with { PlaybackPrebufferMs = -1 }, valid with { PlaybackPrebufferMs = 61 } })
            Reject(settings.Validate, "invalid routing/latency rejected");
    }
    static async Task<int> Main(string[] args) {
        if (args.SequenceEqual(new[] { "--micro-usb-only" })) {
            MicroProtocolChecks(); MicroGestures(); await MicroUsbChecks(); await MicroAttachments();
            Console.WriteLine("phone_micro_usb_passed: actual loopback USB/IP import, descriptors, HID/RPC, cancelled reads, generation replacement and malformed transfer refusal; no driver or Desktop");
            return 0;
        }
        if (args.SequenceEqual(new[] { "--micro-protocol-only" })) {
            MicroProtocolChecks();
            Console.WriteLine("phone_micro_protocol_passed: actual HID framing, UTF8, RPC handshake/configuration, microphone events and malformed bounds; no driver or Desktop");
            return 0;
        }
        try {
            if (args.SequenceEqual(new[] { "--screening-loopback-only" })) {
                await ScreeningLoopback();
                Console.WriteLine("phone_screening_loopback_passed: PCMA, G722 and packaged EVS when present; in-band DTMF over actual local RTP; synthetic audio only");
                return 0;
            }
            if (args.SequenceEqual(new[] { "--audio-buffer-only" })) { Queues(); Settings(); Console.WriteLine("phone_audio_buffer_passed: FIFO, overflow, prebuffer threshold, underrun and session reset; no devices"); return 0; }
            if (args.SequenceEqual(new[] { "--screening-peer-fixture" })) { await ScreeningPeerFixture(); return 0; }
            if (args.SequenceEqual(new[] { "--loopback-probe-only" })) { await InstalledLoopbackProbe(); return 0; }
            if (args.SequenceEqual(new[] { "--phone-parity-only" })) { await PhoneParity(); return 0; }
            if (args.Length == 2 && args[0] == "--pipe-capacity-fixture") { await PipeCapacityFixture(args[1]); return 0; }
            if (args.SequenceEqual(new[] { "--pipe-capacity-only" })) {
                await RpcUnits(); await PipeCapacity();
                Console.WriteLine("phone_pipe_capacity_passed: actual child IPC saturation, complete reserved cleanup, original requests and bounded admission; no Desktop, SIP or devices"); return 0;
            }
            if (args.SequenceEqual(new[] { "--evs-codec-only" })) {
                EvsParameters(); EvsManagedCodecs(); Codecs(); await EvsNegotiation();
                Console.WriteLine("phone_evs_managed_passed: ABI, PCM/RTP clocks, modes/bounds/generations, SDP constraints/fallback and actual local initial/delayed SIP/RTP; synthetic audio only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--evs-performance-only" })) { await EvsPerformance(); return 0; }
            if (args.SequenceEqual(new[] { "--delayed-offer-only" })) {
                await InviteExchanges(); await DelayedOfferCalls();
                Console.WriteLine("phone_delayed_offer_passed: actual local authenticated SDP/ACK/RTP, re-INVITEs, remote/local BYE, CANCEL and late200 cleanup; synthetic audio only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--invite-exchange-only" })) {
                await InviteExchanges();
                Console.WriteLine("phone_invite_exchange_passed: local401/407 continuations, original identity, bounded authentication, late-offer ACK ownership and retransmission; no audio or carrier"); return 0;
            }
            if (args.Length == 2 && args[0] == "--voice-lifetime-installed") {
                await InstalledVoiceLifetime(args[1]); return 0;
            }
            if (args.Length == 2 && args[0] == "--voice-capture-observe") {
                ObserveInstalledVoiceCapture(args[1]); return 0;
            }
            if (args.SequenceEqual(new[] { "--voice-lifetime-only" })) {
                await VoiceLifetimeUnits(); await MicroVoiceLifetimeUnits(); await VoiceLifetimeNativeUnits();
                Console.WriteLine("phone_voice_lifetime_passed: own baseline/start/pair/stop, existing Voice refusal, uncertain effects, thread ownership and pending cancellation; fake Desktop/audio only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--voice-authority-only" })) {
                await VoiceAuthorityUnits();
                Console.WriteLine("phone_voice_authority_passed: exact original call/UI/capture, paired freshness, mute/order, expiry and permanent revocation; fake audio only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--voice-refresh-only" })) {
                BoundVoiceControls();
                Console.WriteLine("phone_voice_refresh_passed: original controls, discovery/freshness, mutation, unavailable/owner loss and resource/thread ownership; fake UI only"); return 0;
            }
            if (args.Length == 2 && args[0] == "--voice-refresh-observe") {
                ObserveInstalledVoiceRefresh(args[1]); return 0;
            }
            if (args.SequenceEqual(new[] { "--held-process-fixture" })) {
                Console.WriteLine("held-process-ready"); await Console.In.ReadLineAsync(); return 0;
            }
            if (args.SequenceEqual(new[] { "--capture-correlation-only" })) {
                await CaptureCorrelations();
                Console.WriteLine("phone_capture_correlation_passed: current session/ancestry, competing/shared/racing observations and actual synthetic process handle lifecycle; no devices or Desktop"); return 0;
            }
            if (args.Length == 2 && args[0] == "--desktop-controls-observe") {
                ObserveInstalledControls(args[1]); return 0;
            }
            if (args.Length == 3 && args[0] == "--voice-control-invoke" && args[1] is "start" or "stop") {
                InvokeInstalledVoiceControl(args[2], args[1] == "stop"); return 0;
            }
            if (args.Length == 3 && args[0] == "--voice-hotkey-recover") {
                await RecoverInstalledVoiceHotkey(args[1], args[2]); return 0;
            }
            if (args.Length == 3 && args[0] == "--capture-endpoint-observe") {
                ObserveInstalledCaptureEndpoint(args[1], args[2]); return 0;
            }
            if (args.SequenceEqual(new[] { "--voice-controls-only" })) {
                VoiceControlClassification();
                Console.WriteLine("phone_voice_controls_passed: exact visible group, toggle agreement, transitions and ambiguity; synthetic controls only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--desktop-controls-only" })) {
                await ControlObservations();
                Console.WriteLine("phone_desktop_controls_passed: scoped identity/state, unavailable/limits/cleanup and read-only IPC; fake accessibility only"); return 0;
            }
            if (args.Length == 2 && args[0] == "--native-owner-only") {
                await NativeOwnerRecovery(args[1]); await NativeProductionProcess(args[1]);
                Console.WriteLine("phone_native_owner_passed: real production process collision, abrupt death/replacement, still-live identity and corrupt record refusal; no devices or carrier"); return 0;
            }
            if (args.Length == 2 && args[0] == "--ipc-fixture") {
                await using var operations = new NativeSipOperations(_ => new AudioPort());
                await new NativePipeServer(args[1], Console.OpenStandardInput(), Console.OpenStandardOutput(), operations.InvokeAsync,
                    () => operations.DisposeAsync().AsTask()).RunAsync();
                return 0;
            }
            if (args.SequenceEqual(new[] { "--pipe-only" })) {
                await PipeLoopback(); Console.WriteLine("phone_pipe_loopback_passed: owned child stdio/heartbeat/EOF"); return 0;
            }
            if (args.Length == 2 && args[0] == "--native-integration-only") {
                await NativeAudioCommands(); await NativeProductionProcess(args[1]);
                Console.WriteLine("phone_native_composition_passed: actual production entrypoint/closed call audio/heartbeat/EOF, original device command receipts and late cleanup; fake devices only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--incoming-identity-only" })) {
                await NativeIncomingIdentity(); await NativeExpiredOffer();
                Console.WriteLine("phone_incoming_identity_passed: actual local INVITE caller/transport identity through native status, hangup and release; no answer or devices"); return 0;
            }
            if (args.SequenceEqual(new[] { "--desktop-commands-only" })) {
                await DesktopCommands(); await NativeAudioCommands();
                Console.WriteLine("phone_desktop_commands_passed: original call/activation/chords/focus/unknown receipts and release waits, heartbeat/hangup while pending; fake Desktop and devices only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--call-owner-only" })) {
                await NativeCallHandoff(); await NativeIncomingIdentity(); await NativeExpiredOffer(); await DesktopCommands(); await NativeAudioCommands();
                Console.WriteLine("phone_call_owner_passed: actual incoming handoff during original release and affected Desktop/audio cleanup; fake devices/input only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--capture-commands-only" })) {
                await CaptureCommands();
                Console.WriteLine("phone_capture_commands_passed: original endpoint/process read-only IPC, unavailable/shared/foreign and bounded input; fake observers only"); return 0;
            }
            if (args.SequenceEqual(new[] { "--registration-only" })) {
                RegistrationExpiry(); await RegistrationLoopback(); await RegistrationLoopback(negotiatedLifetime: true);
                await RegistrationRejectThenRecover();
                Console.WriteLine("phone_registration_passed: local REGISTER, contact expiry precedence and confirmed unregister; no carrier or devices"); return 0;
            }
            if (args.SequenceEqual(new[] { "--outgoing-identity-only" })) {
                OutgoingRegistrationIdentity();
                Console.WriteLine("phone_outgoing_identity_passed: registered caller identity and digest credentials remain distinct; no carrier or devices"); return 0;
            }
            if (args.SequenceEqual(new[] { "--media-only" })) {
                Codecs(); RtpQueues(); await RtpLoopback(); await SipLoopback(includeRegistration: false);
                Console.WriteLine("phone_media_focused_passed: codec/generation, bounded RTP units and actual reordered/lost UDP audio, SIP/RTP lifecycle; no hardware/carrier"); return 0;
            }
            if (args.SequenceEqual(new[] { "--call-audio-only" })) {
                await CallAudioUnits(); await CallAudioSipLoopback();
                Console.WriteLine("phone_call_audio_focused_passed: pending preparation/cancellation/late cleanup, original permit and resource ownership, local SIP/RTP hangup/disposal; no hardware"); return 0;
            }
            if (args.SequenceEqual(new[] { "--call-audio-unit-only" })) {
                Permits(); await CallAudioUnits();
                Console.WriteLine("phone_call_audio_unit_focused_passed: permit expiry/generation/order and affected call audio ownership units; no SIP or hardware"); return 0;
            }
            if (args.SequenceEqual(new[] { "--voice-control-only" })) {
                await VoiceControlUnits();
                Console.WriteLine("phone_voice_control_passed: Micro and explicit fallback inputs bind and release one call without Accessibility control"); return 0;
            }
            if (args.Length != 0 && !(args.Length == 3 && args[0] == "--loopback-sip" && args[1] == "--production-executable" &&
                Path.IsPathFullyQualified(args[2]))) throw new ArgumentException("Unknown or incomplete native test mode; no tests executed.");
            Queues(); Permits(); Settings(); RegistrationExpiry();
            Console.WriteLine("phone_audio_unit_passed: bounded PCM/overflow/silence, monotonic permit expiry/revoke/order, exact routing settings; no devices or SIP");
            await CallAudioUnits();
            Console.WriteLine("phone_call_audio_unit_passed: one original route preparation, permit gating, hangup/cancellation and late/failing disposal");
            await VoiceAuthorityUnits();
            Console.WriteLine("phone_voice_authority_passed: original call/UI/capture authority, freshness and revocation; fake audio only");
            await VoiceLifetimeUnits(); await MicroVoiceLifetimeUnits(); await VoiceLifetimeNativeUnits();
            Console.WriteLine("phone_voice_lifetime_passed: original Voice lifetime, unknown cleanup fencing and pending cancellation; fake Desktop/audio only");
            Codecs();
            Console.WriteLine("phone_codec_unit_passed: four codec durations/clocks, signal, silence on revocation, generation reset, bounds; no devices or SIP");
            EvsParameters(); EvsManagedCodecs(); await EvsNegotiation();
            Console.WriteLine("phone_evs_managed_passed: ABI, generations, SDP and actual initial/delayed SIP/RTP; synthetic audio only");
            RtpQueues();
            Console.WriteLine("phone_rtp_unit_passed: bounded monotonic ordering/loss, wrap, duplicate/overflow/source and permission isolation");
            await RpcUnits();
            Console.WriteLine("phone_rpc_unit_passed: exact epoch/operation receipts, concurrent replay, conflicts, capacity, bounded strict frames, redacted unknown results");
            await DesktopCommands();
            Console.WriteLine("phone_desktop_commands_passed: original Desktop command receipts and call-bound cleanup; fake Desktop/input only");
            await CaptureCommands();
            Console.WriteLine("phone_capture_commands_passed: original endpoint/process read-only commands; fake observers only");
            await CaptureCorrelations();
            Console.WriteLine("phone_capture_correlation_passed: current session/ancestry and actual synthetic process handle lifecycle; no devices or Desktop");
            await ControlObservations();
            Console.WriteLine("phone_desktop_controls_passed: scoped read-only accessibility observations; fake providers only");
            VoiceControlClassification();
            Console.WriteLine("phone_voice_controls_passed: exact visible group, toggle agreement, transitions and ambiguity; synthetic controls only");
            MicroProtocolChecks();
            MicroGestures();
            BoundVoiceControls();
            Console.WriteLine("phone_voice_refresh_passed: original controls, freshness and resource/thread ownership; fake UI only");
            if (args.Contains("--loopback-sip")) {
                await MicroUsbChecks();
                await MicroAttachments();
                await InviteExchanges();
                Console.WriteLine("phone_invite_exchange_passed: local401/407 and late-offer ACK ownership/retransmission");
                await DelayedOfferCalls();
                Console.WriteLine("phone_delayed_offer_passed: local authenticated SDP/ACK/RTP and original cancellation/dialog cleanup");
                await NativeCallHandoff();
                Console.WriteLine("phone_call_owner_passed: original host owns current incoming call across release/command concurrency");
                await NativeIncomingIdentity(); await NativeExpiredOffer();
                Console.WriteLine("phone_incoming_identity_passed: original incoming caller context retained through native control lifecycle");
                int production = Array.IndexOf(args, "--production-executable");
                if (production < 0 || production + 1 >= args.Length) throw new ArgumentException("Full regression requires the actual production native DLL.");
                await NativeOwnerRecovery(args[production + 1]);
                Console.WriteLine("phone_native_owner_passed: real native lifetime lease and predecessor identity");
                await NativeAudioCommands(); await NativeProductionProcess(args[production + 1]);
                Console.WriteLine("phone_native_composition_passed: original device command ownership and actual production entrypoint/heartbeat/EOF");
                await CallAudioSipLoopback();
                Console.WriteLine("phone_call_audio_loopback_passed: actual local SIP/RTP call-bound audio factory and original hangup/disposal");
                await RtpLoopback();
                Console.WriteLine("phone_rtp_loopback_passed: actual UDP wrap/reorder/duplicate/loss timer, PCM order and audio-generation isolation; synthetic PCM only");
                await SipLoopback();
                await PipeLoopback();
                Console.WriteLine("phone_pipe_loopback_passed: child stdio SIP control, status/hangup during dial, heartbeat loss and EOF close owned runtime; synthetic PCM only");
                Console.WriteLine("phone_sip_loopback_passed: local registration/removal, SIP accept/bidirectional RTP/BYE, admission refusal, busy, ring expiry, cancellation; synthetic PCM only");
            }
            return 0;
        } catch (Exception error) { Console.Error.WriteLine(error.ToString()); return 1; }
    }
}
