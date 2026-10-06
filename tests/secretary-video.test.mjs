import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, readFile, readdir, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { canonical, digest } from '../dist/packages/sdk/src/node.js';
import { VideoPreparer, loadVideoConfiguration } from '../dist/services/secretary/src/media-video.js';
import { prepareAcquiredMediaInput } from '../dist/services/secretary/src/media-document.js';
import { validateVideo } from '../dist/services/secretary/src/video-schema.js';
import { triageMedia } from '../dist/services/secretary/src/triage-media.js';
import { validateTriage } from '../dist/services/secretary/src/triage-schema.js';

const run = promisify(execFile), configPath = process.env.IVY_TEST_VIDEO_CONFIG;
const real = { timeout: 150000, skip: !configPath && 'Actual FFmpeg acceptance requires IVY_TEST_VIDEO_CONFIG.' };
const source = (bytes, mediaType = 'video/mp4') => ({ name: '../../outside; $(not-a-command).mp4', mediaType, byteLength: bytes.length, contentHash: digest(bytes) });
async function root(t) {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-secretary-video-'));
  t.after(async () => { const actual = await realpath(directory); assert.ok(relative(await realpath(tmpdir()), actual).startsWith('ivy-secretary-video-')); await rm(actual, { recursive: true, force: true }); });
  return directory;
}
async function configured(t) {
  const directory = await root(t), config = JSON.parse(await readFile(configPath, 'utf8')); validateVideo('VideoConfiguration', config);
  const decoder = new VideoPreparer(join(directory, 'work'), config); t.after(() => decoder.close());
  async function clip(name, options = {}) {
    const path = join(directory, name), duration = options.duration ?? 2;
    const args = ['-v', 'error', '-nostdin', '-f', 'lavfi', '-i', 'testsrc2=size=' + (options.size ?? '320x240') + ':rate=' + (options.rate ?? 8) + ':duration=' + duration,
      ...(options.audio === false ? [] : ['-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=16000:duration=' + duration]),
      '-c:v', options.webm ? 'libvpx' : 'mpeg4', '-threads', '1', ...(options.webm ? ['-deadline', 'realtime'] : ['-q:v', '4']),
      ...(options.audio === false ? ['-an'] : ['-c:a', 'aac', '-shortest']), path];
    await run(config.ffmpeg.path, args, { windowsHide: true, timeout: 30000, maxBuffer: 65536 }); return readFile(path);
  }
  return { directory, decoder, config, clip };
}
function projection(result) {
  assert.equal(result.kind, 'video', result.code); const value = JSON.parse(result.inputs[0].text); validateVideo('VideoProjection', value); return value;
}

test('video source/configuration failures remain explicit and integrity is checked without a decoder', async t => {
  const directory = await root(t), bytes = Buffer.from('untrusted video');
  const serviceConfigPath=join(directory,'config.json');assert.equal(await loadVideoConfiguration(serviceConfigPath), null);
  assert.equal((await prepareAcquiredMediaInput(source(bytes), bytes, 1024)).code, 'media_video_decoder_unconfigured');
  await assert.rejects(prepareAcquiredMediaInput({ ...source(bytes), contentHash: digest('other') }, bytes, 1024), { code: 'media_input_integrity_invalid' });
  await assert.rejects(prepareAcquiredMediaInput(source(bytes), bytes, 0), { code: 'media_input_limit_invalid' });
  const exe = { path: process.execPath, hash: digest(await readFile(process.execPath)) };
  const decoder = new VideoPreparer(join(directory, 'work'), { schemaVersion: 1, ffmpeg: exe, ffprobe: exe }); t.after(() => decoder.close());
  assert.equal((await decoder.prepare(source(bytes), bytes, 1024)).code, 'media_signature_mismatch');
  await assert.rejects(decoder.prepare({ ...source(bytes), byteLength: bytes.length + 1 }, bytes, 1024), { code: 'media_input_integrity_invalid' });
  const oversized = Buffer.alloc(32 * 1024 * 1024 + 1); oversized.write('ftyp', 4);
  assert.equal((await decoder.prepare(source(oversized), oversized, 1024)).code, 'media_video_too_large');
  assert.deepEqual(await readdir(directory), []);
  const configuration = { schemaVersion: 1, ffmpeg: { ...exe, path: join(directory, 'missing-decoder') }, ffprobe: exe };
  await writeFile(serviceConfigPath, JSON.stringify({schemaVersion:1,video:configuration}), { mode: 0o600 });
  await assert.rejects(loadVideoConfiguration(serviceConfigPath), { code: 'ENOENT' });
});

test('video projection cannot hide sampling, invent audio presence or reorder media offsets', () => {
  const hash = digest('original'), value = { schemaVersion: 1, representation: 'video_samples', sourceHash: hash, decoderHash: hash, probeHash: hash,
    durationSeconds: 2, width: 320, height: 240, frames: [{ inputOffset: 1, sampleTimeSeconds: 0, contentHash: hash }], audio: null,
    limitations: ['visual_frames_sampled', 'images_resized', 'subtitles_and_auxiliary_streams_not_interpreted', 'source_has_no_audio'] };
  validateVideo('VideoProjection', value);
  for (const change of [{ representation: 'complete_original_video' }, { durationSeconds: 91 }, { width: 8192 }, { frames: [] },
    { frames: [{ ...value.frames[0], inputOffset: 3 }] }, { frames: [{ ...value.frames[0], sampleTimeSeconds: 1 }] },
    { limitations: value.limitations.filter(item => item !== 'visual_frames_sampled') },
    { audio: { inputOffset: 2, sampleRate: 16000, channels: 1, startTimeSeconds: 0, durationSeconds: 2, contentHash: hash } }]) {
    assert.throws(() => validateVideo('VideoProjection', { ...value, ...change }));
  }
});

test('bounded decoder commands terminate actual noisy/hung child processes and drain private diagnostics', { timeout: 10000 }, async t => {
  const directory = await root(t), exe = { path: process.execPath, hash: digest(await readFile(process.execPath)) };
  const decoder = new VideoPreparer(directory, { schemaVersion: 1, ffmpeg: exe, ffprobe: exe }); t.after(() => decoder.close());
  await assert.rejects(decoder.command(process.execPath, ['-e', "process.stdout.write(Buffer.alloc(100000));setInterval(()=>{},1000)"], directory, 1024, 3000), { code: 'media_native_input_too_large' });
  await assert.rejects(decoder.command(process.execPath, ['-e', "setInterval(()=>{},1000)"], directory, 1024, 100), { code: 'media_video_timeout' });
  const output = await decoder.command(process.execPath, ['-e', "process.stderr.write('private attachment metadata');process.stdout.write('original')"], directory, 1024, 3000);
  assert.equal(output.toString(), 'original');
  const pending = decoder.command(process.execPath, ['-e', "setInterval(()=>{},1000)"], directory, 1024, 3000);
  await decoder.close(); await assert.rejects(pending, { code: 'media_video_cancelled' });
});

test('actual MP4 video yields ordered hashed JPEG samples and complete bounded PCM audio with recoverable identical input', real, async t => {
  const f = await configured(t), bytes = await f.clip('original.mp4'), original = source(bytes);
  let executions = 0; const command = f.decoder.command.bind(f.decoder); f.decoder.command = (...args) => { executions++; return command(...args); };
  const prepared = await prepareAcquiredMediaInput(original, bytes, 5 * 1024 * 1024, f.decoder), value = projection(prepared);
  assert.equal(value.sourceHash, digest(bytes)); assert.equal(value.decoderHash, f.config.ffmpeg.hash); assert.equal(value.probeHash, f.config.ffprobe.hash); assert.equal(value.width, 320); assert.equal(value.height, 240);
  assert.equal(value.frames.length, 8); assert.equal(prepared.inputs.length, 10); assert.ok(value.audio);
  assert.ok(value.limitations.includes('visual_frames_sampled')); assert.ok(value.limitations.includes('audio_downmixed_and_resampled'));
  for (const [index, frame] of value.frames.entries()) {
    assert.equal(frame.inputOffset, index + 1); assert.equal(frame.sampleTimeSeconds, index * value.durationSeconds / 8);
    const image = Buffer.from(prepared.inputs[frame.inputOffset].url.split(',')[1], 'base64'); assert.equal(digest(image), frame.contentHash);
    assert.equal(image.readUInt16BE(0), 0xffd8); assert.equal(image.readUInt16BE(image.length - 2), 0xffd9);
  }
  const wav = Buffer.from(prepared.inputs[value.audio.inputOffset].url.split(',')[1], 'base64');
  assert.equal(digest(wav), value.audio.contentHash); assert.equal(wav.toString('ascii', 0, 4), 'RIFF'); assert.equal(wav.readUInt32LE(40), wav.length - 44);
  assert.equal(wav.readUInt32LE(24), 16000); assert.equal(wav.readUInt16LE(22), 1); assert.ok(value.audio.durationSeconds >= 1.9 && value.audio.durationSeconds <= 2.1);
  assert.deepEqual(await readdir(join(f.directory, 'work')), []); assert.deepEqual(bytes, await readFile(join(f.directory, 'original.mp4')));
  const again = await f.decoder.prepare(original, bytes, 5 * 1024 * 1024); assert.deepEqual(again, prepared); again.inputs[0].text = 'substituted';
  const restarted = new VideoPreparer(join(f.directory, 'restart'), f.config); t.after(() => restarted.close());
  assert.deepEqual(await restarted.prepare(original, bytes, 5 * 1024 * 1024), prepared);
  assert.deepEqual(await f.decoder.prepare(original, bytes, 5 * 1024 * 1024), prepared);
  const before = executions; assert.equal((await f.decoder.prepare(original, bytes, 100)).code, 'media_native_input_too_large'); assert.equal(executions, before);
});

test('actual silent AVI/WebM preserve explicit audio absence and overflow never becomes partial semantic input', real, async t => {
  const f = await configured(t);
  for (const [name, type, webm] of [['silent.avi', 'video/x-msvideo', false], ['silent.webm', 'video/webm', true]]) {
    const bytes = await f.clip(name, { audio: false, webm }), prepared = await f.decoder.prepare(source(bytes, type), bytes, 5 * 1024 * 1024), value = projection(prepared);
    assert.equal(value.audio, null); assert.ok(value.limitations.includes('source_has_no_audio')); assert.equal(prepared.inputs.length, 9);
    const tooSmall = await f.decoder.prepare(source(bytes, type), bytes, 100); assert.equal(tooSmall.kind, 'unavailable'); assert.equal(tooSmall.code, 'media_native_input_too_large'); assert.equal('inputs' in tooSmall, false);
  }
  assert.deepEqual(await readdir(join(f.directory, 'work')), []);
});

test('actual duration and decoder identity gates reject work before semantic decoding', real, async t => {
  const f = await configured(t), bytes = await f.clip('too-long.mp4', { duration: 91, size: '32x32', rate: 1, audio: false });
  const commands = [], command = f.decoder.command.bind(f.decoder);
  f.decoder.command = (...args) => { commands.push(args[0]); return command(...args); };
  assert.equal((await f.decoder.prepare(source(bytes), bytes, 5 * 1024 * 1024)).code, 'media_video_duration_unsupported');
  assert.deepEqual(commands, [f.config.ffprobe.path]); assert.deepEqual(await readdir(join(f.directory, 'work')), []);
  const changed = new VideoPreparer(join(f.directory, 'changed'), { ...f.config, ffmpeg: { ...f.config.ffmpeg, hash: digest('different executable') } });
  t.after(() => changed.close());
  assert.equal((await changed.prepare(source(bytes), bytes, 5 * 1024 * 1024)).code, 'media_video_decoder_changed');
  assert.equal((await readdir(f.directory)).includes('changed'), false);
});

test('actual video projection enters the original attachment position without changing native frame or source identity', real, async t => {
  const f = await configured(t), bytes = await f.clip('semantic.mp4'), original = source(bytes), item = { objectId: 'original-item', revision: 1 };
  const context = { proof: { pin: { objectId: 'original-coverage', revision: 1 }, contractKey: 'secretary/media-coverage', contentHash: digest('coverage') }, item, sourceId: 'mail', gap: null,
    attachments: [{ name: original.name, mediaType: original.mediaType, manifest: { objectId: 'original-video', revision: 1 }, errorCode: null }],
    input: (index, budget) => { assert.equal(index, 0); return prepareAcquiredMediaInput(original, bytes, budget, f.decoder); } };
  const result = await triageMedia(context, item, 'mail'); validateTriage('TriageMedia', result.media);
  const attachment = result.media.attachments[0]; assert.equal(attachment.kind, 'video'); assert.equal(attachment.contentHash, digest(bytes)); assert.equal(attachment.inputIndex, 2);
  assert.equal(result.inputs.length, 11); const value = JSON.parse(result.inputs[attachment.inputIndex - 1].text); validateVideo('VideoProjection', value);
  for (const frame of value.frames) assert.equal(result.inputs[attachment.inputIndex - 1 + frame.inputOffset].type, 'image');
  assert.equal(result.inputs[attachment.inputIndex - 1 + value.audio.inputOffset].type, 'audio'); assert.ok(Buffer.byteLength(canonical(result.inputs)) < 6 * 1024 * 1024);
});
