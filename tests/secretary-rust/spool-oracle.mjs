import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
const build = resolve(process.env.IVY_SECRETARY_ORACLE ?? 'dist');
const module = path => pathToFileURL(join(build, path)).href;
const { MediaSpool } = await import(module('services/secretary/src/media-spool.js'));
const { validateMedia } = await import(module('services/secretary/src/media-schema.js'));
// Reuse the existing synthetic inputs without rebuilding or changing the oracle.
const fixtureSource = (await readFile('tests/secretary-media-fixture.mjs', 'utf8'))
  .replaceAll("'../dist/packages/sdk/src/node.js'", JSON.stringify(module('packages/sdk/src/node.js')))
  .replaceAll("'../dist/services/secretary/src/media-spool.js'", JSON.stringify(module('services/secretary/src/media-spool.js')));
const { owner, intent, materialization } = await import('data:text/javascript;base64,' + Buffer.from(fixtureSource).toString('base64'));
const [mode, root] = process.argv.slice(2);
if (mode === 'fixtures') console.log(JSON.stringify({ owner, intent, materialization }));
else if (mode === 'validate') {
  let input = ''; for await (const chunk of process.stdin) input += chunk;
  const { name, value } = JSON.parse(input); validateMedia(name, value);
} else {
  const spool = await MediaSpool.open(root, owner);
  try {
    if (mode === 'create') {
      spool.prepare(intent, materialization);
      await spool.acquire(intent.operationId, new AbortController().signal, async () => {}, async () => new Response(Buffer.from([0, 1, 254, 255])));
      spool.prepare({ ...intent, operationId: 'interrupted' }, materialization);
    } else if (mode === 'verify') {
      assert.deepEqual(spool.payload(intent.operationId).bytes, Buffer.from([0, 1, 254, 255]));
      assert.equal(spool.get('interrupted').errorCode, 'media_download_interrupted');
      assert.equal(spool.get('rust-original').phase, 'failed');
      assert.equal(spool.get('rust-original').errorCode, 'media_download_cancelled');
    } else throw Error('Unknown mode');
  } finally { spool.close(); }
  if (mode === 'create') {
    const db = new DatabaseSync(join(root, 'media.sqlite'));
    db.prepare("UPDATE attempts SET phase='downloading' WHERE operation_id='interrupted'").run();
    db.close();
  }
}
