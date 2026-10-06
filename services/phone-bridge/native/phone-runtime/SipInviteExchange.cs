using SIPSorcery.SIP;
using System.Net.Sockets;
using System.Text;

namespace Ivy.PhoneBridge;

// One original INVITE and its single authentication continuation. Sending, cancellation, SDP
// negotiation and call outcomes remain with the call owner; this boundary never sends on its own.
internal sealed class SipInviteExchange {
    private readonly object sync = new();
    private readonly SIPTransport transport;
    private readonly SIPEndPoint proxy;
    private SipInviteTransaction current;
    private bool authenticated;
    public SipInviteTransaction Current { get { lock (sync) return current; } }

    public SipInviteExchange(SIPTransport transport, SIPRequest invite, SIPEndPoint proxy = null) {
        ArgumentNullException.ThrowIfNull(transport); ArgumentNullException.ThrowIfNull(invite);
        if (invite.Method != SIPMethodsEnum.INVITE || invite.Header?.CSeqMethod != SIPMethodsEnum.INVITE ||
            string.IsNullOrWhiteSpace(invite.Header.CallId) || string.IsNullOrWhiteSpace(invite.Header.From?.FromTag) ||
            (invite.Body != null && invite.Body.Length != 0 && string.IsNullOrWhiteSpace(invite.Body)))
            throw new ArgumentException("Original INVITE with a valid body or no body required.");
        this.transport = transport; this.proxy = proxy?.CopyOf(); current = Create(invite.Copy());
    }

    private SipInviteTransaction Create(SIPRequest request) => new(transport, request, proxy);

    public SipInviteTransaction Authenticate(UACInviteTransaction expected, SIPResponse challenge, string username, string password) {
        lock (sync) {
            var original = current.TransactionRequest;
            if (!ReferenceEquals(expected, current) || authenticated || challenge == null ||
                challenge.Status is not (SIPResponseStatusCodesEnum.Unauthorised or SIPResponseStatusCodesEnum.ProxyAuthenticationRequired) ||
                challenge.Header?.CallId != original.Header.CallId || challenge.Header.CSeq != original.Header.CSeq ||
                challenge.Header.CSeqMethod != SIPMethodsEnum.INVITE || challenge.Header.From?.FromTag != original.Header.From.FromTag ||
                challenge.Header.AuthenticationHeaders?.Count is not > 0)
                throw new InvalidOperationException("Current original INVITE authentication challenge required.");
            if (string.IsNullOrWhiteSpace(username) || username.Length > 256 || username.Any(char.IsControl) ||
                string.IsNullOrWhiteSpace(password) || password.Length > 4096 || password.Any(char.IsControl))
                throw new ArgumentException("Bounded protected SIP credentials required.");
            var request = original.DuplicateAndAuthenticate(challenge.Header.AuthenticationHeaders, username, password);
            // Both transactions derive ACK ownership from their actual body. SIPSorcery's client
            // convenience API loses manual ACK on its401/407 continuation in the pinned version.
            var continuation = Create(request);
            authenticated = true; current = continuation;
            return continuation;
        }
    }
}

internal sealed class SipInviteTransaction(SIPTransport transport, SIPRequest request, SIPEndPoint proxy)
    : UACInviteTransaction(transport, request, proxy, sendOkAckManually: string.IsNullOrEmpty(request.Body)) {
    private readonly object acknowledgement = new();

    public Task<SocketError> AcknowledgeAsync(SIPResponse response, string body) {
        lock (acknowledgement) {
            if (!string.IsNullOrEmpty(TransactionRequest.Body) || AckRequest != null || response.StatusCode is < 200 or > 299 ||
                response.Header.CallId != TransactionRequest.Header.CallId || response.Header.CSeq != TransactionRequest.Header.CSeq ||
                response.Header.To?.ToTag != TransactionFinalResponse?.Header.To?.ToTag)
                throw new InvalidOperationException("Unacknowledged original delayed INVITE response required.");
            // RFC3261 section13.2.2.4: new branch, original INVITE CSeq/credentials and dialog route.
            // The library's void AckAnswer discards its send task; the protected transaction API
            // lets the owner await this single ACK before starting media or sending original BYE.
            var target = response.Header.Contact?.Count > 0 ? response.Header.Contact[0].ContactURI.CopyOf() : TransactionRequest.URI.CopyOf();
            var ack = new SIPRequest(SIPMethodsEnum.ACK, target) {
                Header = new SIPHeader(TransactionRequest.Header.From, response.Header.To, response.Header.CSeq, response.Header.CallId) {
                    CSeqMethod = SIPMethodsEnum.ACK,
                    AuthenticationHeaders = TransactionRequest.Header.AuthenticationHeaders,
                    Routes = response.Header.RecordRoutes?.Reversed() ?? TransactionRequest.Header.Routes,
                    ProxySendFrom = TransactionRequest.Header.ProxySendFrom,
                    ContentType = body == null ? null : SIPSorcery.Net.SDP.SDP_MIME_CONTENTTYPE,
                    ContentLength = body == null ? 0 : Encoding.UTF8.GetByteCount(body)
                }, Body = body
            };
            ack.SetSendFromHints(TransactionRequest.LocalSIPEndPoint);
            ack.Header.Vias.PushViaHeader(SIPViaHeader.GetDefaultSIPViaHeader());
            AckRequest = ack; UpdateTransactionState(SIPTransactionStatesEnum.Confirmed);
            return SendRequestAsync(ack);
        }
    }
}
