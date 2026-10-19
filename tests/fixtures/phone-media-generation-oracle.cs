using System;
using System.Reflection;
using System.Text.Json;
using Ivy.PhoneBridge;
using SIPSorcery.Media;

// Synthetic managed codec only. No session, socket, audio device or account.
public static class PhoneMediaGenerationOracle {
    sealed class Port : IPcmAudioPort {
        public bool IsOpen => true;
        public long Generation {get;set;}=1;
        public long Samples;
        public Action OnWrite;
        public int ReadCaptured(Span<float> output){output.Clear();return output.Length;}
        public void WriteReceived(ReadOnlySpan<float> input){Samples+=input.Length;OnWrite?.Invoke();}
    }
    public static void Run() {
        var format=new CodecSettings(new[]{"OPUS"}).Formats()[0];
        using var encoder=new AudioEncoder(includeOpus:true);
        byte[] packet=encoder.EncodeAudio(new short[960],format);
        var port=new Port();using var codec=new MediaCodec(format,port);
        codec.DecodeRtp(new RtpAudioPacket(0,0,1,format.FormatID,packet,1));
        port.OnWrite=()=>{port.Generation++;port.OnWrite=null;};
        codec.DecodeRtp(new RtpAudioPacket(2,1920,1,format.FormatID,packet,1));
        var flags=BindingFlags.NonPublic|BindingFlags.Instance;
        object timestamp=typeof(MediaCodec).GetField("nextReceiveTimestamp",flags).GetValue(codec);
        long before=port.Samples,concealed=codec.ConcealedSamples;
        codec.DecodeRtp(new RtpAudioPacket(4,3840,1,format.FormatID,packet,port.Generation));
        Console.WriteLine(JsonSerializer.Serialize(new{generation=port.Generation,
            retainedTimestampAfterRevocation=timestamp,newGenerationSamples=port.Samples-before,
            newGenerationConcealedSamples=codec.ConcealedSamples-concealed,
            expectedFreshGenerationSamples=960,expectedFreshGenerationConcealedSamples=0}));
    }
}
