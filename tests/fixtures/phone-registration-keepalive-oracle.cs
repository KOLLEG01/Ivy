using System;
using System.Collections.Generic;
using System.Globalization;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Threading.Tasks;
using Ivy.PhoneBridge;
using SIPSorcery.SIP;

// Exercise the original registration method with controlled transport tasks.
// No SIP agent, account, periodic timer, device or network socket is started.
public static class PhoneRegistrationKeepaliveOracle {
    // A separate owned loopback probe: the real original SDK returns the
    // local socket's failure code before any datagram can be transmitted.
    public static void SocketFailure() {
        using var channel=new SIPUDPChannel(new IPEndPoint(IPAddress.Loopback,0));
        var target=new SIPEndPoint(SIPProtocolsEnum.udp,new IPEndPoint(IPAddress.IPv6Loopback,9));
        var result=channel.SendAsync(target,new byte[]{13,10,13,10},false,null).GetAwaiter().GetResult();
        Console.Write(JsonSerializer.Serialize(new {code=(int)result,label=result.ToString()},NativeRpc.Json));
    }
    sealed class Clock : TimeProvider {
        public long Ticks;
        public override DateTimeOffset GetUtcNow() => new(Ticks,TimeSpan.Zero);
        public override long GetTimestamp() => 0;
        public override long TimestampFrequency => TimeSpan.TicksPerSecond;
    }
    sealed class Channel : SIPChannel {
        public TaskCompletionSource<SocketError> Pending;
        public SocketError Result;
        public bool Throw;
        public int Sends;
        public Channel() { ListeningIPAddress=IPAddress.Loopback;Port=5098;SIPProtocol=SIPProtocolsEnum.udp; }
        public override Task<SocketError> SendAsync(SIPEndPoint target,byte[] bytes,bool initiate,string hint) {
            if (!bytes.AsSpan().SequenceEqual(new byte[]{13,10,13,10})) throw new InvalidOperationException("Unexpected keepalive bytes.");
            Sends++;
            if (Pending!=null) return Pending.Task;
            return Throw ? Task.FromException<SocketError>(new InvalidOperationException("synthetic transport failure")) : Task.FromResult(Result);
        }
        public override Task<SocketError> SendSecureAsync(SIPEndPoint target,byte[] bytes,string name,bool initiate,string hint) => throw new NotSupportedException();
        public override bool HasConnection(string id) => false;
        public override bool HasConnection(SIPEndPoint target) => false;
        public override bool HasConnection(Uri target) => false;
        public override bool IsAddressFamilySupported(AddressFamily family) => family==AddressFamily.InterNetwork;
        public override bool IsProtocolSupported(SIPProtocolsEnum protocol) => protocol==SIPProtocolsEnum.udp;
        public override void Close() { }
        public override void Dispose() { }
    }
    static void Set(SipRegistration owner,string field,object value) => typeof(SipRegistration).GetField(field,BindingFlags.Instance|BindingFlags.NonPublic).SetValue(owner,value);
    static readonly MethodInfo Send=typeof(SipRegistration).GetMethod("SendKeepAliveAsync",BindingFlags.Instance|BindingFlags.NonPublic);
    static Task Keepalive(SipRegistration owner) => (Task)Send.Invoke(owner,null);
    static void Confirm(SipRegistration owner,Channel channel) {
        Set(owner,"local",channel.ListeningSIPEndPoint);
        Set(owner,"remote",new SIPEndPoint(SIPProtocolsEnum.udp,new IPEndPoint(IPAddress.Loopback,5099)));
        Set(owner,"state","registered");Set(owner,"responseCode",200);Set(owner,"grantedExpiry",(long?)120);
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());
        using var transport=new SIPTransport(true);using var channel=new Channel();transport.AddSIPChannel(channel);
        var clock=new Clock();
        var owner=(SipRegistration)RuntimeHelpers.GetUninitializedObject(typeof(SipRegistration));
        Set(owner,"sync",new object());Set(owner,"time",clock);Set(owner,"transport",transport);
        Set(owner,"removed",new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously));
        Set(owner,"validFor",TimeSpan.FromSeconds(120));Set(owner,"confirmedAt",0L);
        Confirm(owner,channel);
        var rows=new List<object>();
        foreach(var action in input.RootElement.EnumerateArray()) {
            string op=action.GetProperty("op").GetString();
            clock.Ticks=long.Parse(action.GetProperty("ticks").GetString(),CultureInfo.InvariantCulture);
            channel.Throw=false;channel.Result=SocketError.Success;
            if(action.TryGetProperty("code",out var code))channel.Result=(SocketError)code.GetInt32();
            if(op=="throw") channel.Throw=true;
            if(op.StartsWith("late-",StringComparison.Ordinal)) {
                channel.Pending=new(TaskCreationOptions.RunContinuationsAsynchronously);
                var pending=Keepalive(owner);
                if(op=="late-stop") {Set(owner,"stopped",true);Set(owner,"state","stopped");}
                else Confirm(owner,channel); // Same text, fresh original endpoint references.
                channel.Pending.SetResult(channel.Result);pending.GetAwaiter().GetResult();channel.Pending=null;
            } else if(op!="observe") Keepalive(owner).GetAwaiter().GetResult();
            rows.Add(new { observation=JsonSerializer.Serialize(owner.Observation,NativeRpc.Json),sends=channel.Sends });
        }
        Console.Write(JsonSerializer.Serialize(rows,NativeRpc.Json));
    }
}
