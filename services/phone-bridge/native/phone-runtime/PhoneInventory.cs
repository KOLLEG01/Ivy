using NAudio.CoreAudioApi;

namespace Ivy.PhoneBridge;

public static class PhoneInventory {
    // Enumeration opens no capture/player and does not alter defaults, volume or routing.
    public static object Read() {
        using var enumerator = new MMDeviceEnumerator();
        var devices = new List<object>();
        foreach (var device in enumerator.EnumerateAudioEndPoints(DataFlow.All, DeviceState.All)) {
            using (device) {
                if (devices.Count >= 32) throw new InvalidOperationException("Audio inventory exceeds its bounded response.");
                string id = device.ID, name = device.FriendlyName;
                if (id.Length > 1024 || name.Length > 256) throw new InvalidOperationException("Audio endpoint metadata exceeds bounds.");
                bool? muted = null;
                if (device.State == DeviceState.Active) {
                    try { muted = device.AudioEndpointVolume.Mute; }
                    catch { /* Unavailable volume observation must not imply unmuted/ready. */ }
                }
                devices.Add(new { id, name, direction = device.DataFlow == DataFlow.Capture ? "capture" : "render", state = device.State.ToString(), muted });
            }
        }
        var formats = new CodecSettings(["G722", "PCMA", "PCMU", "OPUS", "EVS"]).Formats();
        return new { devices, codecs = formats.Select(format => new { name = format.FormatName, payload = format.FormatID,
            pcmRate = format.ClockRate, rtpClockRate = format.RtpClockRate, channels = format.ChannelCount }).ToArray(),
            processLoopbackSupported = OperatingSystem.IsWindowsVersionAtLeast(10, 0, 20348) };
    }
}
