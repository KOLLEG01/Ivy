import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const { SecretaryStore } = await import(pathToFileURL(resolve(process.env.IVY_SECRETARY_ORACLE ?? 'dist', 'services/secretary/src/store.js')).href);
const [mode, dir] = process.argv.slice(2);
const store = new SecretaryStore({ request() { throw Error('Unexpected external call'); } }, 'fixture-root', dir);
try {
  if (mode === 'create') store.technicalCreate('fixture', 'same', { phase:'running', text:'\ud800', nested:{x:1} });
  else {
    assert.deepEqual(store.technicalNamed('fixture','same').value, {phase:'succeeded',text:'\udc00',nested:{x:2}});
    assert.deepEqual(store.technicalBlob('fixture','binary').bytes,Buffer.from([0,1,254,255]));
    assert.equal(store.db.prepare('PRAGMA user_version').get().user_version,0);
  }
} finally { store.close(); }
