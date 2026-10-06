namespace Ivy.PhoneBridge;

public interface ICallAudioRoute : IPcmAudioPort, IAsyncDisposable {
    MediaStatus Status { get; }
    bool Failed { get; }
    void Resume();
    void Suspend();
}
public interface ICallAudioPort : IPcmAudioPort, IAsyncDisposable {
    string CallId { get; }
    bool Failed { get; }
    void Revoke();
}
public sealed record AudioAttachmentStatus(string CallId, string State, MediaStatus Route);
// Only the route factory that awaited successful disposal may assert this distinction.
internal sealed class AudioPreparationReleasedException(Exception cause) : Exception("Audio preparation failed after confirmed resource release.", cause);

// The admitted call owns this port and its one asynchronous device preparation. Authority comes
// from the separately supplied original call/session permit, never from SIP or device readiness.
public sealed class CallAudioPort : ICallAudioPort {
    public delegate Task<ICallAudioRoute> RouteFactory(AudioSettings settings, DesktopIdentity desktop, Func<bool> permitted, CancellationToken cancellationToken);
    private readonly object sync = new();
    private readonly AudioPermit permit;
    private readonly RouteFactory factory;
    private readonly CancellationTokenSource stop = new();
    private ICallAudioRoute route;
    private Task preparation, disposal;
    private Task<AudioAttachmentStatus> rebind;
    private Task cancellation = Task.CompletedTask;
    private int revoked;
    private long permittedGeneration = -1;
    private bool failed, closed, revocationStarted, recoveryArmed, rebindAttempted;
    private Exception revocationError;
    private Exception rebindCleanupError;
    private string captureEndpointId;
    private DesktopIdentity preparedDesktop;
    private AudioSettings preparedSettings;
    private bool authorityBound;
    public string CallId { get; }
    public CallAudioPort(string callId, AudioPermit permit, RouteFactory factory = null) {
        if (!Guid.TryParseExact(callId, "D", out _)) throw new ArgumentException("Original audio call UUID required.");
        ArgumentNullException.ThrowIfNull(permit);
        CallId = callId; this.permit = permit;
        this.factory = factory ?? (async (settings, desktop, permitted, cancellationToken) =>
            await WindowsAudioRoute.OpenAsync(settings, desktop, permitted, cancellationToken));
    }
    private bool Permitted() => Volatile.Read(ref revoked) == 0 && !Volatile.Read(ref failed) && permit.IsCurrent() && permit.Generation == Interlocked.Read(ref permittedGeneration);
    public bool Failed {
        get { lock (sync) {
            try { if (route?.Failed == true) failed = true; } catch { failed = true; }
            return failed;
        } }
    }
    internal bool RecoveryPending { get { lock (sync) return recoveryArmed && revoked == 0 && !closed &&
        (!rebindAttempted || rebind is { IsCompleted: false }); } }
    internal void ArmVoiceRecovery() {
        lock (sync) {
            if (!authorityBound || revoked != 0 || closed || Failed || route == null)
                throw new InvalidOperationException("Only active original Voice audio can arm recovery.");
            recoveryArmed = true;
        }
    }
    public AudioAttachmentStatus Status {
        get {
            lock (sync) {
                bool open = IsOpen; var status = route?.Status;
                string state = closed ? "closed" : failed ? "failed" : revoked != 0 ? "revoked" : preparation == null ? "unprepared" :
                    route == null ? "preparing" : open ? "open" : "suspended";
                return new(CallId, state, status);
            }
        }
    }
    public bool IsOpen { get { lock (sync) return !Failed && Permitted() && route != null && route.IsOpen; } }
    public long Generation { get { lock (sync) return route?.Generation ?? 0; } }
    public Task PrepareAsync(AudioSettings settings, DesktopIdentity desktop, CancellationToken cancellationToken = default) {
        ArgumentNullException.ThrowIfNull(settings); settings.Validate();
        if (desktop == null && settings.SourceMode == "desktop_process") throw new ArgumentException("Desktop process capture requires its owner.");
        lock (sync) {
            if (revoked != 0 || preparation != null) throw new InvalidOperationException("An original call can prepare its devices only once.");
            captureEndpointId = settings.CaptureEndpointId;
            preparedDesktop = desktop;
            preparedSettings = settings;
            preparation = PrepareCoreAsync(settings, desktop, cancellationToken); return preparation;
        }
    }
    internal Task<AudioAttachmentStatus> RebindAsync(DesktopIdentity desktop, Func<bool> originalVoiceCapture) {
        ArgumentNullException.ThrowIfNull(desktop); ArgumentNullException.ThrowIfNull(originalVoiceCapture);
        if (!originalVoiceCapture()) throw new NativeRpcException("phone_audio_owner_changed");
        lock (sync) {
            if (!recoveryArmed || rebindAttempted || revoked != 0 || closed || !Failed || route == null ||
                preparedDesktop == null || desktop.pid != preparedDesktop.pid ||
                desktop.startTimeUtcTicks != preparedDesktop.startTimeUtcTicks ||
                desktop.appUserModelId != preparedDesktop.appUserModelId ||
                !StringComparer.OrdinalIgnoreCase.Equals(desktop.imagePath, preparedDesktop.imagePath))
                throw new NativeRpcException("runtime_not_ready");
            rebindAttempted = true;
            rebind = RebindCoreAsync(originalVoiceCapture);
            return rebind;
        }
    }
    private async Task<AudioAttachmentStatus> RebindCoreAsync(Func<bool> originalVoiceCapture) {
        await Task.Yield();
        ICallAudioRoute previous, opened = null;
        lock (sync) { previous = route; route = null; }
        try {
            try { await previous.DisposeAsync(); }
            catch (Exception error) { rebindCleanupError = error; throw; }
            if (!originalVoiceCapture() || !permit.IsCurrent() || Volatile.Read(ref revoked) != 0)
                throw new NativeRpcException("phone_audio_owner_changed");
            try { opened = await factory(preparedSettings, preparedDesktop, Permitted, stop.Token); }
            catch (AudioPreparationReleasedException) { throw; }
            catch (Exception error) { rebindCleanupError = error; throw; }
            if (opened == null || opened.IsOpen || opened.Status.State != "suspended" ||
                !originalVoiceCapture() || !permit.IsCurrent() || Volatile.Read(ref revoked) != 0)
                throw new NativeRpcException("phone_audio_owner_changed");
            lock (sync) {
                if (revoked != 0 || closed || !permit.IsCurrent()) throw new NativeRpcException("runtime_not_ready");
                route = opened; opened = null; failed = false;
                Interlocked.Exchange(ref permittedGeneration, -1);
                Activate();
                if (!IsOpen) throw new NativeRpcException("phone_audio_not_ready");
                return Status;
            }
        } catch { lock (sync) failed = true; throw; }
        finally { if (opened != null) {
            try { await opened.DisposeAsync(); }
            catch (Exception error) { rebindCleanupError = error; throw; }
        } }
    }
    private async Task PrepareCoreAsync(AudioSettings settings, DesktopIdentity desktop, CancellationToken cancellationToken) {
        await Task.Yield(); // Publish the original task before a factory can complete or call back.
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(stop.Token, cancellationToken);
        ICallAudioRoute opened = null;
        try {
            linked.Token.ThrowIfCancellationRequested();
            if (Volatile.Read(ref revoked) != 0) throw new OperationCanceledException(linked.Token);
            opened = await factory(settings, desktop, Permitted, linked.Token);
            linked.Token.ThrowIfCancellationRequested();
            if (opened == null || opened.IsOpen || opened.Status.State != "suspended") throw new InvalidOperationException("New call audio must start suspended.");
            lock (sync) {
                if (revoked != 0) throw new OperationCanceledException(linked.Token);
                route = opened; opened = null;
            }
        } catch { lock (sync) failed = revoked == 0; throw; }
        finally { if (opened != null) await opened.DisposeAsync(); }
    }
    public void Activate() {
        lock (sync) {
            if (revoked != 0 || Failed || route == null || !permit.IsCurrent()) throw new InvalidOperationException("Prepared original call and fresh audio authority required.");
            long currentGeneration = permit.Generation;
            if (currentGeneration != permittedGeneration) { route.Suspend(); Interlocked.Exchange(ref permittedGeneration, currentGeneration); }
            route.Resume();
        }
    }
    public void Suspend() { lock (sync) route?.Suspend(); }
    internal bool IsPreparedFor(string endpointId) { lock (sync) return captureEndpointId == endpointId && route != null && !Failed && revoked == 0; }
    internal (DesktopIdentity Desktop, string EndpointId) VoiceTarget {
        get { lock (sync) {
            if (preparedDesktop == null || !IsPreparedFor(captureEndpointId)) throw new InvalidOperationException("Original prepared audio target required.");
            return (preparedDesktop, captureEndpointId);
        } }
    }
    internal void BindAuthority(string endpointId) {
        lock (sync) {
            if (authorityBound || !IsPreparedFor(endpointId)) throw new InvalidOperationException("One authority per original prepared call required.");
            authorityBound = true; ClearAuthority();
        }
    }
    internal void RefreshAuthority(long timestamp) {
        lock (sync) { permit.Renew(timestamp, TimeSpan.FromMilliseconds(500)); Activate(); }
    }
    internal void GrantAuthority() { lock (sync) { permit.Grant(); Activate(); } }
    internal void ClearAuthority() {
        lock (sync) { permit.Suspend(); route?.Suspend(); }
    }
    internal void ReleaseVoiceAuthority() {
        lock (sync) {
            if (revoked != 0 || closed || Failed) throw new InvalidOperationException("Ended call audio cannot acquire a replacement Voice owner.");
            ClearAuthority(); authorityBound = false;
        }
    }
    public void Revoke() {
        Volatile.Write(ref revoked, 1);
        lock (sync) {
            if (revocationStarted) return; revocationStarted = true;
            permit.Revoke();
            // Cancellation callbacks can be arbitrarily slow; call hangup must not wait for them.
            try { cancellation = stop.CancelAsync(); route?.Suspend(); }
            catch (Exception error) { failed = true; revocationError = error; }
        }
    }
    public int ReadCaptured(Span<float> output) {
        lock (sync) {
            if (Failed || !Permitted() || route == null) { output.Clear(); return 0; }
            int count = route.ReadCaptured(output);
            if (Failed || !Permitted()) { output.Clear(); return 0; }
            return count;
        }
    }
    public void WriteReceived(ReadOnlySpan<float> input) {
        lock (sync) if (!Failed && Permitted() && route != null) route.WriteReceived(input);
    }
    public ValueTask DisposeAsync() {
        Revoke();
        lock (sync) { disposal ??= DisposeCoreAsync(); return new ValueTask(disposal); }
    }
    private async Task DisposeCoreAsync() {
        await Task.Yield();
        Exception failure = null;
        Task preparing, rebinding; ICallAudioRoute owned;
        lock (sync) { preparing = preparation; rebinding = rebind; }
        try { if (preparing != null) await preparing; }
        catch (AudioPreparationReleasedException) { /* Preparation failed; its factory already confirmed all owned resources released. */ }
        catch (OperationCanceledException) { /* Cancellation itself is not failed resource cleanup. */ }
        catch (Exception error) { failure = error; }
        try { if (rebinding != null) await rebinding; }
        catch { /* Only an unconfirmed resource cleanup blocks release below. */ }
        lock (sync) { owned = route; route = null; }
        try {
            if (owned != null) await owned.DisposeAsync();
            await cancellation;
            if (revocationError != null) throw revocationError;
            if (rebindCleanupError != null) throw rebindCleanupError;
            if (failure != null) throw failure;
            lock (sync) closed = true;
        } catch { lock (sync) failed = true; throw; }
        finally { stop.Dispose(); }
    }
}
