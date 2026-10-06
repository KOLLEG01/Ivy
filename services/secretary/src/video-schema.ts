import { SchemaValidators } from '../../../packages/sdk/src/node.js';
import schema from '../../../specs/schemas/secretary-video.schema.json' with { type: 'json' };
import { need } from './schema.js';

export interface VideoExecutable { path: string; hash: string }
export interface VideoConfiguration { schemaVersion: 1; ffmpeg: VideoExecutable; ffprobe: VideoExecutable }
export interface VideoProjection {
  schemaVersion: 1; representation: 'video_samples'; sourceHash: string; decoderHash: string; probeHash: string;
  durationSeconds: number; width: number; height: number;
  frames: { inputOffset: number; sampleTimeSeconds: number; contentHash: string }[];
  audio: { inputOffset: number; sampleRate: 16000; channels: 1; startTimeSeconds: number; durationSeconds: number; contentHash: string } | null;
  limitations: ('visual_frames_sampled' | 'images_resized' | 'subtitles_and_auxiliary_streams_not_interpreted' | 'audio_downmixed_and_resampled' | 'source_has_no_audio')[];
}
const validators = new SchemaValidators();
export function validateVideo(name: 'VideoConfiguration' | 'VideoProjection', value: unknown): void {
  validators.validate({ ...schema, $ref: '#/$defs/' + name }, value);
  if (name === 'VideoProjection') {
    const projection = value as VideoProjection;
    need(['visual_frames_sampled', 'images_resized', 'subtitles_and_auxiliary_streams_not_interpreted', projection.audio ? 'audio_downmixed_and_resampled' : 'source_has_no_audio']
      .every(item => (projection.limitations as string[]).includes(item)) && !projection.limitations.includes(projection.audio ? 'source_has_no_audio' : 'audio_downmixed_and_resampled'),
      'media_video_projection_invalid', 'Video context must declare its actual sampling and audio limitations.');
    need(projection.frames.every((frame, index) => frame.inputOffset === index + 1 && frame.sampleTimeSeconds === index * projection.durationSeconds / 8) &&
      (!projection.audio || projection.audio.inputOffset === projection.frames.length + 1 && projection.audio.startTimeSeconds + projection.audio.durationSeconds <= projection.durationSeconds + 0.1),
      'media_video_projection_invalid', 'Projected media offsets and times must remain in the original bounded timeline.');
  }
}
export const videoFormats = new Map([['video/mp4', 'mov'], ['video/quicktime', 'mov'], ['video/webm', 'matroska'], ['video/x-matroska', 'matroska'], ['video/x-msvideo', 'avi']]);
