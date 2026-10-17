import {readFileSync, existsSync} from 'node:fs';
import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
import {randomUUID} from 'node:crypto';

registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const {digest} = await import('../../packages/contracts/src/canonical.ts');
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const {managementFrameBytes} = await import('../../packages/contracts/src/limits.ts');
const source = readFileSync(new URL('../../services/task-board/src/runtime/store.ts', import.meta.url), 'utf8');
const schema = source.match(/\.exec\(`(PRAGMA journal_mode=WAL;[\s\S]*?)`\);/)[1];
const methods = source.slice(source.indexOf('  saveLocalBlob('), source.indexOf('  localKey('));
const LocalStore = new Function('requireThat', 'randomUUID', stripTypeScriptTypes(
  'class LocalStore { local; constructor(db) { this.local = db; }\n' + methods + '\n}') + '\nreturn LocalStore;')(requireThat, randomUUID);

if (process.argv[2] === 'create') {
  const db = new DatabaseSync(process.argv[3]);
  db.exec(schema);
  const store = new LocalStore(db);
  const pin = store.saveLocalBlob('key', 'name\ud800', 'parent\udfff', Buffer.from('from node'));
  process.stdout.write(JSON.stringify(pin));
  db.close();
} else if (process.argv[2] === 'inspect') {
  const db = new DatabaseSync(process.argv[3]);
  const store = new LocalStore(db);
  const pin = store.saveLocalBlob('rust-key', 'rust-name', 'rust-parent', Buffer.from('from rust'));
  const bytes = store.localBlob('rust-key', pin, 'rust-parent');
  const usage = db.prepare("SELECT rows,bytes FROM storage_usage WHERE kind='blobs'").get();
  process.stdout.write(JSON.stringify({pin, text: bytes.toString(), usage}));
  db.close();
} else {
  let evidence = readFileSync(new URL('../../services/task-board/src/runtime/evidence-bytes.ts', import.meta.url), 'utf8')
    .replace(/^import .*;\r?\n/gm, '').replace(/export /g, '');
  // Observe exactly the byte-verification prefix. Decoding/canonical validation is
  // a separate boundary and is deliberately not claimed by these differential cases.
  evidence = evidence.slice(0, evidence.indexOf('  let decoded:')) + '  return body;\n}';
  const {saveEvidenceChunks, readEvidenceChunks} = new Function('digest', 'requireThat', 'managementFrameBytes',
    stripTypeScriptTypes(evidence) + '\nreturn {saveEvidenceChunks,readEvidenceChunks};')(digest, requireThat, managementFrameBytes);
  const chunkSize = 1024 * 1024, cases = [];
  const add = async (size, mutation = '', readKey = false) => {
    const body = Buffer.alloc(size);
    for (let offset = 0, index = 0; offset < size; offset += chunkSize, index++) body.fill(65 + index % 26, offset, Math.min(size, offset + chunkSize));
    const saved = [], reads = [], values = [], key = 'task-board/native-' + (readKey ? 'read-' : '') + 'evidence-chunk';
    const store = {
      saveLocalBlob(key, name, parent, bytes) {
        if (mutation === 'save-failure' && saved.length === 1) throw new IvyError('fixture_failed', 'Interrupted save.');
        const index = saved.length;
        saved.push({key, name, parent, byteLength: bytes.length, contentHash: digest(bytes)});
        values.push(Buffer.from(bytes));
        return {objectId: String(index), revision: 1};
      },
      localBlob(key, pin, parent) {
        reads.push({key, index: Number(pin.objectId), parent});
        if (mutation === 'read-failure') throw new IvyError('fixture_failed', 'Unavailable chunk.');
        return values[Number(pin.objectId)];
      },
    };
    let result;
    try {
      const chunks = await saveEvidenceChunks(store, 'owner', body, key);
      const manifest = {byteLength: body.length, contentHash: digest(body), chunks};
      if (mutation === 'count') manifest.chunks.pop();
      if (mutation === 'empty') manifest.byteLength = 0;
      if (mutation === 'oversize') manifest.byteLength = managementFrameBytes + 1;
      if (mutation === 'length') manifest.byteLength--;
      if (mutation === 'chunk-length') chunks[0].byteLength++;
      if (mutation === 'chunk-hash') chunks[0].contentHash = 'wrong';
      if (mutation === 'body-hash') manifest.contentHash = 'wrong';
      if (mutation === 'reorder') chunks.reverse();
      if (mutation === 'bytes') values[0][0] ^= 1;
      if (mutation === 'short') values[0] = values[0].subarray(1);
      const bytes = await readEvidenceChunks(store, 'owner', manifest, key);
      result = {byteLength: bytes.length, contentHash: digest(bytes)};
    } catch (error) { result = {code: error.code, message: error.message}; }
    cases.push({size, mutation, readKey, result, saved, reads});
  };
  for (const size of [0, 1, chunkSize - 1, chunkSize, chunkSize + 1, 2 * chunkSize + 7, managementFrameBytes, managementFrameBytes + 1]) await add(size);
  for (const mutation of ['count','empty','oversize','length','chunk-length','chunk-hash','body-hash','reorder','bytes','short','save-failure','read-failure']) await add(chunkSize + 7, mutation);
  await add(chunkSize + 7, '', true);
  process.stdout.write(JSON.stringify(cases));
}
