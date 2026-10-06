using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace Ivy.PhoneBridge {
    public sealed class CaptureSession {
        public readonly string instanceId, state;
        public readonly uint pid;
        public readonly bool singleProcess;
        public CaptureSession(string instanceId, int state, uint pid, int processResult) {
            CaptureObservation.ValidateText(instanceId);
            if (state < 0 || state > 2) throw new ArgumentException("Invalid session state.");
            // AUDCLNT_S_NO_SINGLE_PROCESS is success, but PID is only the original creator.
            if (processResult != 0 && processResult != 0x0889000d)
                throw new InvalidOperationException("Audio process identity unavailable.");
            this.instanceId = instanceId; this.state = new[] { "inactive", "active", "expired" }[state];
            this.pid = pid; this.singleProcess = processResult == 0 && pid != 0;
        }
    }

    public interface ICaptureEndpoint : IDisposable {
        string Id { get; }
        bool IsActiveCapture { get; }
        CaptureSession[] ReadSessions();
    }

    public sealed class CaptureObservation {
        public readonly string endpointId, state;
        public readonly CaptureSession[] sessions;
        CaptureObservation(string endpointId, string state, CaptureSession[] sessions) {
            this.endpointId = endpointId; this.state = state; this.sessions = sessions;
        }
        internal static void ValidateText(string value) {
            if (String.IsNullOrWhiteSpace(value) || value.Length > 32767 || value.IndexOf('\0') >= 0)
                throw new ArgumentException("An exact native identifier is required.");
        }
        // Each observation opens/releases its own endpoint. No cached positive result, device
        // fallback, audio stream, notification registration or volume/default-device mutation.
        public static CaptureObservation Read(string endpointId, Func<string, ICaptureEndpoint> open) {
            ValidateText(endpointId);
            if (open == null) throw new ArgumentNullException("open");
            try {
                using (var endpoint = open(endpointId)) {
                    if (endpoint == null || !String.Equals(endpoint.Id, endpointId, StringComparison.Ordinal) || !endpoint.IsActiveCapture)
                        throw new InvalidOperationException("Capture endpoint unavailable.");
                    var values = endpoint.ReadSessions();
                    if (values == null || values.Length > 4096) throw new InvalidOperationException("Session inventory unavailable.");
                    var ids = new HashSet<string>(StringComparer.Ordinal);
                    foreach (var value in values) if (value == null || !ids.Add(value.instanceId))
                        throw new InvalidOperationException("Ambiguous session inventory.");
                    if (!endpoint.IsActiveCapture || !String.Equals(endpoint.Id, endpointId, StringComparison.Ordinal))
                        throw new InvalidOperationException("Capture endpoint changed.");
                    return new CaptureObservation(endpointId, "observed", (CaptureSession[])values.Clone());
                }
            } catch { return new CaptureObservation(endpointId, "unavailable", new CaptureSession[0]); }
        }
    }

    public static class WindowsCaptureObservation {
        public static CaptureObservation Observe(string endpointId) {
            return CaptureObservation.Read(endpointId, id => new Endpoint(id));
        }
        // Only the methods used below have callable signatures. Reserved methods preserve SDK
        // vtable positions; no audio client or mutation interface is exposed by this adapter.
        [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IDevices {
            void ReservedEnumerate(); void ReservedDefault();
            [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string id, out IDevice device);
        }
        [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IDevice {
            [PreserveSig] int Activate(ref Guid iid, uint context, IntPtr parameters, [MarshalAs(UnmanagedType.IUnknown)] out object result);
            void ReservedProperties();
            [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id);
            [PreserveSig] int GetState(out uint state);
        }
        [ComImport, Guid("1BE09788-6894-4089-8586-9A2A6C265AC5"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface IEndpoint { [PreserveSig] int GetDataFlow(out int flow); }
        [ComImport, Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface ISessionManager {
            void ReservedSessionControl(); void ReservedVolume();
            [PreserveSig] int GetSessionEnumerator(out ISessions sessions);
        }
        [ComImport, Guid("E2F5BB11-0570-40CA-ACDD-3AA01277DEE8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface ISessions {
            [PreserveSig] int GetCount(out int count);
            [PreserveSig] int GetSession(int index, [MarshalAs(UnmanagedType.IUnknown)] out object session);
        }
        [ComImport, Guid("BFB7FF88-7239-4FC9-8FA2-07C950BE9C6D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        interface ISession {
            [PreserveSig] int GetState(out int state);
            void ReservedDisplayName(); void ReservedSetDisplayName();
            void ReservedIcon(); void ReservedSetIcon();
            void ReservedGrouping(); void ReservedSetGrouping();
            void ReservedRegister(); void ReservedUnregister();
            void ReservedSessionIdentifier();
            [PreserveSig] int GetInstanceIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string id);
            [PreserveSig] int GetProcessId(out uint pid);
        }
        [DllImport("ole32.dll")] static extern int CoInitializeEx(IntPtr reserved, uint mode);
        [DllImport("ole32.dll")] static extern void CoUninitialize();
        static void Check(int result) {
            if (result != 0) throw new InvalidOperationException("Native audio observation failed.");
        }
        static void Release(object value) { if (value != null) Marshal.FinalReleaseComObject(value); }

        sealed class Endpoint : ICaptureEndpoint {
            IDevices devices; IDevice device; bool initialized;
            public Endpoint(string id) {
                if (Environment.OSVersion.Platform != PlatformID.Win32NT || IntPtr.Size != 8)
                    throw new PlatformNotSupportedException("Windows x64 required.");
                try {
                    // The bounded helper uses MTA. Do not silently reuse an incompatible apartment.
                    int result = CoInitializeEx(IntPtr.Zero, 0);
                    if (result != 0 && result != 1) Marshal.ThrowExceptionForHR(result);
                    initialized = true;
                    devices = (IDevices)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")));
                    Check(devices.GetDevice(id, out device));
                } catch { Dispose(); throw; }
            }
            public string Id { get { string id; Check(device.GetId(out id)); return id; } }
            public bool IsActiveCapture {
                get { uint state; int flow; Check(device.GetState(out state)); Check(((IEndpoint)device).GetDataFlow(out flow)); return state == 1 && flow == 1; }
            }
            public CaptureSession[] ReadSessions() {
                object managerObject = null; ISessions sessions = null;
                try {
                    var iid = typeof(ISessionManager).GUID;
                    Check(device.Activate(ref iid, 23, IntPtr.Zero, out managerObject));
                    Check(((ISessionManager)managerObject).GetSessionEnumerator(out sessions));
                    int count; Check(sessions.GetCount(out count));
                    if (count < 0 || count > 4096) throw new InvalidOperationException("Audio session limit exceeded.");
                    var result = new CaptureSession[count];
                    for (int index = 0; index < count; index++) {
                        object value = null;
                        try {
                            Check(sessions.GetSession(index, out value));
                            var session = (ISession)value;
                            string id; uint pid; int state;
                            Check(session.GetInstanceIdentifier(out id));
                            int processResult = session.GetProcessId(out pid);
                            Check(session.GetState(out state));
                            result[index] = new CaptureSession(id, state, pid, processResult);
                        } finally { Release(value); }
                    }
                    return result;
                } finally { try { Release(sessions); } finally { Release(managerObject); } }
            }
            public void Dispose() {
                try { Release(device); }
                finally { device = null; try { Release(devices); }
                    finally { devices = null; if (initialized) { initialized = false; CoUninitialize(); } } }
            }
        }
    }
}
