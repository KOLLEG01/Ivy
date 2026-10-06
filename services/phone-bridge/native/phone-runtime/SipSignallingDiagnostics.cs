using SIPSorcery.SIP;

namespace Ivy.PhoneBridge;

public sealed class SipSignallingDiagnostics : IDisposable {
    private readonly object sync = new();
    private readonly SIPTransport transport;
    private readonly string callId;
    private readonly Queue<object> events = new();
    private long sent, received;
    private bool closed;
    public SipSignallingDiagnostics(SIPTransport transport, string callId) {
        this.transport = transport; this.callId = callId;
        transport.SIPRequestInTraceEvent += RequestIn; transport.SIPRequestOutTraceEvent += RequestOut;
        transport.SIPResponseInTraceEvent += ResponseIn; transport.SIPResponseOutTraceEvent += ResponseOut;
    }
    private void Record(string id, string direction, string method, int? status) {
        if (id != callId) return;
        lock (sync) {
            if (closed) return;
            if (direction == "sent") sent++; else received++;
            if (events.Count == 16) events.Dequeue();
            events.Enqueue(new { at = DateTimeOffset.UtcNow, direction, method, status });
        }
    }
    private void RequestIn(SIPEndPoint local, SIPEndPoint remote, SIPRequest request) => Record(request.Header.CallId, "received", request.Method.ToString(), null);
    private void RequestOut(SIPEndPoint local, SIPEndPoint remote, SIPRequest request) => Record(request.Header.CallId, "sent", request.Method.ToString(), null);
    private void ResponseIn(SIPEndPoint local, SIPEndPoint remote, SIPResponse response) => Record(response.Header.CallId, "received", response.Header.CSeqMethod.ToString(), response.StatusCode);
    private void ResponseOut(SIPEndPoint local, SIPEndPoint remote, SIPResponse response) => Record(response.Header.CallId, "sent", response.Header.CSeqMethod.ToString(), response.StatusCode);
    public object Observation { get { lock (sync) return new { sent, received, events = events.ToArray() }; } }
    public void Dispose() {
        lock (sync) { if (closed) return; closed = true; }
        transport.SIPRequestInTraceEvent -= RequestIn; transport.SIPRequestOutTraceEvent -= RequestOut;
        transport.SIPResponseInTraceEvent -= ResponseIn; transport.SIPResponseOutTraceEvent -= ResponseOut;
    }
}
