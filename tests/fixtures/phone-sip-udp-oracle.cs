using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Globalization;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Ivy.PhoneBridge;
using SIPSorcery.SIP;

public static class PhoneSipUdpOracle {
    static readonly FieldInfo CacheField=typeof(SIPUDPChannel).GetField("m_sendFailures",BindingFlags.Static|BindingFlags.NonPublic);
    static readonly ConcurrentDictionary<IPEndPoint,DateTime> Cache=(ConcurrentDictionary<IPEndPoint,DateTime>)CacheField.GetValue(null);
    static IPEndPoint Peer(string text) {var endpoint=IPEndPoint.Parse(text);return endpoint.Port==0 ? endpoint : new SIPEndPoint(SIPProtocolsEnum.udp,endpoint).GetIPEndPoint();}
    static SIPUDPChannel Channel(Socket socket) {
        // The actual SDK SendAsync/EndSendTo and expiry methods run without
        // starting a receive loop, SIP agent, account or background timer.
        var channel=(SIPUDPChannel)RuntimeHelpers.GetUninitializedObject(typeof(SIPUDPChannel));
        typeof(SIPUDPChannel).GetField("m_udpSocket",BindingFlags.Instance|BindingFlags.NonPublic).SetValue(channel,socket);
        return channel;
    }
    static Socket Socket() {var value=new Socket(AddressFamily.InterNetwork,SocketType.Dgram,ProtocolType.Udp);value.Bind(new IPEndPoint(IPAddress.Loopback,0));return value;}
    static int Send(SIPUDPChannel channel,IPEndPoint peer) => (int)channel.SendAsync(new SIPEndPoint(SIPProtocolsEnum.udp,peer),new byte[]{13,10,13,10},false,null).GetAwaiter().GetResult();
    static object Admission() {
        Cache.Clear();using var first=Socket();using var second=Socket();using var peer=Socket();
        var a=Channel(first);var b=Channel(second);var remote=(IPEndPoint)peer.LocalEndPoint;
        Cache.TryAdd(remote,DateTime.Now);var codes=new List<object>{Send(a,remote),Send(b,remote)};
        first.Dispose();codes.Add(Send(a,remote));Cache.TryRemove(remote,out _);codes.Add(Send(a,remote));
        try{a.SendAsync(new SIPEndPoint(SIPProtocolsEnum.udp,remote),Array.Empty<byte>(),false,null);codes.Add("missing_error");}
        catch(ArgumentException){codes.Add("invalid_buffer");}
        codes.Add(Send(b,remote));
        var bytes=new byte[8];EndPoint source=new IPEndPoint(IPAddress.Any,0);peer.ReceiveTimeout=2000;
        int count=peer.ReceiveFrom(bytes,ref source);
        bool sameSocket=source.Equals(second.LocalEndPoint)&&count==4&&bytes[0]==13&&bytes[1]==10&&bytes[2]==13&&bytes[3]==10;
        codes.Add(Send(b,new IPEndPoint(IPAddress.IPv6Loopback,9)));
        return new { codes,sameSocket,cacheCount=Cache.Count };
    }
    static object Sweep() {
        Cache.Clear();using var socket=Socket();using var stopped=new CancellationTokenSource();stopped.Cancel();
        var channel=Channel(socket);
        typeof(SIPUDPChannel).GetField("m_cts",BindingFlags.Instance|BindingFlags.NonPublic).SetValue(channel,stopped);
        var old=Peer("127.0.0.1:21001");var fresh=Peer("127.0.0.1:21002");var future=Peer("127.0.0.1:21003");
        Cache.TryAdd(old,DateTime.Now.AddSeconds(-31));Cache.TryAdd(fresh,DateTime.Now.AddSeconds(-29));Cache.TryAdd(future,DateTime.Now.AddSeconds(10));
        Exception error=null;
        var worker=new Thread(()=>{try{typeof(SIPUDPChannel).GetMethod("ExpireFailedSends",BindingFlags.Instance|BindingFlags.NonPublic).Invoke(channel,null);}catch(Exception value){error=value;}});
        worker.IsBackground=true;worker.Start();if(!worker.Join(2000))throw new InvalidOperationException("Original expiry did not stop.");if(error!=null)throw error;
        return new { oldRemoved=!Cache.ContainsKey(old),freshRetained=Cache.ContainsKey(fresh),futureRetained=Cache.ContainsKey(future) };
    }
    static object ReceiveFailure() {
        using var socket=Socket();var completed=new TaskCompletionSource<object>(TaskCreationOptions.RunContinuationsAsynchronously);
        EndPoint initial=new IPEndPoint(IPAddress.Any,0);var bytes=new byte[8];
        socket.BeginReceiveFrom(bytes,0,bytes.Length,SocketFlags.None,ref initial,result=>{
            EndPoint peer=new IPEndPoint(IPAddress.Any,0);
            try{socket.EndReceiveFrom(result,ref peer);completed.SetResult(new {kind="success",peerPort=((IPEndPoint)peer).Port});}
            catch(SocketException){completed.SetResult(new {kind="socket_error",peerPort=((IPEndPoint)peer).Port});}
            catch(ObjectDisposedException){completed.SetResult(new {kind="disposed",peerPort=((IPEndPoint)peer).Port});}
            catch(Exception error){completed.SetException(error);}
        },null);
        socket.Dispose();if(!completed.Task.Wait(2000))throw new InvalidOperationException("Original receive completion unavailable.");
        return completed.Task.GetAwaiter().GetResult();
    }
    public static void Run() {
        using var input=JsonDocument.Parse(Console.In.ReadToEnd());Cache.Clear();var rows=new List<object>();var expired=new List<IPEndPoint>();
        foreach(var action in input.RootElement.EnumerateArray()) {
            string op=action.GetProperty("op").GetString();var now=new DateTime(long.Parse(action.GetProperty("ticks").GetString(),CultureInfo.InvariantCulture));
            IPEndPoint peer=action.TryGetProperty("peer",out var p)&&p.ValueKind!=JsonValueKind.Null ? Peer(p.GetString()) : null;
            if(op=="record"&&peer!=null&&peer.Port!=0)Cache.TryAdd(peer,now);
            if(op=="remove")Cache.TryRemove(peer,out _);
            if(op=="snapshot"||op=="sweep") {expired.Clear();foreach(var item in Cache)if(now.Subtract(item.Value).TotalSeconds>30)expired.Add(item.Key);}
            if(op=="apply"||op=="sweep")foreach(var key in expired)Cache.TryRemove(key,out _);
            string stored=peer!=null&&Cache.TryGetValue(peer,out var at) ? at.Ticks.ToString(CultureInfo.InvariantCulture) : null;
            rows.Add(new {count=Cache.Count,blocked=peer!=null&&Cache.ContainsKey(peer),stored});
        }
        var output=new {rows,admission=Admission(),sweep=Sweep(),receiveFailure=ReceiveFailure(),localUtcOffsetTicks=TimeZoneInfo.Local.GetUtcOffset(DateTimeOffset.Now).Ticks.ToString(CultureInfo.InvariantCulture)};
        Cache.Clear();Console.Write(JsonSerializer.Serialize(output,NativeRpc.Json));
    }
}
