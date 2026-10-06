namespace Ivy.PhoneBridge;

public static class NativeEntryPoint {
    // Inherited pipes belong to the parent's Windows job. Never run synthetic media or accept
    // account credentials on a command line. The server bounds cleanup before this process exits.
    public static async Task<int> Main(string[] args) {
        if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess || args.Length != 4 ||
            args[0] != "--epoch" || !Guid.TryParseExact(args[1], "D", out _) || args[2] != "--owner-root") return 64;
        try {
            NativeOwnerLease.Acquire(args[3], args[1]);
            var operations = new NativeSipOperations(callId => new CallAudioPort(callId, new AudioPermit()));
            await new NativePipeServer(args[1], Console.OpenStandardInput(), Console.OpenStandardOutput(),
                operations.InvokeAsync, () => operations.DisposeAsync().AsTask(), cleanupSlots: () => Math.Max(4, operations.ActiveCalls * 4)).RunAsync();
            return 0;
        } catch {
            // Do not await cleanup again: it may be the original native device operation that
            // exceeded the server deadline. Process exit and the parent's stop fence own that case.
            Console.Error.WriteLine("phone_runtime_failed"); return 1;
        }
    }
}
