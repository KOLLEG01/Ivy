using Ivy.PhoneBridge;
using SIPSorcery.Media;
using SIPSorcery.Net;
using SIPSorceryMedia.Abstractions;

static partial class Program {
    static async Task WebRtcVoiceProbe() {
        await using var route = new WebRtcAudioRoute(() => true);
        await route.PrepareAsync();
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { @event = "offer", sdp = route.Offer }));
        string? line = await Console.In.ReadLineAsync().WaitAsync(TimeSpan.FromSeconds(120));
        Check(line != null && line.Length <= 64000, "probe receives a bounded answer");
        var input = System.Text.Json.JsonDocument.Parse(line!).RootElement;
        var answer = input.GetProperty("sdp").GetString();
        var callerSpeech = input.TryGetProperty("wavPath", out var wav) ? ScreeningAudio.Parse(await File.ReadAllBytesAsync(wav.GetString()!)) : Array.Empty<float>();
        int callerOffset = 0;
        route.Resume();
        await route.AcceptAsync(answer!);
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { @event = "connected" }));
        var began = System.Diagnostics.Stopwatch.StartNew();
        bool heard = false;
        while (began.Elapsed < TimeSpan.FromSeconds(90)) {
            if (began.Elapsed.TotalSeconds >= 7 && callerOffset < callerSpeech.Length) {
                int length = Math.Min(960, callerSpeech.Length - callerOffset);
                route.WriteReceived(callerSpeech.AsSpan(callerOffset, length)); callerOffset += length;
            }
            var samples = new float[960]; route.ReadCaptured(samples);
            float peak = samples.Max(value => Math.Abs(value));
            if (!heard && peak > .01) {
                heard = true;
                Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { @event = "firstAudio", afterConnectedMs = began.Elapsed.TotalMilliseconds, peak }));
            }
            await Task.Delay(20);
        }
        Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(new { @event = "complete", status = route.Observation }));
        Check(heard, "actual Voice returns audible decoded native audio");
    }
    static async Task WebRtcAudio() {
        bool permitted = true;
        await using var route = new WebRtcAudioRoute(() => Volatile.Read(ref permitted), 80);
        using var remote = new RTCPeerConnection(null);
        using var audio = new AudioEncoder(includeOpus: true);
        AudioFormat format = audio.SupportedFormats.Single(value => value.FormatName.Equals("opus", StringComparison.OrdinalIgnoreCase));
        remote.addTrack(new MediaStreamTrack(new List<AudioFormat> { format }, MediaStreamStatusEnum.SendRecv));
        var received = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var remoteConnected = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        remote.OnAudioFormatsNegotiated += formats => format = formats.First();
        remote.onconnectionstatechange += state => { if (state == RTCPeerConnectionState.connected) remoteConnected.TrySetResult(); };
        remote.OnRtpPacketReceived += (_, media, packet) => {
            if (media != SDPMediaTypesEnum.audio) return;
            lock (audio) {
                var pcm = audio.DecodeAudio(packet.Payload, format);
                if (pcm.Any(sample => Math.Abs((int)sample) > 500)) received.TrySetResult();
            }
        };
        await route.PrepareAsync();
        Check(route.Offer.Contains("opus/48000", StringComparison.OrdinalIgnoreCase) && route.Offer.Contains("m=application"), "native offer includes audio and realtime data channel");
        Check(!route.IsOpen && route.Status.SourceMode == "webrtc", "digital route starts suspended without devices");
        Check(remote.setRemoteDescription(new RTCSessionDescriptionInit { type = RTCSdpType.offer, sdp = route.Offer }) == SetDescriptionResultEnum.OK,
            "peer accepts actual native SDP");
        var answer = remote.createAnswer(null);
        await remote.setLocalDescription(answer);
        await route.AcceptAsync(remote.localDescription.sdp.ToString());
        await remoteConnected.Task.WaitAsync(TimeSpan.FromSeconds(10));
        route.Resume();
        var tone = Enumerable.Range(0, 960).Select(index => (float)(.25 * Math.Sin(index * Math.PI / 24))).ToArray();
        var shortTone = tone.Select(value => (short)(value * short.MaxValue)).ToArray();
        ushort sequence = 40000; uint timestamp = 123000;
        void SendTone(bool lost = false) {
            lock (audio) {
                var payload = audio.EncodeAudio(shortTone, format);
                if (!lost) remote.SendRtpRaw(SDPMediaTypesEnum.audio, payload, timestamp, 0, format.FormatID, sequence);
                sequence++; timestamp += 960;
            }
        }
        // The first authenticated audio packet must wait for a short playout reserve.
        // Otherwise a following packet delayed by only one sender tick creates silence.
        SendTone(); SendTone();
        var firstDeadline = DateTime.UtcNow.AddSeconds(2);
        while (route.Status.CaptureQueuedSamples == 0 && DateTime.UtcNow < firstDeadline) await Task.Delay(5);
        Check(route.Status.CaptureQueuedSamples > 0, "authenticated remote audio reaches the playout queue");
        var firstFrame = Enumerable.Repeat(1f, 960).ToArray();
        Check(route.ReadCaptured(firstFrame) == 0 && firstFrame.All(value => value == 0),
            "WebRTC holds its first packet until a short jitter reserve is available");
        SendTone();
        await Until(() => route.Status.CaptureQueuedSamples >= 1920, "two authenticated packets reach the playout queue");
        Check(route.ReadCaptured(firstFrame) == 0 && route.Status.CaptureQueuedSamples == 1920,
            "WebRTC retains enough reserve for a lost packet followed by sender jitter");
        var deadline = DateTime.UtcNow.AddSeconds(5);
        bool incoming = false;
        while (DateTime.UtcNow < deadline && (!incoming || !received.Task.IsCompletedSuccessfully)) {
            route.WriteReceived(tone);
            SendTone();
            await Task.Delay(20);
            var output = new float[960]; route.ReadCaptured(output);
            incoming |= output.Any(value => Math.Abs(value) > .02);
        }
        Check(incoming && received.Task.IsCompletedSuccessfully, "actual DTLS/SRTP/Opus carries both PCM directions");
        route.Suspend(); route.Resume();
        SendTone(); SendTone(); SendTone();
        await Until(() => route.Status.CaptureQueuedSamples >= 2880, "loss fixture has a short initial playout reserve");
        var recovered = new float[960];
        Check(route.ReadCaptured(recovered) == 960, "loss fixture begins with a full frame");
        for (int frame = 0; frame < 60; frame++) {
            SendTone(lost: frame % 13 == 4);
            await Task.Delay(20);
            int count = route.ReadCaptured(recovered);
            Check(count == 960, $"actual encrypted packet loss keeps playout full: frame={frame}, samples={count}");
        }
        var observed = System.Text.Json.JsonSerializer.SerializeToElement(route.Observation, NativeRpc.Json);
        Check(observed.GetProperty("receive").GetProperty("missingPackets").GetInt64() == 5 &&
            observed.GetProperty("concealedSamples").GetInt64() == 4800,
            "actual DTLS/SRTP gaps reconstruct the missing Opus frames without resetting codec history");
        // Hold the two worker threads so a timer continuation cannot run. The
        // production sender must still advance its actual RTP clock at 50 Hz.
        ThreadPool.GetMinThreads(out int minWorkers, out int minIo);
        ThreadPool.GetMaxThreads(out int maxWorkers, out int maxIo);
        using var blockedWorker = new ManualResetEventSlim();
        using var releaseWorker = new ManualResetEventSlim();
        using var releasedWorker = new ManualResetEventSlim();
        try {
            Check(ThreadPool.SetMinThreads(1, minIo) && ThreadPool.SetMaxThreads(2, maxIo), "clock fixture bounds worker capacity");
            ThreadPool.QueueUserWorkItem(_ => { blockedWorker.Set(); releaseWorker.Wait(); releasedWorker.Set(); });
            Check(blockedWorker.Wait(TimeSpan.FromSeconds(2)), "clock fixture occupies the other worker");
            long Before() => System.Text.Json.JsonSerializer.SerializeToElement(route.Observation, NativeRpc.Json).GetProperty("sentPackets").GetInt64();
            long before = Before();
            Thread.Sleep(200);
            long sent = Before() - before;
            Check(sent is >= 7 and <= 13, $"WebRTC maintains its actual audio clock during worker starvation: packets={sent}");
        } finally {
            ThreadPool.SetMaxThreads(maxWorkers, maxIo); ThreadPool.SetMinThreads(minWorkers, minIo);
            releaseWorker.Set(); releasedWorker.Wait(TimeSpan.FromSeconds(2));
        }
        permitted = false;
        route.WriteReceived(tone);
        var blocked = Enumerable.Repeat(1f, 960).ToArray();
        Check(route.ReadCaptured(blocked) == 0 && blocked.All(value => value == 0), "revoked call cannot pass or replay audio");
        route.Suspend(); permitted = true;
        route.Resume();
        Check(route.ReadCaptured(blocked) == 0 && blocked.All(value => value == 0), "new authority does not replay queued Voice audio");
        await route.DisposeAsync();
        Check(route.Status.State == "closed" && !route.IsOpen, "native realtime peer and queues close with call");
        remote.close();
    }
}
