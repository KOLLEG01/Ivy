using System;
using System.Linq;
using System.Reflection;
using System.Text.Json;
using Ivy.PhoneBridge;
using NAudio.Dsp;

// The sealed original's own factory configures its real WDL engine. Only
// synthetic mono PCM enters it; no audio port, device or session is opened.
public static class PhoneResamplerOracle {
    static WdlResampler Create(int source, int target) => source==target ? null :
        (WdlResampler)typeof(MediaCodec).GetMethod("Resampler", BindingFlags.Static|BindingFlags.NonPublic)
            .Invoke(null, new object[]{source,target});
    static float[] Samples(string hex) {
        byte[] bytes=Convert.FromHexString(hex);var result=new float[bytes.Length/4];
        for(int i=0;i<result.Length;i++)result[i]=BitConverter.Int32BitsToSingle(
            bytes[4*i]|(bytes[4*i+1]<<8)|(bytes[4*i+2]<<16)|(bytes[4*i+3]<<24));
        return result;
    }
    static string Hex(ReadOnlySpan<float> pcm) {
        byte[] bytes=new byte[pcm.Length*4];
        for(int i=0;i<pcm.Length;i++) {
            int bits=BitConverter.SingleToInt32Bits(pcm[i]);
            for(int b=0;b<4;b++)bytes[4*i+b]=(byte)(bits>>(8*b));
        }
        return Convert.ToHexString(bytes).ToLowerInvariant();
    }
    public static void Run() {
        while(Console.ReadLine() is string line) {
            try {
                using var json=JsonDocument.Parse(line);var input=json.RootElement;
                int source=input.GetProperty("sourceRate").GetInt32(), target=input.GetProperty("targetRate").GetInt32();
                var filter=Create(source,target);
                int[] resets=input.GetProperty("resets").EnumerateArray().Select(x=>x.GetInt32()).ToArray();
                var frames=input.GetProperty("frames").EnumerateArray().ToArray();
                var output=new string[frames.Length];var counts=new int[frames.Length];
                for(int i=0;i<frames.Length;i++) {
                    if(resets.Contains(i))filter=Create(source,target);
                    var pcm=Samples(frames[i].GetString());var converted=new float[target*120/1000+16];
                    int count;
                    if(filter==null){pcm.CopyTo(converted,0);count=pcm.Length;}
                    else {
                        int wanted=filter.ResamplePrepare(pcm.Length,1,out Span<float> prepared);
                        if(wanted!=pcm.Length)throw new InvalidOperationException("Original feed contract changed.");
                        pcm.CopyTo(prepared);
                        count=filter.ResampleOut(converted,pcm.Length,converted.Length,1);
                    }
                    output[i]=Hex(converted.AsSpan(0,count));counts[i]=count;
                }
                Console.WriteLine(JsonSerializer.Serialize(new{frames=output,counts}));
            }catch(Exception error){Console.WriteLine(JsonSerializer.Serialize(new{error=error.GetType().Name}));}
            Console.Out.Flush();
        }
    }
}
