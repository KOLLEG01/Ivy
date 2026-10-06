export interface DecodedAudio {
  channelData: Float32Array[];
  sampleRate: number;
}

export const CODEX_AUDIO_SAMPLE_RATE_HERTZ = 16_000;

export function decodedAudioDurationSeconds(audio: DecodedAudio): number {
  validateDecodedAudio(audio);
  return frameCount(audio) / audio.sampleRate;
}

export function encodeMonoPcm16Wav(
  audio: DecodedAudio,
  targetSampleRate = CODEX_AUDIO_SAMPLE_RATE_HERTZ
): Buffer {
  validateDecodedAudio(audio);
  if (!Number.isInteger(targetSampleRate) || targetSampleRate < 8_000 || targetSampleRate > 192_000) {
    throw new Error("Target audio sample rate is invalid");
  }

  const sourceFrames = frameCount(audio);
  const outputFrames = Math.max(1, Math.ceil(sourceFrames * targetSampleRate / audio.sampleRate));
  const dataBytes = outputFrames * 2;
  const wav = Buffer.allocUnsafe(44 + dataBytes);
  wav.write("RIFF", 0, "ascii");
  wav.writeUInt32LE(36 + dataBytes, 4);
  wav.write("WAVE", 8, "ascii");
  wav.write("fmt ", 12, "ascii");
  wav.writeUInt32LE(16, 16);
  wav.writeUInt16LE(1, 20);
  wav.writeUInt16LE(1, 22);
  wav.writeUInt32LE(targetSampleRate, 24);
  wav.writeUInt32LE(targetSampleRate * 2, 28);
  wav.writeUInt16LE(2, 32);
  wav.writeUInt16LE(16, 34);
  wav.write("data", 36, "ascii");
  wav.writeUInt32LE(dataBytes, 40);

  const sourceStep = audio.sampleRate / targetSampleRate;
  for (let outputIndex = 0; outputIndex < outputFrames; outputIndex += 1) {
    const sourcePosition = Math.min(sourceFrames - 1, outputIndex * sourceStep);
    const leftIndex = Math.floor(sourcePosition);
    const rightIndex = Math.min(sourceFrames - 1, leftIndex + 1);
    const fraction = sourcePosition - leftIndex;
    let sample = 0;
    for (const channel of audio.channelData) {
      const left = finiteSample(channel[leftIndex]);
      const right = finiteSample(channel[rightIndex]);
      sample += left + (right - left) * fraction;
    }
    sample = Math.max(-1, Math.min(1, sample / audio.channelData.length));
    const pcm = sample < 0 ? Math.round(sample * 32_768) : Math.round(sample * 32_767);
    wav.writeInt16LE(pcm, 44 + outputIndex * 2);
  }
  return wav;
}

function validateDecodedAudio(audio: DecodedAudio): void {
  if (!Number.isFinite(audio.sampleRate) || audio.sampleRate <= 0) {
    throw new Error("Decoded audio sample rate is invalid");
  }
  if (audio.channelData.length === 0 || frameCount(audio) === 0) {
    throw new Error("Decoded audio contains no samples");
  }
}

function frameCount(audio: DecodedAudio): number {
  return audio.channelData.reduce(
    (minimum, channel) => Math.min(minimum, channel.length),
    Number.POSITIVE_INFINITY
  );
}

function finiteSample(value: number | undefined): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}
