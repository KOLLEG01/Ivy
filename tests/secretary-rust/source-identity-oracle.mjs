import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
const root = resolve(process.env.IVY_SECRETARY_ORACLE ?? '.local/build/secretary');
const { catalogueAccountHash } = await import(pathToFileURL(resolve(root, 'services/secretary/src/source-identity.js')));
const cases = [null, [], '', {}, { account: null }, { account: [] }, { account: {} },
  { account: { type: 'other', email: 'user@example.test' } }, { account: { type: 'chatgpt', email: '' } },
  ...['user@example.test', 'User@Example.test', ' user@example.test ', 'é@example.test', 'e\u0301@example.test', 'user\ud800@example.test']
    .map(email => ({ account: { type: 'chatgpt', email, ignored: true }, ignored: true }))];
process.stdout.write(JSON.stringify(cases.map(value => {
  try { return { value, expected: { hash: catalogueAccountHash(value) } }; }
  catch (error) { return { value, expected: { code: error.code } }; }
})));
