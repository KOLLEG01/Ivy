import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
const root = resolve(process.env.IVY_SECRETARY_ORACLE ?? '.local/build/secretary');
const load = name => import(pathToFileURL(resolve(root, name)));
const { triageMedia } = await load('services/secretary/src/triage-media.js');
const { prepareMediaInput } = await load('services/secretary/src/media-input.js');
const { digest, hashJson } = await load('packages/sdk/src/node.js');
const rows = [];
for (const mode of ['empty', 'gap', 'missing-manifest', 'missing-error', 'mixed', 'video', 'wrong-source', 'missing-input', 'wrong-item']) {
  const item = { objectId: 'item', revision: 1 }, budgets = [];
  const sources = [
    { name: 'photo\ud800.png', mediaType: 'image/png', bytes: Buffer.concat([Buffer.from('\x89PNG\r\n\x1a\n', 'binary'), Buffer.alloc(24)]) },
    { name: 'voice.ogg', mediaType: 'audio/ogg', bytes: Buffer.concat([Buffer.from('OggS'), Buffer.alloc(27)]) },
    { name: 'large.txt', mediaType: 'text/plain', bytes: Buffer.alloc(6 * 1024 * 1024, 'x') },
    { name: 'after.txt', mediaType: 'text/plain', bytes: Buffer.from('complete') },
  ];
  const context = {
    proof: { pin: { objectId: 'coverage', revision: 1 }, contractKey: 'secretary/media-coverage', contentHash: 'sha256:' + '0'.repeat(64) },
    item, sourceId: 'mail', gap: mode === 'gap' ? 'source_gap' : null,
    attachments: ['empty', 'gap'].includes(mode) ? [] : sources.map((s, i) => ({ name: s.name, mediaType: s.mediaType,
      manifest: mode.startsWith('missing-') && mode !== 'missing-input' ? null : { objectId: 'manifest-' + i, revision: 1 },
      errorCode: mode === 'missing-manifest' ? 'source_unavailable' : null, representation: i === 1 ? 'rendered_preview' : 'original_bytes' })),
    async input(index, budget) {
      budgets.push([index, budget]);
      if (mode === 'missing-input') return null;
      const { bytes, ...metadata } = sources[index];
      const source = { ...metadata, byteLength: bytes.length, contentHash: digest(bytes) };
      if (mode === 'wrong-source') source.name = 'substituted';
      if (mode === 'video' && index === 0) return { kind: 'video', source,
        inputs: [{ type: 'text', text: 'frame 1', text_elements: [] }, { type: 'text', text: 'frame 2', text_elements: [] }] };
      return prepareMediaInput(source, bytes, budget);
    },
  };
  let expected;
  try { const result = await triageMedia(context, mode === 'wrong-item' ? { ...item, revision: 2 } : item, 'mail');
    expected = { media: result.media, inputsHash: hashJson(result.inputs), inputCount: result.inputs.length, budgets }; }
  catch (error) { expected = { code: error.code, budgets }; }
  rows.push({ mode, expected });
}
process.stdout.write(JSON.stringify(rows));
