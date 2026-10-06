using System.Runtime.InteropServices;

namespace Ivy.PhoneBridge;

// PeriodicTimer otherwise follows the coarse Windows scheduler quantum on some hosts. The
// 20 ms RTP sender must not run at roughly 30 ms intervals and overflow both audio queues.
internal sealed class WindowsTimerResolutionLease : IDisposable {
    private int active;
    private WindowsTimerResolutionLease() {
        if (OperatingSystem.IsWindows() && TimeBeginPeriod(1) == 0) active = 1;
    }
    internal bool IsActive => Volatile.Read(ref active) != 0;
    internal static WindowsTimerResolutionLease Acquire() => new();
    public void Dispose() {
        if (Interlocked.Exchange(ref active, 0) != 0) _ = TimeEndPeriod(1);
    }
    [DllImport("winmm.dll", EntryPoint = "timeBeginPeriod")]
    private static extern uint TimeBeginPeriod(uint periodMilliseconds);
    [DllImport("winmm.dll", EntryPoint = "timeEndPeriod")]
    private static extern uint TimeEndPeriod(uint periodMilliseconds);
}
