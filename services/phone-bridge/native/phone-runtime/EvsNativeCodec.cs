using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Ivy.PhoneBridge;

// A fixed, packaged ABI. Each negotiated direction and audio generation receives a fresh handle.
// The shared library has no Desktop, audio-device, configuration or network side effects.
internal sealed class EvsNativeCodec : IDisposable {
    internal const string FileName = "ivy_phone_evs.dll";
    internal const int SampleRate = 48000, Samples = 960;
    private readonly object sync = new();
    private readonly bool encoding;
    private readonly int channels;
    private readonly CodecHandle handle;
    private readonly EncodeDelegate encode;
    private readonly DecodeDelegate decode;

    public EvsNativeCodec(bool encoding, int bitrate = 128000, int bandwidth = 3, int channels = 1) {
        this.encoding = encoding; this.channels = channels;
        nint library = NativeLibrary.Load(Path.Combine(AppContext.BaseDirectory, FileName));
        nint created = 0; DestroyDelegate destroy = null;
        try {
            T Export<T>(string name) where T : Delegate => Marshal.GetDelegateForFunctionPointer<T>(NativeLibrary.GetExport(library, name));
            if (Export<AbiDelegate>("ivy_phone_evs_abi")() != 2) throw new InvalidOperationException("Unsupported packaged EVS ABI.");
            destroy = Export<DestroyDelegate>("ivy_phone_evs_destroy");
            encode = Export<EncodeDelegate>("ivy_phone_evs_encode"); decode = Export<DecodeDelegate>("ivy_phone_evs_decode");
            int result = Export<CreateDelegate>("ivy_phone_evs_create")(encoding ? 1 : 0, bitrate, bandwidth, channels, out created);
            if (result != 0 || created == 0) throw new InvalidOperationException("EVS direction initialization failed.");
            handle = new CodecHandle(created, library, destroy);
        } catch {
            try { if (created != 0) destroy?.Invoke(created); }
            finally { NativeLibrary.Free(library); }
            throw;
        }
    }

    internal byte[] Encode(short[] pcm, bool headerFull) {
        lock (sync) {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            if (!encoding || pcm == null || pcm.Length != Samples) throw new ArgumentException("Exact EVS encoding direction and20ms PCM required.");
            byte[] packet = new byte[channels * 321];
            int bytes = encode(handle, pcm, pcm.Length, packet, packet.Length, headerFull ? 1 : 0);
            if (bytes is < 1 || bytes > packet.Length) { Array.Clear(packet); throw new InvalidOperationException("EVS frame encoding failed."); }
            var result = packet.AsSpan(0, bytes).ToArray(); Array.Clear(packet); return result;
        }
    }

    internal short[] Decode(byte[] packet, bool headerFullOnly) {
        lock (sync) {
            ObjectDisposedException.ThrowIf(handle.IsClosed, this);
            if (encoding || packet == null || packet.Length is < 1 or > 4096) throw new ArgumentException("Bounded EVS decoding direction and RTP payload required.");
            var pcm = new short[Samples * 6];
            try {
                int samples = decode(handle, packet, packet.Length, pcm, pcm.Length, headerFullOnly ? 1 : 0);
                if (samples < Samples || samples > pcm.Length || samples % Samples != 0)
                    throw new InvalidOperationException("EVS frame decoding failed.");
                return pcm.AsSpan(0, samples).ToArray();
            } finally { Array.Clear(pcm); }
        }
    }

    public void Dispose() { lock (sync) handle.Dispose(); }

    private sealed class CodecHandle : SafeHandleZeroOrMinusOneIsInvalid {
        private readonly nint library;
        private readonly DestroyDelegate destroy;
        public CodecHandle(nint value, nint library, DestroyDelegate destroy) : base(true) {
            this.library = library; this.destroy = destroy; SetHandle(value);
        }
        protected override bool ReleaseHandle() {
            try { destroy(handle); }
            finally { NativeLibrary.Free(library); }
            return true;
        }
    }
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate int AbiDelegate();
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate int CreateDelegate(int encoding, int bitrate, int bandwidth, int channels, out nint handle);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void DestroyDelegate(nint handle);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate int EncodeDelegate(SafeHandle handle, [In] short[] pcm, int samples, [Out] byte[] payload, int capacity, int full);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate int DecodeDelegate(SafeHandle handle, [In] byte[] payload, int bytes, [Out] short[] pcm, int capacity, int full);
}
