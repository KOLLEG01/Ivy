import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const { SecretaryStore } = await import(pathToFileURL(resolve(process.env.IVY_SECRETARY_ORACLE ?? 'dist', 'services/secretary/src/store.js')).href);
const [mode, dir] = process.argv.slice(2);
const store = new SecretaryStore({ request() { throw Error('Unexpected external call'); } }, 'fixture-root', dir);
const nested = count => { let value = '\ud800'; while (count--) value = [value]; return { value }; };
const wide = count => ({ value: Array(count).fill(null) });
try {
  if (mode === 'create') {
    store.technicalCreate('fixture', 'same', { phase:'running', text:'\ud800', nested:{x:1} });
    store.technicalCreate('fixture', 'deep', nested(127));
    store.technicalCreate('fixture', 'wide', wide(399998));
    for (const [name, value] of [['too-deep', nested(128)], ['too-wide', wide(399999)]]) {
      assert.throws(() => store.technicalCreate('fixture', name, value), { code: 'limit_exceeded' });
      assert.equal(store.technicalNamed('fixture', name), null);
    }
  }
  else {
    assert.deepEqual(store.technicalNamed('fixture','same').value, {phase:'succeeded',text:'\udc00',nested:{x:2}});
    assert.deepEqual(store.technicalBlob('fixture','binary').bytes,Buffer.from([0,1,254,255]));
    assert.equal(store.db.prepare('PRAGMA user_version').get().user_version,0);
    assert.deepEqual(store.technicalNamed('fixture', 'deep').value, nested(127));
    assert.deepEqual(store.technicalNamed('fixture', 'wide').value, wide(399998));
  }
} finally { store.close(); }
