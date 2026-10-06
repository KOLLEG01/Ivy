using System.Net;
using System.Net.Sockets;
using SIPSorcery.Net;
using SIPSorcery.SIP;
using SIPSorcery.SIP.App;
using SIPSorceryMedia.Abstractions;

namespace Ivy.PhoneBridge;

// SIP protocol resources for one original delayed-offer call. SipCall retains the domain state,
// native command results and Desktop/audio ownership. No reflection or transport trace hooks.
internal sealed class LateOfferSipCall : IAsyncDisposable {
    private readonly object sync = new();
    private readonly SIPTransport transport;
    private readonly SipMediaSession media;
    private readonly SIPEndPoint outboundProxy;
    private readonly Action ended, uncertain;
    private readonly TaskCompletionSource<bool> answer = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly TaskCompletionSource settled = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private SipInviteExchange exchange;
    private SIPCallDescriptor descriptor;
    private SIPDialogue dialogue;
    private UASInviteTransaction reInvite;
    private Timer deadline, reInviteDeadline;
    private bool attempted, sent, ending, active, finalReceived, provisional, cancelSent, byeSent, remoteEnded, failed, closed;
    private Task disposal, finalWork = Task.CompletedTask;
    public bool IsActive { get { lock (sync) return active && !ending && !failed; } }
    internal SIPDialogue Dialogue { get { lock (sync) return dialogue; } }

    public LateOfferSipCall(SIPTransport transport, SipMediaSession media, Action ended, Action uncertain, SIPEndPoint outboundProxy = null) {
        this.transport = transport; this.media = media; this.ended = ended; this.uncertain = uncertain;
        this.outboundProxy = outboundProxy;
        transport.SIPTransportRequestReceived += Requested;
        media.OnTimeout += RtpTimeout;
    }

    public async Task<bool> StartAsync(SIPCallDescriptor original, int ringSeconds) {
        lock (sync) {
            if (attempted || ending || closed) throw new InvalidOperationException("Original delayed INVITE already attempted or stopped.");
            attempted = true; descriptor = original;
            deadline = new Timer(_ => {
                lock (sync) {
                    if (finalReceived || ending || closed) return;
                    ending = true; active = false;
                }
                Hangup(); ended();
            }, null, TimeSpan.FromSeconds(ringSeconds), Timeout.InfiniteTimeSpan);
        }
        try {
            var target = SIPURI.ParseSIPURI(original.Uri);
            var destination = outboundProxy ?? await transport.ResolveSIPUriAsync(target);
            lock (sync) {
                if (ending || closed || destination == null) { answer.TrySetResult(false); settled.TrySetResult(); return false; }
                var request = new SIPRequest(SIPMethodsEnum.INVITE, target) {
                    Header = new SIPHeader(original.GetFromHeader(), SIPToHeader.ParseToHeader(original.To), 1, original.CallId)
                };
                request.Header.From.FromTag = CallProperties.CreateNewTag();
                request.Header.CSeqMethod = SIPMethodsEnum.INVITE;
                request.Header.Contact = [SIPContactHeader.GetDefaultSIPContactHeader(target.Scheme)];
                request.Header.Contact[0].ContactURI.User = original.Username;
                request.Header.UserAgent = "IvyPhone";
                request.Header.Vias.PushViaHeader(new SIPViaHeader(new IPEndPoint(IPAddress.Any, 0), CallProperties.CreateBranchId()));
                // No100rel: a bodyless INVITE's reliable provisional offer would require SDP in PRACK.
                exchange = new SipInviteExchange(transport, request, outboundProxy);
                Subscribe(exchange.Current); sent = true; exchange.Current.SendInviteRequest();
            }
        } catch { Fail(); }
        return await answer.Task;
    }

    private void Subscribe(UACInviteTransaction transaction) {
        transaction.UACInviteTransactionFailed += (current, _) => {
            lock (sync) if (!ReferenceEquals(current, exchange.Current) || closed || settled.Task.IsCompletedSuccessfully) return;
            Fail();
        };
        transaction.UACInviteTransactionInformationResponseReceived += (local, remote, current, response) => {
            lock (sync) {
                if (!ReferenceEquals(current, exchange.Current) || finalReceived || closed) return Task.FromResult(SocketError.Success);
                provisional = true;
            }
            if (response.Header.Require?.Split(',').Any(value => value.Trim() == "100rel") == true) { Hangup(); ended(); }
            else StopIfRequested();
            return Task.FromResult(SocketError.Success);
        };
        transaction.UACInviteTransactionFinalResponseReceived += FinalResponse;
    }

    private Task<SocketError> FinalResponse(SIPEndPoint local, SIPEndPoint remote, SIPTransaction transaction, SIPResponse response) {
        lock (sync) {
            var work = CompleteFinalAsync(transaction, response);
            finalWork = work;
            return work;
        }
    }

    private async Task<SocketError> CompleteFinalAsync(SIPTransaction transaction, SIPResponse response) {
        await Task.Yield(); // Publish ownership before negotiation or an early remote BYE can finish.
        try {
            bool stop;
            lock (sync) {
                if (!ReferenceEquals(transaction, exchange.Current) || closed) return SocketError.Success;
                if (response.StatusCode is 401 or 407 && !ending) {
                    try {
                        var continuation = exchange.Authenticate(exchange.Current, response,
                            string.IsNullOrWhiteSpace(descriptor.AuthUsername) ? descriptor.Username : descriptor.AuthUsername, descriptor.Password);
                        provisional = false; Subscribe(continuation); continuation.SendInviteRequest(); return SocketError.Success;
                    } catch (ArgumentException) { /* Missing credentials: the challenge is a terminal refusal. */ }
                    catch (InvalidOperationException) { /* The single original authentication attempt was refused. */ }
                }
                finalReceived = true; deadline?.Dispose();
                if (response.StatusCode is < 200 or > 299) {
                    ending = true; answer.TrySetResult(false); settled.TrySetResult(); return SocketError.Success;
                }
                dialogue = new SIPDialogue(exchange.Current);
                stop = ending || failed || media.IsClosed;
            }
            string body = null;
            bool accepted = false;
            if (!stop && ValidSdp(response.Header.ContentType, response.Body)) {
                try {
                    if (media.SetRemoteDescription(SdpType.offer, SDP.ParseSDPDescription(response.Body)) == SetDescriptionResultEnum.OK && !media.Failed)
                        body = media.CreateAnswer(null)?.ToString();
                    accepted = !string.IsNullOrWhiteSpace(body);
                } catch { /* A malformed or unusable remote offer must not start media. */ }
            }
            Task<SocketError> acknowledgement;
            lock (sync) {
                stop |= ending || failed || media.IsClosed || !accepted;
                if (stop) { ending = true; body = RejectedAnswer(response.Body); }
                acknowledgement = exchange.Current.AcknowledgeAsync(response, body);
                dialogue.SDP = body;
            }
            if (await acknowledgement != SocketError.Success) throw new InvalidOperationException("Original SIP ACK send was not confirmed.");
            lock (sync) {
                dialogue.DialogueState = SIPDialogueStateEnum.Confirmed;
                stop |= ending || failed || media.IsClosed;
            }
            if (!stop) {
                await media.Start();
                lock (sync) {
                    stop = ending || failed || media.IsClosed;
                    if (!stop) { active = true; answer.TrySetResult(true); }
                }
            }
            if (stop) { answer.TrySetResult(false); Hangup(); }
            return SocketError.Success;
        } catch { Fail(); return SocketError.Fault; }
    }

    internal static bool ValidSdp(string contentType, string body) =>
        !string.IsNullOrWhiteSpace(body) && body.Length <= 65536 &&
        string.Equals(contentType?.Split(';')[0].Trim(), SDP.SDP_MIME_CONTENTTYPE, StringComparison.OrdinalIgnoreCase);

    internal static string RejectedAnswer(string offer) {
        try {
            if (string.IsNullOrWhiteSpace(offer) || offer.Length > 65536) return null;
            var rejected = SDP.ParseSDPDescription(offer);
            rejected.SessionId = "0"; rejected.AnnouncementVersion = 0;
            rejected.Connection = new SDPConnectionInformation(IPAddress.Any);
            foreach (var track in rejected.Media) { track.Port = 0; track.MediaStreamStatus = MediaStreamStatusEnum.Inactive; }
            return rejected.ToString();
        } catch { return null; }
    }

    public void Hangup() {
        lock (sync) {
            ending = true; active = false; deadline?.Dispose(); reInviteDeadline?.Dispose();
            if (!sent) { answer.TrySetResult(false); settled.TrySetResult(); }
        }
        StopIfRequested();
    }

    private void StopIfRequested() {
        try {
            lock (sync) {
                if (!ending || closed || !sent) return;
                if (dialogue != null && finalReceived) {
                    // The200 handler must finish its ACK before this BYE. An earlier cancellation
                    // only sets ending; DialogueState becomes Confirmed immediately after ACK.
                    if (dialogue.DialogueState != SIPDialogueStateEnum.Confirmed || byeSent || remoteEnded) return;
                    byeSent = true;
                    var bye = dialogue.GetInDialogRequest(SIPMethodsEnum.BYE);
                    bye.SetSendFromHints(exchange.Current.TransactionRequest.LocalSIPEndPoint);
                    SendBye(bye, false);
                } else if (!finalReceived && provisional && !cancelSent) {
                    cancelSent = true;
                    var original = exchange.Current.TransactionRequest;
                    var cancel = original.Copy(); cancel.Method = SIPMethodsEnum.CANCEL;
                    cancel.Header.CSeqMethod = SIPMethodsEnum.CANCEL; cancel.Body = null;
                    cancel.Header.ContentType = null; cancel.Header.ContentLength = 0;
                    cancel.Header.AuthenticationHeaders.Clear();
                    cancel.SetSendFromHints(original.LocalSIPEndPoint);
                    exchange.Current.CancelCall();
                    var cancelling = new SIPNonInviteTransaction(transport, cancel, null);
                    cancelling.NonInviteTransactionFailed += (_, _) => Fail();
                    cancelling.SendRequest();
                }
            }
        } catch { Fail(); }
    }

    private void SendBye(SIPRequest request, bool authenticated) {
        var transaction = new SIPNonInviteTransaction(transport, request, outboundProxy);
        transaction.NonInviteTransactionFailed += (_, _) => Fail();
        transaction.NonInviteTransactionFinalResponseReceived += (local, remote, current, response) => {
            try {
                if (response.StatusCode is 401 or 407 && !authenticated && !string.IsNullOrWhiteSpace(descriptor.Password)) {
                    var next = request.DuplicateAndAuthenticate(response.Header.AuthenticationHeaders,
                        string.IsNullOrWhiteSpace(descriptor.AuthUsername) ? descriptor.Username : descriptor.AuthUsername, descriptor.Password);
                    SendBye(next, true);
                } else if (response.StatusCode is >= 200 and <= 299 or 481) settled.TrySetResult();
                else Fail();
                return Task.FromResult(SocketError.Success);
            } catch { Fail(); return Task.FromResult(SocketError.Fault); }
        };
        transaction.SendRequest();
    }

    private Task Requested(SIPEndPoint local, SIPEndPoint remote, SIPRequest request) {
        SIPDialogue current;
        lock (sync) {
            current = dialogue;
            if (closed || current == null || request.Header.CallId != current.CallId ||
                request.Header.To?.ToTag != current.LocalTag || request.Header.From?.FromTag != current.RemoteTag)
                return Task.CompletedTask;
        }
        if (request.Method == SIPMethodsEnum.BYE) {
            lock (sync) {
                if (request.Header.CSeq <= current.RemoteCSeq) {
                    new SIPNonInviteTransaction(transport, request, null).SendResponse(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.InternalServerError, null));
                    return Task.CompletedTask;
                }
                current.RemoteCSeq = request.Header.CSeq; remoteEnded = true;
                ending = true; active = false; deadline?.Dispose(); reInviteDeadline?.Dispose();
            }
            new SIPNonInviteTransaction(transport, request, null).SendResponse(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.Ok, null));
            answer.TrySetResult(false); settled.TrySetResult(); ended();
        } else if (request.Method == SIPMethodsEnum.INVITE) Reinvite(request, current);
        else if (request.Method == SIPMethodsEnum.CANCEL) {
            lock (sync) {
                bool matches = reInvite != null && request.Header.CSeq == reInvite.TransactionRequest.Header.CSeq &&
                    request.Header.Vias.TopViaHeader.Branch == reInvite.TransactionRequest.Header.Vias.TopViaHeader.Branch;
                new SIPNonInviteTransaction(transport, request, null).SendResponse(SIPResponse.GetResponse(request,
                    matches ? SIPResponseStatusCodesEnum.Ok : SIPResponseStatusCodesEnum.CallLegTransactionDoesNotExist, null));
                if (matches) reInvite.CancelCall(request);
            }
        }
        else if (request.Method is not (SIPMethodsEnum.ACK or SIPMethodsEnum.INFO)) {
            var status = request.Method == SIPMethodsEnum.OPTIONS ? SIPResponseStatusCodesEnum.Ok : SIPResponseStatusCodesEnum.MethodNotAllowed;
            new SIPNonInviteTransaction(transport, request, null).SendResponse(SIPResponse.GetResponse(request, status, null));
        }
        return Task.CompletedTask;
    }

    private void Reinvite(SIPRequest request, SIPDialogue current) {
        var transaction = new UASInviteTransaction(transport, request, null);
        lock (sync) {
            if (ending || failed || !active || !ReferenceEquals(current, dialogue) || request.Header.CSeq <= current.RemoteCSeq) {
                transaction.SendFinalResponse(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.CallLegTransactionDoesNotExist, null)); return;
            }
            if (reInvite != null) { transaction.SendFinalResponse(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.RequestPending, null)); return; }
            reInvite = transaction; current.RemoteCSeq = request.Header.CSeq;
            transaction.UASInviteTransactionCancelled += (original, _) => {
                lock (sync) {
                    if (!ReferenceEquals(reInvite, original)) return;
                    reInvite = null; reInviteDeadline?.Dispose();
                    ending = true; active = false;
                }
                // SDP may already have changed. End the original call after CANCEL processing,
                // without invoking its parent while the protocol lock is held.
                ThreadPool.QueueUserWorkItem(_ => { Hangup(); ended(); });
            };
        }
        try {
            bool hasOffer = !string.IsNullOrEmpty(request.Body);
            SDP sdp;
            if (hasOffer) {
                if (!ValidSdp(request.Header.ContentType, request.Body) ||
                    media.SetRemoteDescription(SdpType.offer, SDP.ParseSDPDescription(request.Body)) != SetDescriptionResultEnum.OK)
                    throw new InvalidOperationException("Unsupported original re-INVITE offer.");
                sdp = media.CreateAnswer(null);
            } else sdp = media.CreateOffer();
            if (sdp == null || media.IsClosed || media.Failed) throw new InvalidOperationException("No current SDP negotiation.");
            transaction.OnAckReceived += (local, remote, original, ack) => {
                try {
                    lock (sync) {
                        if (!ReferenceEquals(reInvite, original) || ending || ack.Header.CSeq != request.Header.CSeq ||
                            ack.Header.From?.FromTag != current.RemoteTag || ack.Header.To?.ToTag != current.LocalTag) return Task.FromResult(SocketError.Success);
                    }
                    if (!hasOffer && (!ValidSdp(ack.Header.ContentType, ack.Body) ||
                        media.SetRemoteDescription(SdpType.answer, SDP.ParseSDPDescription(ack.Body)) != SetDescriptionResultEnum.OK))
                        throw new InvalidOperationException("Missing or invalid re-INVITE ACK answer.");
                    lock (sync) {
                        if (!ReferenceEquals(reInvite, original) || ending) return Task.FromResult(SocketError.Success);
                        if (request.Header.Contact?.Count > 0) current.RemoteTarget = request.Header.Contact[0].ContactURI.CopyOf();
                        current.SDP = sdp.ToString(); current.RemoteSDP = hasOffer ? request.Body : ack.Body;
                        reInvite = null; reInviteDeadline?.Dispose();
                    }
                    return Task.FromResult(SocketError.Success);
                } catch { Fail(); Hangup(); return Task.FromResult(SocketError.Fault); }
            };
            transaction.UASInviteTransactionFailed += (original, _) => {
                lock (sync) if (!ReferenceEquals(reInvite, original) || ending || closed) return;
                Fail(); Hangup();
            };
            lock (sync) {
                if (!ReferenceEquals(reInvite, transaction)) return;
                if (ending || failed) throw new InvalidOperationException("Original call ended during re-INVITE.");
                reInviteDeadline = new Timer(_ => {
                    lock (sync) {
                        if (!ReferenceEquals(reInvite, transaction) || ending || closed) return;
                        ending = true; active = false;
                    }
                    Fail(); Hangup();
                }, null, TimeSpan.FromSeconds(5), Timeout.InfiniteTimeSpan);
                transaction.SendFinalResponse(transaction.GetOkResponse(SDP.SDP_MIME_CONTENTTYPE, sdp.ToString()));
            }
        } catch {
            lock (sync) {
                if (!ReferenceEquals(reInvite, transaction)) return;
                transaction.SendFinalResponse(SIPResponse.GetResponse(request, SIPResponseStatusCodesEnum.NotAcceptableHere, null));
                reInvite = null; reInviteDeadline?.Dispose();
            }
            // SetRemoteDescription may already have altered media; stop rather than reuse partial SDP.
            Hangup(); ended();
        }
    }

    private void RtpTimeout(SDPMediaTypesEnum _) { Hangup(); ended(); }
    private void Fail() {
        bool notify;
        lock (sync) {
            if (closed || settled.Task.IsCompletedSuccessfully) return;
            notify = !failed; failed = true; ending = true; active = false;
            deadline?.Dispose(); reInviteDeadline?.Dispose();
            answer.TrySetException(new InvalidOperationException("Original SIP outcome is unknown."));
            if (!sent) settled.TrySetResult();
        }
        // Never call the parent while holding this protocol lock.
        if (notify) ThreadPool.QueueUserWorkItem(_ => uncertain());
    }

    public ValueTask DisposeAsync() { lock (sync) { disposal ??= DisposeCoreAsync(); return new ValueTask(disposal); } }
    private async Task DisposeCoreAsync() {
        await Task.Yield(); Hangup();
        // Keep the original transaction alive for a late200/ACK/BYE. Missing remote cleanup
        // evidence prevents SipTransportHost from releasing its original call slot.
        await settled.Task.WaitAsync(TimeSpan.FromSeconds(35));
        Task finishing;
        lock (sync) finishing = finalWork;
        await finishing.WaitAsync(TimeSpan.FromSeconds(5));
        lock (sync) { closed = true; deadline?.Dispose(); reInviteDeadline?.Dispose(); }
        transport.SIPTransportRequestReceived -= Requested; media.OnTimeout -= RtpTimeout;
    }
}
