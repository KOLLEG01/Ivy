using System.Globalization;
using SIPSorceryMedia.Abstractions;

namespace Ivy.PhoneBridge;

// One fixed EVS Primary sender profile, with independently bounded receive capabilities.
// SDP rules: ETSI TS126445 AnnexA.3. A refused EVS format can still use an agreed other codec.
internal static class EvsFormat {
    internal const string OfferParameters = "br=5.9-24.4;bw=nb-swb;max-red=0";
    internal static AudioFormat Offered => new(126, "EVS", 32000, 16000, 1, OfferParameters);
    internal static bool IsEvs(AudioFormat format) => string.Equals(format.FormatName, "EVS", StringComparison.OrdinalIgnoreCase);
    internal static bool Mono(string rtpmap) => string.Equals(rtpmap, "EVS/16000", StringComparison.OrdinalIgnoreCase) ||
        string.Equals(rtpmap, "EVS/16000/1", StringComparison.OrdinalIgnoreCase);
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
        internal Rate Receive() => new(Math.Max(5.9m, Low), Math.Min(24.4m, High));
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
        1 => "nb", 2 => "wb", 4 => "swb", 3 => "nb-wb", 7 => "nb-swb", _ => throw new ArgumentException("No supported EVS receive bandwidth.")
    };
    internal static bool TrySelect(string remote, bool answeringOffer, out string selected) {
        selected = null;
        try {
            var fields = Parse(remote);
            string Get(string key) => fields.GetValueOrDefault(key);
            // Match the proven IvySIP profile: omitted optional flags retain their RFC defaults.
            // Explicit modes that the packaged codec does not implement remain incompatible.
            if (Get("evs-mode-switch") is not (null or "0") || Get("dtx") is not (null or "0") ||
                Get("dtx-recv") is not (null or "0") || Get("cmr") is not (null or "-1") ||
                Get("ch-aw-recv") is not (null or "0" or "-1") || Get("max-red") is not (null or "0") ||
                Get("channels") is not (null or "1") || Get("ch-send") is not (null or "1") || Get("ch-recv") is not (null or "1") ||
                Get("hf-only") is not (null or "0" or "1")) return false;
            foreach (var prefix in new[] { "br", "bw" }) {
                string both = Get(prefix);
                bool Same(string value) => prefix == "br" ? Rates(value) == Rates(both) : Bandwidth(value) == Bandwidth(both);
                if (both != null && (Get(prefix + "-send") is string send && !Same(send) || Get(prefix + "-recv") is string recv && !Same(recv))) return false;
            }
            var remoteReceive = Rates(Get("br-recv") ?? Get("br"));
            var remoteSend = Rates(Get("br-send") ?? Get("br"));
            var receive = remoteSend.Receive();
            int remoteReceiveBandwidth = Bandwidth(Get("bw-recv") ?? Get("bw"));
            int receiveBandwidth = Bandwidth(Get("bw-send") ?? Get("bw")) & 7;
            if (!remoteReceive.Includes(24.4m) || receive.Low > receive.High || (remoteReceiveBandwidth & 4) == 0 || receiveBandwidth == 0) return false;
            if (!answeringOffer && (remoteSend.Low < 5.9m || remoteSend.High > 24.4m || (Bandwidth(Get("bw")) & 8) != 0)) return false;
            var result = new List<string>();
            bool separateRates = Get("br") == null && (Get("br-send") != null || Get("br-recv") != null);
            if (separateRates) { result.Add("br-send=24.4"); result.Add("br-recv=" + receive); }
            else {
                result.Add("br=" + receive);
                if (Get("br-send") != null) result.Add("br-recv=" + receive);
                if (Get("br-recv") != null) result.Add("br-send=" + receive);
            }
            bool separateBandwidth = Get("bw") == null && (Get("bw-send") != null || Get("bw-recv") != null);
            if (separateBandwidth) { result.Add("bw-send=swb"); result.Add("bw-recv=" + BandwidthText(receiveBandwidth)); }
            else {
                result.Add("bw=" + BandwidthText(receiveBandwidth));
                if (Get("bw-send") != null) result.Add("bw-recv=" + BandwidthText(receiveBandwidth));
                if (Get("bw-recv") != null) result.Add("bw-send=" + BandwidthText(receiveBandwidth));
            }
            result.Add("max-red=0");
            foreach (string key in new[] { "hf-only", "evs-mode-switch", "channels" }) if (Get(key) is string value) result.Add(key + "=" + value);
            if (Get("ch-send") != null) result.Add("ch-recv=1");
            if (Get("ch-recv") != null) result.Add("ch-send=1");
            selected = string.Join(';', result); return true;
        } catch (ArgumentException) { return false; }
    }
}
