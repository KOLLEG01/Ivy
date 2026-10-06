using System;
using Ivy.PhoneBridge;

public static class CaptureObservationTests {
    sealed class Endpoint : ICaptureEndpoint {
        public string Id { get; set; }
        public bool Active = true, FailRead, FailDispose, InvalidateOnRead;
        public int Reads, Disposals;
        public CaptureSession[] Values = new CaptureSession[0];
        public bool IsActiveCapture { get { return Active; } }
        public CaptureSession[] ReadSessions() {
            Reads++;
            if (FailRead) throw new Exception("device removed");
            if (InvalidateOnRead) Active = false;
            return Values;
        }
        public void Dispose() { Disposals++; if (FailDispose) throw new Exception("release failed"); }
    }
    static void Check(bool value, string label) { if (!value) throw new Exception(label); }
    static CaptureObservation Read(Endpoint endpoint) { return CaptureObservation.Read("exact-capture-id", id => {
        Check(id == "exact-capture-id", "exact requested endpoint"); return endpoint;
    }); }
    public static void Run() {
        var endpoint = new Endpoint { Id = "exact-capture-id", Values = new[] {
            new CaptureSession("active", 1, 123, 0), new CaptureSession("inactive", 0, 456, 0),
            new CaptureSession("expired", 2, 456, 0), new CaptureSession("shared", 1, 123, 0x0889000d),
            new CaptureSession("system", 1, 0, 0)
        } };
        var observed = Read(endpoint);
        Check(observed.state == "observed" && observed.sessions.Length == 5, "complete inventory");
        Check(observed.sessions[0].singleProcess && observed.sessions[0].state == "active", "single process active");
        Check(!observed.sessions[3].singleProcess && observed.sessions[3].pid == 123, "shared success is not exclusive ownership");
        Check(!observed.sessions[4].singleProcess, "system session cannot own Desktop audio");
        Check(endpoint.Disposals == 1 && endpoint.Reads == 1, "released complete observation");
        endpoint.Values[0] = null;
        Check(observed.sessions[0] != null, "observation owns its snapshot");
        Check(Read(endpoint).state == "unavailable" && endpoint.Disposals == 2, "no stale successful inventory after failure");
        foreach (var bad in new[] {
            new Endpoint { Id = "wrong-id" }, new Endpoint { Id = "exact-capture-id", Active = false },
            new Endpoint { Id = "exact-capture-id", FailRead = true },
            new Endpoint { Id = "exact-capture-id", FailDispose = true },
            new Endpoint { Id = "exact-capture-id", InvalidateOnRead = true },
            new Endpoint { Id = "exact-capture-id", Values = null },
            new Endpoint { Id = "exact-capture-id", Values = new CaptureSession[4097] },
            new Endpoint { Id = "exact-capture-id", Values = new[] { new CaptureSession("same", 0, 1, 0), new CaptureSession("same", 1, 1, 0) } }
        }) {
            var failed = Read(bad);
            Check(failed.state == "unavailable" && failed.sessions.Length == 0 && bad.Disposals == 1, "failure clears inventory and releases owner");
            if (bad.Id != "exact-capture-id") Check(bad.Reads == 0, "wrong endpoint never read");
        }
        Check(CaptureObservation.Read("exact-capture-id", id => { throw new Exception("unplugged"); }).state == "unavailable", "failed open");
        foreach (var code in new[] { 1, 2, -1, unchecked((int)0x88890004) }) {
            bool rejected = false; try { new CaptureSession("session", 1, 42, code); } catch (InvalidOperationException) { rejected = true; }
            Check(rejected, "unrecognized process result cannot become ownership");
        }
        foreach (var id in new[] { null, "", " ", "bad\0id", new string('x', 32768) }) {
            bool rejected = false; try { CaptureObservation.Read(id, value => { throw new Exception("must not open"); }); }
            catch (ArgumentException) { rejected = true; }
            Check(rejected, "invalid exact identifier rejected before OS access");
        }
        Console.WriteLine("capture_observation_unit_passed: fake endpoint only; shared ownership, invalidation, disposal, complete snapshots");
    }
}
