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
    static (short[] original, short[] limited, int[] wide) WideG722(AudioEncoder decoder, SIPSorceryMedia.Abstractions.AudioFormat format, byte[] packet) {
        // Test-only observation of the sealed original. It still performs all
        // ADPCM, predictor and history updates. Recompute only the two linear
        // QMF dot products from its actual history/table, validating every
        // unmodified signed narrowing before projecting the approved limit.
        var flags=BindingFlags.Instance|BindingFlags.Static|BindingFlags.Public|BindingFlags.NonPublic;
        var field=decoder.GetType().GetField("_g722DecoderState",flags);
        var coefficients=(int[])typeof(G722Codec).GetField("qmf_coeffs",flags).GetValue(null);
        var legacy=new short[packet.Length*2];var limited=new short[legacy.Length];var wide=new int[legacy.Length];
        for(int i=0;i<packet.Length;i++) {
            var pair=decoder.DecodeAudio(new[]{packet[i]},format);
            var state=(G722CodecState)field.GetValue(decoder);
            if(state.ItuTestMode||state.EncodeFrom8000Hz||state.Packed||state.BitsPerSample!=8||pair.Length!=2)throw new InvalidOperationException("Unexpected original QMF mode.");
            var history=state.QmfSignalHistory;
            long[] sums=Enumerable.Range(0,2).Select(phase=>Enumerable.Range(0,12).Sum(tap=>(long)history[2*tap+phase]*coefficients[phase==0?tap:11-tap])).ToArray();
            int[] values={checked((int)(sums[1]>>11)),checked((int)(sums[0]>>11))};
            for(int channel=0;channel<2;channel++) {
                int index=2*i+channel, value=values[channel];
                if(unchecked((short)value)!=pair[channel])throw new InvalidOperationException("Original QMF projection disagrees with its unchanged output.");
                legacy[index]=pair[channel];wide[index]=value;limited[index]=(short)Math.Clamp(value,short.MinValue,short.MaxValue);
            }
        }
        return (legacy,limited,wide);
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
                    var unchanged=new string[frames.Length];var widened=new int[frames.Length][];
                    for(int i=0;i<frames.Length;i++) {
                        if(resets.Contains(i)){original.Dispose();original=new AudioEncoder(includeOpus:codec=="OPUS");decoder?.Dispose();decoder=null;}
                        if(op=="encode") output[i]=Convert.ToHexString(original.EncodeAudio(Samples(frames[i].GetString()),format)).ToLowerInvariant();
                        else if(codec=="G722") {
                            if(op=="decode_wide") {var result=WideG722(original,format,Convert.FromHexString(frames[i].GetString()));output[i]=Hex(result.limited);unchanged[i]=Hex(result.original);widened[i]=result.wide;}
                            else output[i]=Hex(original.DecodeAudio(Convert.FromHexString(frames[i].GetString()),format));
                        }
                        else {
                            var frame=frames[i];var packet=Convert.FromHexString(frame.GetProperty("packet").GetString());
                            int maximum=frame.GetProperty("samples").GetInt32();bool fec=frame.GetProperty("fec").GetBoolean();
                            decoder??=OpusCodecFactory.CreateDecoder(48000,1);
                            var pcm=new short[maximum];int count=decoder.Decode(packet,pcm,maximum,fec);
                            output[i]=Hex(pcm.AsSpan(0,count).ToArray());
                        }
                    }
                    if(op=="decode_wide") Reply(new{frames=output,originalFrames=unchanged,wideFrames=widened,originalNarrowingVerified=true});
                    else Reply(new{frames=output});
                } finally { original.Dispose();decoder?.Dispose(); }
            }catch(Exception error){Reply(new{error=error.GetType().Name});}
        }
    }
}
