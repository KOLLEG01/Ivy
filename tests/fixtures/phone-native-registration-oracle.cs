using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text.Json;
using Ivy.PhoneBridge;
using SIPSorcery.SIP;

// DTO serialization and endpoint formatting only. No sockets, accounts, devices
// or source-provided secrets are used by these original managed records.
public static class PhoneNativeRegistrationOracle {
    static string Text(JsonElement input,string name) => input.GetProperty(name).ValueKind==JsonValueKind.Null ? null : input.GetProperty(name).GetString();
    static string Endpoint(JsonElement input,string name) => Text(input,name) is string value ? SIPEndPoint.ParseSIPEndPoint(value).ToString() : null;
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());var rows=new List<string>();
        foreach(var item in input.RootElement.EnumerateArray()) {
            var response=item.GetProperty("responseCode");var removed=item.GetProperty("remoteRemoved");
            var milliseconds=item.GetProperty("lastKeepAliveUnixMs");
            var value=new SipRegistrationObservation(Text(item,"state"),response.ValueKind==JsonValueKind.Null ? null : response.GetInt32(),
                removed.ValueKind==JsonValueKind.Null ? null : removed.GetBoolean(),Endpoint(item,"localEndpoint"),Endpoint(item,"remoteEndpoint"),
                Text(item,"grantedExpirySeconds") is string expiry ? long.Parse(expiry,CultureInfo.InvariantCulture) : null,
                milliseconds.ValueKind==JsonValueKind.Null ? null : DateTimeOffset.FromUnixTimeMilliseconds(milliseconds.GetInt64()),Text(item,"lastKeepAliveError"));
            rows.Add(JsonSerializer.Serialize(value,NativeRpc.Json));
        }
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}
