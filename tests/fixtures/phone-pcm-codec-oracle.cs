using System;
using System.Linq;
using System.Reflection;
using System.Text.Json;
using Concentus;
using Ivy.PhoneBridge;
using SIPSorcery.Media;

// Actual original codec instances at their PCM clock. No media session,
// resampler, socket, capture/device, service or account is constructed here.
public static class PhonePcmCodecOracle {
    static void Reply(object value) { Console.WriteLine(JsonSerializer.Serialize(value)); Console.Out.Flush(); }
    static short[] Samples(string hex) {
        byte[] bytes=Convert.FromHexString(hex);var result=new short[bytes.Length/2];
        for(int i=0;i<result.Length;i++)result[i]=(short)(bytes[2*i]|(bytes[2*i+1]<<8));
        return result;
    }
    static string Hex(short[] pcm) {
        byte[] bytes=new byte[pcm.Length*2];
        for(int i=0;i<pcm.Length;i++){bytes[2*i]=(byte)pcm[i];bytes[2*i+1]=(byte)(pcm[i]>>8);}
        return Convert.ToHexString(bytes).ToLowerInvariant();
    }
    public static void Run() {
        while(Console.ReadLine() is string line) {
            try {
                using var json=JsonDocument.Parse(line);var input=json.RootElement;
                string codec=input.GetProperty("codec").GetString(), op=input.GetProperty("op").GetString();
                if(codec!="G722"&&codec!="OPUS")throw new ArgumentException("Synthetic codec not supported.");
                var format=new CodecSettings(new[]{codec}).Formats().Single();
                var original=new AudioEncoder(includeOpus:codec=="OPUS");
                IOpusDecoder decoder=null;
                try {
                    if(op=="profile") {
                        original.EncodeAudio(new short[960],format);
                        var field=original.GetType().GetFields(BindingFlags.Instance|BindingFlags.NonPublic|BindingFlags.Public).Single(f=>f.FieldType.Name.Contains("OpusEncoder"));
                        var encoder=field.GetValue(original);
                        object Property(string name)=>encoder.GetType().GetProperty(name).GetValue(encoder);
                        Reply(new{bitrate=Property("Bitrate"),complexity=Property("Complexity"),channels=Property("NumChannels"),sampleRate=Property("SampleRate"),fec=Property("UseInbandFEC"),loss=Property("PacketLossPercent"),vbr=Property("UseVBR"),dtx=Property("UseDTX")});
                        continue;
                    }
                    var frames=input.GetProperty("frames").EnumerateArray().ToArray();
                    int[] resets=input.TryGetProperty("resets",out var reset)?reset.EnumerateArray().Select(x=>x.GetInt32()).ToArray():Array.Empty<int>();
                    var output=new string[frames.Length];
                    for(int i=0;i<frames.Length;i++) {
                        if(resets.Contains(i)){original.Dispose();original=new AudioEncoder(includeOpus:codec=="OPUS");decoder?.Dispose();decoder=null;}
                        if(op=="encode") output[i]=Convert.ToHexString(original.EncodeAudio(Samples(frames[i].GetString()),format)).ToLowerInvariant();
                        else if(codec=="G722") output[i]=Hex(original.DecodeAudio(Convert.FromHexString(frames[i].GetString()),format));
                        else {
                            var frame=frames[i];var packet=Convert.FromHexString(frame.GetProperty("packet").GetString());
                            int maximum=frame.GetProperty("samples").GetInt32();bool fec=frame.GetProperty("fec").GetBoolean();
                            decoder??=OpusCodecFactory.CreateDecoder(48000,1);
                            var pcm=new short[maximum];int count=decoder.Decode(packet,pcm,maximum,fec);
                            output[i]=Hex(pcm.AsSpan(0,count).ToArray());
                        }
                    }
                    Reply(new{frames=output});
                } finally { original.Dispose();decoder?.Dispose(); }
            }catch(Exception error){Reply(new{error=error.GetType().Name});}
        }
    }
}
