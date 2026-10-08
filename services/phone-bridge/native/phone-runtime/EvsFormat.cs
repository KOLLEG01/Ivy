using System.Globalization;
using SIPSorceryMedia.Abstractions;

namespace Ivy.PhoneBridge;

// EVS Primary CBR profiles with independently negotiated send/receive constraints.
// SDP rules: ETSI TS126445 AnnexA.3. A refused EVS format can still use an agreed other codec.
internal static class EvsFormat {
    internal const string OfferParameters = "br=7.2-128;bw=nb-fb;max-red=0";
    internal static string Parameters(int channels) => OfferParameters + (channels == 2 ? ";channels=2;hf-only=1" : "");
    internal static AudioFormat Offered => new(126, "EVS", EvsNativeCodec.SampleRate, 16000, 1, Parameters(1));
    internal static AudioFormat OfferedStereo => new(125, "EVS", EvsNativeCodec.SampleRate, 16000, 2, Parameters(2));
    internal static bool IsEvs(AudioFormat format) => string.Equals(format.FormatName, "EVS", StringComparison.OrdinalIgnoreCase);
    internal static int Channels(string rtpmap) => rtpmap?.ToUpperInvariant() switch {
        "EVS/16000" or "EVS/16000/1" => 1, "EVS/16000/2" => 2, _ => 0
    };
    internal static bool Mono(string rtpmap) => Channels(rtpmap) == 1;
    private static Dictionary<string, string> Parse(string text) {
        var values = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        if (string.IsNullOrWhiteSpace(text)) return values;
        if (text.Length > 1024 || text.Any(char.IsControl)) throw new ArgumentException("Bounded EVS parameters required.");
        foreach (var field in text.Split(';', StringSplitOptions.RemoveEmptyEntries)) {
            var pair = field.Split('=', 2, StringSplitOptions.TrimEntries);
            if (pair.Length != 2 || pair[0].Length is < 1 or > 64 || pair[1].Length is < 1 or > 128 ||
                pair[0].Any(value => !char.IsAsciiLetterOrDigit(value) && value != '-') || !values.TryAdd(pair[0], pair[1].ToLowerInvariant()) || values.Count > 24)
                throw new ArgumentException("Unambiguous EVS parameters required.");
        }
        return values;
    }
    internal static bool HeaderFull(string text) {
        var fields = Parse(text);
        if (!fields.TryGetValue("hf-only", out string value)) return false;
        return value switch { "0" => false, "1" => true, _ => throw new ArgumentException("Invalid EVS payload mode.") };
    }
    private readonly record struct Rate(decimal Low, decimal High) {
        internal bool Includes(decimal value) => Low <= value && value <= High;
        internal Rate Receive() => new(Math.Max(7.2m, Low), Math.Min(128m, High));
        public override string ToString() => Low.ToString(CultureInfo.InvariantCulture) + (Low == High ? "" : "-" + High.ToString(CultureInfo.InvariantCulture));
    }
    private static Rate Rates(string value) {
        if (value == null) return new(5.9m, 128m);
        string[] parts = value.Split('-'); decimal[] allowed = [5.9m, 7.2m, 8m, 9.6m, 13.2m, 16.4m, 24.4m, 32m, 48m, 64m, 96m, 128m];
        if (parts.Length is < 1 or > 2 || !decimal.TryParse(parts[0], NumberStyles.AllowDecimalPoint, CultureInfo.InvariantCulture, out decimal low) || !allowed.Contains(low))
            throw new ArgumentException("Unsupported EVS bitrate range.");
        decimal high = low;
        if (parts.Length == 2 && (!decimal.TryParse(parts[1], NumberStyles.AllowDecimalPoint, CultureInfo.InvariantCulture, out high) || !allowed.Contains(high) || high <= low))
            throw new ArgumentException("Unsupported EVS bitrate range.");
        return new(low, high);
    }
    private static int Bandwidth(string value) => value switch {
        null or "nb-fb" => 15, "nb" => 1, "wb" => 2, "swb" => 4, "fb" => 8, "nb-wb" => 3, "nb-swb" => 7,
        _ => throw new ArgumentException("Unsupported EVS bandwidth.")
    };
    private static string BandwidthText(int value) => value switch {
        1 => "nb", 2 => "wb", 4 => "swb", 8 => "fb", 3 => "nb-wb", 7 => "nb-swb", 15 => "nb-fb", _ => throw new ArgumentException("No supported EVS receive bandwidth.")
    };
    private static (int Bitrate, int Bandwidth) Profile(Rate rates, int bandwidths) {
        // 5.9k SC-VBR requires DTX. Offer the complete constant-rate ladder without
        // promising discontinuous transmission or channel-aware redundancy.
        foreach (int rate in new[] { 128000, 96000, 64000, 48000, 32000, 24400, 16400, 13200, 9600, 8000, 7200 })
            for (int bandwidth = 3; bandwidth >= 0; bandwidth--)
                if (rates.Includes(rate / 1000m) && (bandwidths & (1 << bandwidth)) != 0 &&
                    !(bandwidth == 0 && rate > 24400 || bandwidth == 2 && rate < 9600 || bandwidth == 3 && rate < 16400))
                    return (rate, bandwidth);
        throw new ArgumentException("No supported EVS bitrate/bandwidth combination.");
    }
    internal static (int Bitrate, int Bandwidth) Sender(string parameters) {
        var fields = Parse(parameters);
        return Profile(Rates(fields.GetValueOrDefault("br-send") ?? fields.GetValueOrDefault("br")),
            Bandwidth(fields.GetValueOrDefault("bw-send") ?? fields.GetValueOrDefault("bw")));
    }
    internal static bool TrySelect(string remote, bool answeringOffer, out string selected, int channels = 1) {
        selected = null;
        try {
            var fields = Parse(remote);
            string Get(string key) => fields.GetValueOrDefault(key);
            // Match the proven IvySIP profile: omitted optional flags retain their RFC defaults.
            // Explicit modes that the packaged codec does not implement remain incompatible.
            if (Get("evs-mode-switch") is not (null or "0") || Get("dtx") is not (null or "0") ||
                Get("dtx-recv") is not (null or "0") || Get("cmr") is not (null or "-1") ||
                Get("ch-aw-recv") is not (null or "0" or "-1") || Get("max-red") is not (null or "0") ||
                Get("hf-only") is not (null or "0" or "1")) return false;
            if (channels is not (1 or 2)) return false;
            string channelCount = channels.ToString(CultureInfo.InvariantCulture);
            foreach (string key in new[] { "channels", "ch-send", "ch-recv" })
                if (Get(key) is string count && count != channelCount) return false;
            if (channels == 2 && (Get("hf-only") != "1" || Get("channels") != "2")) return false;
            foreach (var prefix in new[] { "br", "bw" }) {
                string both = Get(prefix);
                bool Same(string value) => prefix == "br" ? Rates(value) == Rates(both) : Bandwidth(value) == Bandwidth(both);
                if (both != null && (Get(prefix + "-send") is string send && !Same(send) || Get(prefix + "-recv") is string recv && !Same(recv))) return false;
            }
            var remoteReceive = Rates(Get("br-recv") ?? Get("br"));
            var remoteSend = Rates(Get("br-send") ?? Get("br"));
            var receive = remoteSend.Receive();
            int remoteReceiveBandwidth = Bandwidth(Get("bw-recv") ?? Get("bw"));
            int receiveBandwidth = Bandwidth(Get("bw-send") ?? Get("bw"));
            var sendProfile = Profile(remoteReceive.Receive(), remoteReceiveBandwidth);
            _ = Profile(receive, receiveBandwidth);
            if (!answeringOffer && (remoteSend.Low < 7.2m || remoteReceive.Low < 7.2m)) return false;
            var result = new List<string>();
            bool separateRates = Get("br") == null && (Get("br-send") != null || Get("br-recv") != null);
            if (separateRates) { result.Add("br-send=" + (sendProfile.Bitrate / 1000m).ToString(CultureInfo.InvariantCulture)); result.Add("br-recv=" + receive); }
            else {
                result.Add("br=" + receive);
                if (Get("br-send") != null) result.Add("br-recv=" + receive);
                if (Get("br-recv") != null) result.Add("br-send=" + receive);
            }
            bool separateBandwidth = Get("bw") == null && (Get("bw-send") != null || Get("bw-recv") != null);
            if (separateBandwidth) { result.Add("bw-send=" + BandwidthText(1 << sendProfile.Bandwidth)); result.Add("bw-recv=" + BandwidthText(receiveBandwidth)); }
            else {
                result.Add("bw=" + BandwidthText(receiveBandwidth));
                if (Get("bw-send") != null) result.Add("bw-recv=" + BandwidthText(receiveBandwidth));
                if (Get("bw-recv") != null) result.Add("bw-send=" + BandwidthText(receiveBandwidth));
            }
            result.Add("max-red=0");
            foreach (string key in new[] { "hf-only", "evs-mode-switch", "channels" }) if (Get(key) is string value) result.Add(key + "=" + value);
            if (Get("ch-send") != null) result.Add("ch-recv=" + channelCount);
            if (Get("ch-recv") != null) result.Add("ch-send=" + channelCount);
            selected = string.Join(';', result); return true;
        } catch (ArgumentException) { return false; }
    }
}
