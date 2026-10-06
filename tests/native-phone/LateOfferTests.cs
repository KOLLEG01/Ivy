using System.Collections.Concurrent;
using System.Net;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP;
using SIPSorcery.SIP.App;

static partial class Program {
    static async Task DelayedOfferCalls() {
        new SipBinding("127.0.0.1", 0, "udp", OutgoingOfferMode: "auto").Endpoint();
        Reject(() => new SipBinding("127.0.0.1", 0, "udp", OutgoingOfferMode: "unknown").Endpoint(), "unknown offer modes refused");
        var selected = System.Text.Json.JsonSerializer.Deserialize<SipBinding>(
            "{\"address\":\"127.0.0.1\",\"port\":0,\"transport\":\"udp\",\"outgoingOfferMode\":\"delayed\"}", NativeRpc.Json)!;
        Check(selected.OutgoingOfferMode == "delayed", "native configuration preserves explicit delayed offer selection");
        var rejected = SDP.ParseSDPDescription(LateOfferSipCall.RejectedAnswer(InviteOffer));
        Check(rejected.Media.Count == 1 && rejected.Media.All(value => value.Port == 0), "late cancelled offer rejects every media stream");
        Check(!LateOfferSipCall.ValidSdp("text/plain", InviteOffer) && !LateOfferSipCall.ValidSdp(SDP.SDP_MIME_CONTENTTYPE, new string('x', 65537)),
            "media negotiation refuses wrong type or unbounded SDP");
        foreach (bool proxy in new[] { false, true }) await DelayedOfferConnected(proxy);
        await DelayedOfferCancelled(false); await DelayedOfferCancelled(true);
    }

    static async Task DelayedOfferConnected(bool proxy) {
        using var server = new SIPTransport();
        var channel = new SIPUDPChannel(new IPEndPoint(IPAddress.Loopback, 0)); server.AddSIPChannel(channel);
        using var agent = new SIPUserAgent(server, null, false);
        var codec = new CodecSettings(["PCMA"]); var a = new AudioPort(); var b = new AudioPort();
        await using var peerMedia = new SipMediaSession(codec, b, IPAddress.Loopback);
        var peerReady = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        int admitted = 0;
        server.SIPTransportRequestReceived += async (local, remote, request) => {
            if (request.Method != SIPMethodsEnum.INVITE || request.Header.To.ToTag != null) return;
            try {
                Check(string.IsNullOrEmpty(request.Body), "both original/authenticated INVITEs have no SDP");
                if (request.Header.AuthenticationHeaders.Count == 0) {
                    var challenge = SIPResponse.GetResponse(request, proxy ? SIPResponseStatusCodesEnum.ProxyAuthenticationRequired : SIPResponseStatusCodesEnum.Unauthorised, null);
                    challenge.Header.UnknownHeaders.Add((proxy ? "Proxy-Authenticate" : "WWW-Authenticate") +
                        ": Digest realm=\"fixture\", nonce=\"delayed-call-fixture\", algorithm=MD5, qop=\"auth\"");
                    await server.SendResponseAsync(challenge); return;
                }
                Check(Interlocked.Increment(ref admitted) == 1, "single authenticated call admission");
                var incoming = agent.AcceptCall(request);
                peerReady.TrySetResult(await agent.Answer(incoming, peerMedia));
            } catch (Exception error) { peerReady.TrySetException(error); }
        };
        await using var caller = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp", OutgoingOfferMode: "delayed"), codec, _ => a, _ => false);
        var call = caller.PrepareOutgoing(Guid.NewGuid().ToString());
        call.ConfigureFeatures(null);
        var result = await call.DialAsync($"sip:fixture@127.0.0.1:{channel.ListeningEndPoint.Port}", "fixture", "fixture-only", 5)
            .WaitAsync(TimeSpan.FromSeconds(10));
        Check(result.State == "connected" && await peerReady.Task.WaitAsync(TimeSpan.FromSeconds(5)), "both actual peers connect after delayed ACK answer");
        await Until(() => call.Media.ReceivedPackets >= 5 && peerMedia.ReceivedPackets >= 5, "delayed offer carries bidirectional RTP");
        Check(a.Rms > .1 && b.Rms > .1, "negotiated delayed offer carries synthetic audio both ways");
        call.Features.StartScreening();
        var info = agent.Dialogue.GetInDialogRequest(SIPMethodsEnum.INFO);
        info.Header.ContentType = "application/dtmf-relay"; info.Body = "Signal=1\r\nDuration=100";
        var infoResult = new TaskCompletionSource<int>(TaskCreationOptions.RunContinuationsAsynchronously);
        var infoTransaction = new SIPNonInviteTransaction(server, info, null);
        infoTransaction.NonInviteTransactionFinalResponseReceived += (local, remote, original, response) => {
            infoResult.TrySetResult(response.StatusCode); return Task.FromResult(System.Net.Sockets.SocketError.Success);
        };
        infoTransaction.SendRequest();
        Check(await infoResult.Task.WaitAsync(TimeSpan.FromSeconds(5)) == 200 && call.Features.Observation.Screening == "accepted",
            "actual in-dialog SIP INFO reaches delayed-call DTMF consumer");
        if (!proxy) {
            await PeerReinvite(server, agent.Dialogue, peerMedia, false);
            await PeerReinvite(server, agent.Dialogue, peerMedia, true);
            var stale = agent.Dialogue.GetInDialogRequest(SIPMethodsEnum.BYE);
            stale.Header.CSeq -= 2;
            var refused = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            var staleTransaction = new SIPNonInviteTransaction(server, stale, null);
            staleTransaction.NonInviteTransactionFinalResponseReceived += (local, remote, original, response) => {
                refused.TrySetResult(response.Status == SIPResponseStatusCodesEnum.InternalServerError);
                return Task.FromResult(System.Net.Sockets.SocketError.Success);
            };
            staleTransaction.SendRequest();
            Check(await refused.Task.WaitAsync(TimeSpan.FromSeconds(5)) && call.Observation.State == "connected", "stale in-dialog BYE cannot end original call");
            long prior = call.Media.ReceivedPackets;
            await Until(() => call.Media.ReceivedPackets >= prior + 3, "RTP continues after offered and bodyless re-INVITEs");
            var foreign = agent.Dialogue.GetInDialogRequest(SIPMethodsEnum.BYE);
            foreign.Header.From.FromTag = "foreign-dialog-tag";
            await server.SendRequestAsync(foreign);
            await Task.Delay(50);
            Check(call.Observation.State == "connected", "foreign dialog cannot terminate original call");
            agent.Hangup();
            await Until(() => call.Observation.State == "local_ended", "original peer BYE ends delayed call");
        } else {
            call.Hangup();
            await Until(() => !agent.IsCallActive, "original delayed caller BYE ends peer call");
        }
        await caller.ReleaseAsync(call);
        Check(caller.CurrentCall == null && call.Media.IsClosed, "original call releases after proven protocol cleanup");
    }

    static async Task PeerReinvite(SIPTransport transport, SIPDialogue dialogue, SipMediaSession media, bool bodyless) {
        var request = dialogue.GetInDialogRequest(SIPMethodsEnum.INVITE);
        request.Header.Contact = [SIPContactHeader.GetDefaultSIPContactHeader(request.URI.Scheme)];
        if (!bodyless) { request.Body = media.CreateOffer().ToString(); request.Header.ContentType = SDP.SDP_MIME_CONTENTTYPE; }
        var transaction = new UACInviteTransaction(transport, request, null, sendOkAckManually: bodyless);
        var completed = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        transaction.UACInviteTransactionFinalResponseReceived += (local, remote, original, response) => {
            try {
                Check(response.Status == SIPResponseStatusCodesEnum.Ok, "original in-dialog SDP negotiation accepted");
                Check(media.SetRemoteDescription(bodyless ? SdpType.offer : SdpType.answer, SDP.ParseSDPDescription(response.Body)) == SetDescriptionResultEnum.OK,
                    "peer accepts renegotiated SDP");
                if (bodyless) transaction.AckAnswer(response, media.CreateAnswer(null).ToString(), SDP.SDP_MIME_CONTENTTYPE);
                completed.TrySetResult(true);
            } catch (Exception error) { completed.TrySetException(error); }
            return Task.FromResult(System.Net.Sockets.SocketError.Success);
        };
        transaction.SendInviteRequest();
        await completed.Task.WaitAsync(TimeSpan.FromSeconds(5));
        // The peer's ordinary ACK is asynchronous; require renewed actual RTP before another offer.
        long before = media.ReceivedPackets;
        await Until(() => media.ReceivedPackets > before + 2, "original re-INVITE completes before next peer operation");
    }

    static async Task DelayedOfferCancelled(bool answerLate) {
        using var peer = new SIPTransport();
        var channel = new SIPUDPChannel(new IPEndPoint(IPAddress.Loopback, 0)); peer.AddSIPChannel(channel);
        var invited = new TaskCompletionSource<SIPRequest>(TaskCreationOptions.RunContinuationsAsynchronously);
        var cancelled = new TaskCompletionSource<SIPRequest>(TaskCreationOptions.RunContinuationsAsynchronously);
        var bye = new TaskCompletionSource<SIPRequest>(TaskCreationOptions.RunContinuationsAsynchronously);
        var failures = new ConcurrentQueue<Exception>(); var requests = new ConcurrentQueue<SIPRequest>();
        peer.SIPTransportRequestReceived += async (local, remote, request) => {
            try {
                requests.Enqueue(request.Copy());
                if (request.Method == SIPMethodsEnum.INVITE) {
                    await peer.SendResponseAsync(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.Ringing, null));
                    invited.TrySetResult(request.Copy());
                } else if (request.Method == SIPMethodsEnum.CANCEL) {
                    await peer.SendResponseAsync(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.Ok, null));
                    cancelled.TrySetResult(request.Copy());
                } else if (request.Method == SIPMethodsEnum.BYE) {
                    await peer.SendResponseAsync(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.Ok, null));
                    bye.TrySetResult(request.Copy());
                }
            } catch (Exception error) { failures.Enqueue(error); }
        };
        await using var caller = new SipTransportHost(new SipBinding("127.0.0.1", 0, "udp", OutgoingOfferMode: "delayed"),
            new CodecSettings(["PCMA"]), _ => new AudioPort(), _ => false);
        var call = caller.PrepareOutgoing(Guid.NewGuid().ToString());
        var dial = call.DialAsync($"sip:fixture@127.0.0.1:{channel.ListeningEndPoint.Port}", null, null, 5);
        var original = await invited.Task.WaitAsync(TimeSpan.FromSeconds(5));
        call.Hangup();
        var cancel = await cancelled.Task.WaitAsync(TimeSpan.FromSeconds(5));
        Check(cancel.Header.CallId == original.Header.CallId && cancel.Header.CSeq == original.Header.CSeq &&
            cancel.Header.Vias.TopViaHeader.Branch == original.Header.Vias.TopViaHeader.Branch, "CANCEL belongs to exact original INVITE");
        Reject(() => caller.PrepareOutgoing(Guid.NewGuid().ToString()), "original cleanup retains call slot");
        var final = SIPResponse.GetResponse(original, answerLate ? SIPResponseStatusCodesEnum.Ok : SIPResponseStatusCodesEnum.RequestTerminated, null);
        final.Header.To.ToTag = "cancelled-original-peer";
        if (answerLate) {
            final.Header.Contact = [new SIPContactHeader(null, SIPURI.ParseSIPURI($"sip:fixture@127.0.0.1:{channel.ListeningEndPoint.Port}"))];
            final.Header.ContentType = SDP.SDP_MIME_CONTENTTYPE; final.Body = InviteOffer;
        }
        await peer.SendResponseAsync(final);
        Check((await dial.WaitAsync(TimeSpan.FromSeconds(5))).State == "local_ended", "cancelled original attempt cannot become connected");
        if (answerLate) {
            await bye.Task.WaitAsync(TimeSpan.FromSeconds(5));
            var sent = requests.ToArray(); int ackIndex = Array.FindIndex(sent, value => value.Method == SIPMethodsEnum.ACK);
            int byeIndex = Array.FindIndex(sent, value => value.Method == SIPMethodsEnum.BYE);
            Check(ackIndex >= 0 && byeIndex > ackIndex && SDP.ParseSDPDescription(sent[ackIndex].Body).Media.All(value => value.Port == 0),
                "late200 receives rejected-media ACK before original BYE");
            Check(sent[byeIndex].Header.CallId == original.Header.CallId && sent[byeIndex].Header.To.ToTag == "cancelled-original-peer",
                "late cleanup cannot address a replacement dialog");
        }
        await caller.ReleaseAsync(call);
        Check(call.Media.SentPackets == 0 && call.Media.ReceivedPackets == 0 && call.Media.IsClosed && failures.IsEmpty,
            "cancelled late offer never starts RTP and cleanup has no hidden peer failures");
        Check(requests.Where(value => value.Method == SIPMethodsEnum.INVITE).Select(value => value.Header.Vias.TopViaHeader.Branch).Distinct().Count() == 1,
            "cancelled invitation is never replaced");
    }
}
