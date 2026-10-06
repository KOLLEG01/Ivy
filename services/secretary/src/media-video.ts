import { spawn } from 'node:child_process';
import { lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve } from 'node:path';
import { canonical, digest, IvyError } from '../../../packages/sdk/src/node.js';
import type { Wire } from '../../../packages/sdk/src/node.js';
import { inside, safeDirectory, samePath } from './local-files.js';
import { need } from './schema.js';
import type { MediaInputSource, PreparedMediaInput } from './media-input.js';
import { validateVideo, videoFormats } from './video-schema.js';
import type { VideoConfiguration, VideoExecutable, VideoProjection } from './video-schema.js';
import { loadServiceConfiguration } from './service-config.js';

const check = (condition: unknown, code: string) => need(condition, code, 'Video preparation requires original verified bytes and bounded local decoding.');
const maximumSource = 32 * 1024 * 1024, maximumAudio = 90 * 16000 * 2, maximumFrame = 512 * 1024;
const object = (value: unknown): Record<string, unknown> => { check(value && typeof value === 'object' && !Array.isArray(value), 'media_video_invalid'); return value as Record<string, unknown>; };
async function executable(value: VideoExecutable): Promise<void> {
  check(isAbsolute(value.path), 'media_video_decoder_invalid'); const info = await lstat(value.path);
  check(info.isFile() && !info.isSymbolicLink() && info.size > 0 && info.size <= 256 * 1024 * 1024 && samePath(await realpath(value.path), value.path), 'media_video_decoder_invalid');
  check(digest(await readFile(value.path)) === value.hash, 'media_video_decoder_changed');
}
export async function loadVideoConfiguration(path: string): Promise<VideoConfiguration | null> {
  const config=await loadServiceConfiguration(path,1024*1024,()=>new IvyError('media_video_config_unprotected','Secretary service configuration must be a bounded protected regular file.'));
  if(config?.['video']===undefined)return null;
  const value=config['video'];validateVideo('VideoConfiguration',value);
  const selected=value as VideoConfiguration;await executable(selected.ffmpeg);await executable(selected.ffprobe);return structuredClone(selected);
}

function signature(bytes: Buffer, format: string): boolean {
  if (format === 'mov') return bytes.length >= 16 && bytes.subarray(4, 8).toString('ascii') === 'ftyp';
  if (format === 'avi') return bytes.length >= 16 && bytes.subarray(0, 4).toString('ascii') === 'RIFF' && bytes.subarray(8, 12).toString('ascii') === 'AVI ';
  return bytes.length >= 8 && bytes.subarray(0, 4).equals(Buffer.from([0x1a, 0x45, 0xdf, 0xa3]));
}
function wave(pcm: Buffer): Buffer {
  check(pcm.length > 0 && pcm.length % 2 === 0 && pcm.length <= maximumAudio, 'media_video_audio_invalid');
  const header = Buffer.alloc(44); header.write('RIFF'); header.writeUInt32LE(36 + pcm.length, 4); header.write('WAVEfmt ', 8);
  header.writeUInt32LE(16, 16); header.writeUInt16LE(1, 20); header.writeUInt16LE(1, 22); header.writeUInt32LE(16000, 24);
  header.writeUInt32LE(32000, 28); header.writeUInt16LE(2, 32); header.writeUInt16LE(16, 34); header.write('data', 36); header.writeUInt32LE(pcm.length, 40);
  return Buffer.concat([header, pcm]);
}

/** Executables receive only fixed switches and owned paths, never source names or metadata text. */
export class VideoPreparer {
  readonly configuration: VideoConfiguration;
  private active: Promise<PreparedMediaInput> | null = null;
  private readonly controller = new AbortController();
  private readonly cache = new Map<string, { inputs: Record<string, Wire.Json>[]; bytes: number }>();
  private cacheBytes = 0;
  constructor(readonly workRoot: string, configuration: VideoConfiguration) {
    validateVideo('VideoConfiguration', configuration); check(isAbsolute(workRoot), 'media_video_path_invalid');
    this.configuration = Object.freeze({ ...configuration, ffmpeg: Object.freeze({ ...configuration.ffmpeg }), ffprobe: Object.freeze({ ...configuration.ffprobe }) });
  }
  async close(): Promise<void> { this.controller.abort(); await this.active?.catch(() => undefined); }
  async prepare(source: MediaInputSource, bytes: Buffer, maximumInputBytes: number): Promise<PreparedMediaInput> {
    check(bytes.length === source.byteLength && digest(bytes) === source.contentHash, 'media_input_integrity_invalid');
    check(Number.isInteger(maximumInputBytes) && maximumInputBytes > 0 && maximumInputBytes <= 6 * 1024 * 1024, 'media_input_limit_invalid');
    this.controller.signal.throwIfAborted();
    if (bytes.length > maximumSource) return { kind: 'unavailable', source: structuredClone(source), code: 'media_video_too_large' };
    const key = source.contentHash + '\0' + source.mediaType, cached = this.cache.get(key);
    if (cached) return cached.bytes > maximumInputBytes ? { kind: 'unavailable', source: structuredClone(source), code: 'media_native_input_too_large' }
      : { kind: 'video', source: structuredClone(source), inputs: structuredClone(cached.inputs) };
    check(!this.active, 'media_video_busy');
    const original = structuredClone(source), copy = Buffer.from(bytes);
    const work = this.run(original, copy); this.active = work;
    try {
      const result = await work;
      if (result.kind === 'video') {
        const size = Buffer.byteLength(canonical(result.inputs));
        while (this.cache.size && (this.cacheBytes + size > 16 * 1024 * 1024 || this.cache.size >= 8)) {
          const first = this.cache.keys().next().value!; this.cacheBytes -= this.cache.get(first)!.bytes; this.cache.delete(first);
        }
        this.cache.set(key, { inputs: structuredClone(result.inputs), bytes: size }); this.cacheBytes += size;
        if (size > maximumInputBytes) return { kind: 'unavailable', source: original, code: 'media_native_input_too_large' };
      }
      return result;
    } finally { if (this.active === work) this.active = null; }
  }
  private command(path: string, args: string[], cwd: string, maximumBytes: number, timeoutMs: number): Promise<Buffer> {
    this.controller.signal.throwIfAborted();
    return new Promise((resolveOutput, reject) => {
      const child = spawn(path, args, { cwd, shell: false, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
        env: Object.fromEntries(['SystemRoot', 'WINDIR', 'PATH', 'TEMP', 'TMP', 'TMPDIR', 'LANG'].flatMap(key => process.env[key] ? [[key, process.env[key]!]] : [])) });
      const chunks: Buffer[] = []; let size = 0, failure: string | null = null;
      const stop = (code: string) => { failure ??= code; child.kill('SIGKILL'); };
      const timer = setTimeout(() => stop('media_video_timeout'), timeoutMs);
      const aborted = () => stop('media_video_cancelled'); this.controller.signal.addEventListener('abort', aborted, { once: true });
      child.stdout.on('data', (part: Buffer) => { size += part.length; if (size > maximumBytes) stop('media_native_input_too_large'); else if (!failure) chunks.push(part); });
      // Decoder diagnostics may contain private container metadata. Do not log or retain them.
      child.stderr.resume();
      child.once('error', () => { failure ??= 'media_video_decoder_failed'; });
      child.once('close', code => {
        clearTimeout(timer); this.controller.signal.removeEventListener('abort', aborted);
        if (failure || code !== 0) reject(new IvyError(failure ?? 'media_video_decode_failed', 'The bounded local video decoder did not complete.'));
        else resolveOutput(Buffer.concat(chunks, size));
      });
    });
  }
  private async temporary(): Promise<string> {
    await mkdir(this.workRoot, { recursive: true, mode: 0o700 }); await safeDirectory(this.workRoot);
    const entries = await readdir(this.workRoot, { withFileTypes: true }); check(entries.length < 8, 'media_video_scratch_full');
    // Crashed work is retained under a strict quota; startup never guesses whether another OS child owns it.
    let used = 0;
    for (const entry of entries) {
      check(entry.isDirectory() && !entry.isSymbolicLink() && /^video-/.test(entry.name), 'media_video_path_invalid');
      const directory = join(this.workRoot, entry.name); await safeDirectory(directory); const files = await readdir(directory);
      check(files.length <= 9, 'media_video_scratch_full');
      for (const name of files) { const info = await lstat(join(directory, name)); check(info.isFile() && !info.isSymbolicLink(), 'media_video_path_invalid'); used += info.size; }
    }
    check(used + maximumSource + 8 * maximumFrame <= 256 * 1024 * 1024, 'media_video_scratch_full');
    return mkdtemp(join(this.workRoot, 'video-'));
  }
  private async run(source: MediaInputSource, bytes: Buffer): Promise<PreparedMediaInput> {
    const unavailable = (code: string): PreparedMediaInput => ({ kind: 'unavailable', source, code });
    const format = videoFormats.get(source.mediaType?.split(';', 1)[0]?.trim().toLowerCase() ?? '');
    if (!format) return unavailable('media_type_unsupported');
    if (bytes.length > maximumSource) return unavailable('media_video_too_large');
    if (!signature(bytes, format)) return unavailable('media_signature_mismatch');
    let directory: string | null = null;
    try {
      await executable(this.configuration.ffmpeg); await executable(this.configuration.ffprobe);
      directory = await this.temporary(); const inputPath = join(directory, 'source.media'); await writeFile(inputPath, bytes, { flag: 'wx', mode: 0o600 });
      const input = ['-protocol_whitelist', 'file', '-f', format, ...(format === 'mov' ? ['-enable_drefs', '0'] : [])];
      const probeBytes = await this.command(this.configuration.ffprobe.path, ['-v', 'error', '-max_alloc', '67108864', ...input,
        '-show_entries', 'format=duration,start_time:stream=index,codec_type,width,height,duration,start_time', '-of', 'json', inputPath], directory, 32768, 15000);
      const probe = object(JSON.parse(probeBytes.toString('utf8'))), container = object(probe['format']), duration = Number(container['duration']);
      check(Number.isFinite(duration) && duration > 0 && duration <= 90, 'media_video_duration_unsupported');
      check(Array.isArray(probe['streams']) && probe['streams'].length <= 8, 'media_video_streams_unsupported');
      const streams = (probe['streams'] as unknown[]).map(object), video = streams.filter(value => value['codec_type'] === 'video'), audio = streams.filter(value => value['codec_type'] === 'audio');
      check(video.length === 1 && audio.length <= 1, 'media_video_streams_unsupported');
      const width = Number(video[0]!['width']), height = Number(video[0]!['height']);
      check(Number.isInteger(width) && width >= 2 && width <= 4096 && Number.isInteger(height) && height >= 2 && height <= 4096, 'media_video_dimensions_unsupported');
      for (const stream of [...video, ...audio]) check(Number.isInteger(stream['index']) && Number(stream['index']) >= 0 && Number(stream['index']) < 8, 'media_video_streams_unsupported');
      const common = ['-v', 'error', '-nostdin', '-xerror', '-max_alloc', '67108864', '-threads', '1', ...input, '-i', inputPath, '-map_metadata', '-1', '-fflags', '+bitexact'];
      await this.command(this.configuration.ffmpeg.path, [...common, '-map', '0:' + video[0]!['index'], '-an', '-sn', '-dn', '-filter_threads', '1',
        '-vf', 'fps=fps=' + (8 / duration).toPrecision(15) + ':start_time=0:round=down,scale=480:480:force_original_aspect_ratio=decrease:force_divisible_by=2',
        '-frames:v', '8', '-c:v', 'mjpeg', '-q:v', '3', '-threads', '1', '-flags', '+bitexact', '-f', 'image2', join(directory, 'frame-%02d.jpg')], directory, 1024, 30000);
      const names = (await readdir(directory)).filter(name => /^frame-\d\d\.jpg$/.test(name)).sort(); check(names.length >= 1 && names.length <= 8, 'media_video_frames_invalid');
      const projection: VideoProjection = { schemaVersion: 1, representation: 'video_samples', sourceHash: source.contentHash, decoderHash: this.configuration.ffmpeg.hash, probeHash: this.configuration.ffprobe.hash,
        durationSeconds: duration, width, height, frames: [], audio: null,
        limitations: ['visual_frames_sampled', 'images_resized', 'subtitles_and_auxiliary_streams_not_interpreted', audio.length ? 'audio_downmixed_and_resampled' : 'source_has_no_audio'] };
      const parts: Record<string, Wire.Json>[] = [];
      for (const [index, name] of names.entries()) {
        check(name === 'frame-' + String(index + 1).padStart(2, '0') + '.jpg', 'media_video_frames_invalid');
        const path = join(directory, name), info = await lstat(path); check(info.isFile() && !info.isSymbolicLink() && info.size <= maximumFrame, 'media_video_frames_invalid');
        const image = await readFile(path); check(image.length >= 4 && image[0] === 0xff && image[1] === 0xd8 && image.at(-2) === 0xff && image.at(-1) === 0xd9, 'media_video_frames_invalid');
        projection.frames.push({ inputOffset: index + 1, sampleTimeSeconds: index * duration / 8, contentHash: digest(image) });
        parts.push({ type: 'image', url: 'data:image/jpeg;base64,' + image.toString('base64') });
      }
      if (audio.length) {
        const pcm = await this.command(this.configuration.ffmpeg.path, [...common, '-map', '0:' + audio[0]!['index'], '-vn', '-sn', '-dn', '-ac', '1', '-ar', '16000',
          '-c:a', 'pcm_s16le', '-threads', '1', '-flags', '+bitexact', '-f', 's16le', 'pipe:1'], directory, maximumAudio, 30000);
        const wav = wave(pcm), audioDuration = pcm.length / 32000, start = Number(audio[0]!['start_time'] ?? container['start_time'] ?? 0) - Number(container['start_time'] ?? 0);
        check(Number.isFinite(start) && start >= 0 && start <= duration, 'media_video_audio_invalid');
        const expectedDuration = audio[0]!['duration'];
        if (expectedDuration !== undefined) check(Number.isFinite(Number(expectedDuration)) && Math.abs(audioDuration - Number(expectedDuration)) <= 0.1, 'media_video_audio_incomplete');
        projection.audio = { inputOffset: parts.length + 1, sampleRate: 16000, channels: 1, startTimeSeconds: start, durationSeconds: audioDuration, contentHash: digest(wav) };
        parts.push({ type: 'audio', url: 'data:audio/wav;base64,' + wav.toString('base64') });
      }
      validateVideo('VideoProjection', projection);
      const inputs: Record<string, Wire.Json>[] = [{ type: 'text', text: canonical(projection), text_elements: [] }, ...parts];
      return { kind: 'video', source, inputs };
    } catch (error) {
      if (this.controller.signal.aborted) throw error;
      return unavailable(error instanceof IvyError ? error.code : 'media_video_invalid');
    } finally {
      if (directory) { const actual = await realpath(directory); check(inside(this.workRoot, actual) && relative(this.workRoot, actual).startsWith('video-'), 'media_video_path_invalid'); await rm(actual, { recursive: true, force: true }); }
    }
  }
}
