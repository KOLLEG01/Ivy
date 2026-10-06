using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using Ivy.PhoneBridge;
using SIPSorcery.Net;
using SIPSorcery.SIP;

static partial class Program {
    const string InviteOffer = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
    const string InviteAnswer = "v=0\r\no=- 2 2 IN IP4 127.0.0.1\r\ns=fixture\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 40002 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";

    static async Task InviteExchanges() {
        foreach (bool proxy in new[] { false, true }) await InviteExchangeLoopback(proxy, true);
        await InviteExchangeLoopback(false, false);
    }

    static async Task InviteExchangeLoopback(bool proxyAuthentication, bool lateOffer) {
        using var peer = new SIPTransport(); using var client = new SIPTransport();
        var peerChannel = new SIPUDPChannel(new IPEndPoint(IPAddress.Loopback, 0)); peer.AddSIPChannel(peerChannel);
        var clientChannel = new SIPUDPChannel(new IPEndPoint(IPAddress.Loopback, 0)); client.AddSIPChannel(clientChannel);
        var invites = new ConcurrentQueue<SIPRequest>(); var acks = new ConcurrentQueue<SIPRequest>();
        var completed = new TaskCompletionSource<(UACInviteTransaction, SIPResponse)>(TaskCreationOptions.RunContinuationsAsynchronously);
        var peerFailure = new TaskCompletionSource<Exception>(TaskCreationOptions.RunContinuationsAsynchronously);
        SIPResponse? challengeObserved = null;
        SIPResponse? finalSent = null;
        peer.SIPTransportRequestReceived += async (local, remote, request) => {
            try {
                Check(IPAddress.IsLoopback(remote.Address), "only local SIP peer");
                if (request.Method == SIPMethodsEnum.ACK) { acks.Enqueue(request.Copy()); return; }
                Check(request.Method == SIPMethodsEnum.INVITE, "exchange sends only its two INVITEs and ACKs");
                invites.Enqueue(request.Copy());
                if (request.Header.AuthenticationHeaders.Count == 0) {
                    var challenge = SIPResponse.GetResponse(request, proxyAuthentication ? SIPResponseStatusCodesEnum.ProxyAuthenticationRequired : SIPResponseStatusCodesEnum.Unauthorised, null);
                    challenge.Header.UnknownHeaders.Add((proxyAuthentication ? "Proxy-Authenticate" : "WWW-Authenticate") +
                        ": Digest realm=\"fixture\", nonce=\"original-fixture-nonce\", algorithm=MD5, qop=\"auth\"");
                    await peer.SendResponseAsync(challenge); return;
                }
                var final = SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.Ok, null);
                final.Header.To.ToTag = "original-peer-tag";
                final.Header.Contact = [new SIPContactHeader(null, SIPURI.ParseSIPURI($"sip:peer@127.0.0.1:{peerChannel.ListeningEndPoint.Port}"))];
                final.Header.ContentType = SDP.SDP_MIME_CONTENTTYPE; final.Body = lateOffer ? InviteOffer : InviteAnswer;
                finalSent = final;
                await peer.SendResponseAsync(final);
            } catch (Exception error) { peerFailure.TrySetResult(error); completed.TrySetException(error); }
        };
        var target = SIPURI.ParseSIPURI($"sip:peer@127.0.0.1:{peerChannel.ListeningEndPoint.Port}");
        var invite = new SIPRequest(SIPMethodsEnum.INVITE, target);
        invite.Header = new SIPHeader(new SIPFromHeader(null, SIPURI.ParseSIPURI("sip:fixture@127.0.0.1"), "original-local-tag"),
            new SIPToHeader(null, target, null), 1, Guid.NewGuid().ToString()) {
            CSeqMethod = SIPMethodsEnum.INVITE,
            Contact = [SIPContactHeader.GetDefaultSIPContactHeader(target.Scheme)]
        };
        invite.Header.Vias.PushViaHeader(new SIPViaHeader(clientChannel.ListeningEndPoint, "z9hG4bK" + Guid.NewGuid().ToString("N")));
        if (!lateOffer) { invite.Body = InviteOffer; invite.Header.ContentType = SDP.SDP_MIME_CONTENTTYPE; }
        var exchange = new SipInviteExchange(client, invite);
        var first = exchange.Current;
        Reject(() => new SipInviteExchange(client, new SIPRequest(SIPMethodsEnum.OPTIONS, target)), "non-INVITE rejected before sending");
        var whitespace = invite.Copy(); whitespace.Body = " ";
        Reject(() => new SipInviteExchange(client, whitespace), "whitespace cannot fake a late offer");
        void Subscribe(UACInviteTransaction transaction) {
            transaction.UACInviteTransactionFailed += (_, reason) => completed.TrySetException(new Exception("Local SIP transaction failed: " + reason));
            transaction.UACInviteTransactionFinalResponseReceived += (local, remote, current, response) => {
                try {
                    if (response.StatusCode is 401 or 407) {
                        challengeObserved = response.Copy();
                        var foreign = response.Copy(); foreign.Header.CallId = Guid.NewGuid().ToString();
                        Reject(() => exchange.Authenticate(transaction, foreign, "fixture", "fixture-only"), "foreign call challenge rejected");
                        foreign = response.Copy(); foreign.Header.CSeq++;
                        Reject(() => exchange.Authenticate(transaction, foreign, "fixture", "fixture-only"), "foreign CSeq challenge rejected");
                        Reject(() => exchange.Authenticate(transaction, response, "fixture", ""), "missing protected credential rejected");
                        var authenticated = exchange.Authenticate(transaction, response, "fixture", "fixture-only");
                        Reject(() => exchange.Authenticate(transaction, response, "fixture", "fixture-only"), "old transaction cannot authenticate twice");
                        Subscribe(authenticated); authenticated.SendInviteRequest();
                    } else completed.TrySetResult((transaction, response.Copy()));
                    return Task.FromResult(SocketError.Success);
                } catch (Exception error) { completed.TrySetException(error); return Task.FromResult(SocketError.Fault); }
            };
        }
        Subscribe(first); first.SendInviteRequest();
        var (answered, response) = await completed.Task.WaitAsync(TimeSpan.FromSeconds(5));
        Check(response.Status == SIPResponseStatusCodesEnum.Ok && ReferenceEquals(answered, exchange.Current), "authenticated original transaction receives200");
        var branches = invites.GroupBy(value => value.Header.Vias.TopViaHeader.Branch).ToArray();
        Check(branches.Length == 2, "single authenticated continuation, allowing reliable UDP retransmissions");
        foreach (var branch in branches)
            Check(branch.Select(value => value.ToString()).Distinct(StringComparer.Ordinal).Count() == 1,
                "a retransmitted original INVITE cannot change its content or authentication");
        var sent = branches.Select(value => value.First()).OrderBy(value => value.Header.CSeq).ToArray();
        Check(sent[0].Header.CallId == sent[1].Header.CallId && sent[0].Header.From.FromTag == sent[1].Header.From.FromTag &&
            sent[1].Header.CSeq == sent[0].Header.CSeq + 1 && sent[0].Body == sent[1].Body,
            "authentication preserves original identity/body and advances CSeq");
        var repeatChallenge = challengeObserved!.Copy(); repeatChallenge.Header.CSeq = answered.TransactionRequest.Header.CSeq;
        Reject(() => exchange.Authenticate(answered, repeatChallenge, "fixture", "fixture-only"), "repeat401/407 is bounded, not a third INVITE");
        if (lateOffer) {
            // A200 callback happens after the library's automatic-ACK path. A short local delivery
            // barrier also checks the wire, not just whether this fixture called AckAnswer itself.
            await Task.Delay(75);
            Check(acks.All(value => value.Header.CSeq != response.Header.CSeq), "no bodyless automatic200 ACK after authentication");
            Check(await exchange.Current.AcknowledgeAsync(response, InviteAnswer) == SocketError.Success, "single manual ACK send completes");
            Reject(() => exchange.Current.AcknowledgeAsync(response, InviteAnswer), "owner cannot submit a second negotiated ACK");
        }
        await Until(() => acks.Any(value => value.Header.CSeq == response.Header.CSeq), "original200 ACK arrives locally");
        var ack = acks.Last(value => value.Header.CSeq == response.Header.CSeq);
        Check(ack.Header.CallId == response.Header.CallId && ack.Header.To.ToTag == "original-peer-tag" &&
            (lateOffer ? ack.Body == InviteAnswer : string.IsNullOrEmpty(ack.Body)), "ACK content follows actual initial offer mode");
        int acknowledgements = acks.Count(value => value.Header.CSeq == response.Header.CSeq);
        await peer.SendResponseAsync(finalSent!);
        await Until(() => acks.Count(value => value.Header.CSeq == response.Header.CSeq) > acknowledgements, "retransmitted200 reuses saved ACK");
        Check(acks.Where(value => value.Header.CSeq == response.Header.CSeq).All(value => value.Body == ack.Body), "retransmissions retain same negotiated SDP answer");
        Check(invites.Select(value => value.Header.Vias.TopViaHeader.Branch).Distinct().Count() == 2 && !peerFailure.Task.IsCompleted,
            "no replacement invitation or hidden peer failure");
        Console.WriteLine($"invite_exchange: challenge={(proxyAuthentication ? 407 : 401)} lateOffer={lateOffer} transactions={branches.Length} receivedInvites={invites.Count}");
    }
}
