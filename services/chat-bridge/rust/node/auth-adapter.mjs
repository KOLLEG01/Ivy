// Auth codec and protocol-library calls only. Rust owns the database and policy.
import {BufferJSON, initAuthCreds, proto, addTransactionCapability, DEFAULT_CONNECTION_CONFIG} from 'baileys';
import {makeLibSignalRepository} from 'baileys/lib/Signal/libsignal.js';

export const encode = value => JSON.stringify(value, BufferJSON.replacer);
export const decode = value => JSON.parse(value, BufferJSON.reviver);
const sqlText = value => Buffer.from(value, 'utf8').toString('utf8');
const logger = {trace(){}, debug(){}, info(){}, warn(){}, error(){}, child(){return this;}};

export class AuthFenced extends Error {
  constructor() { super('Authentication generation is fenced.'); this.name = 'AuthFenced'; }
}

export async function createAuthAdapter(saved, raw) {
  let fenced = false;
  const check = () => { if (fenced) throw new AuthFenced(); };
  // A failure here is an uncertain owner/commit boundary. A retry by the
  // original transaction wrapper must never issue a second callback/write.
  async function callback(method, value) {
    check();
    try { const result = await raw[method](value); check(); return result; }
    catch { fenced = true; throw new AuthFenced(); }
  }
  const creds = saved ? decode(saved) : initAuthCreds();
  const underlying = {
    async get(type, ids) {
      const rows = await callback('get', ids.map(id => [sqlText(type), sqlText(id)]));
      if (!Array.isArray(rows) || rows.length !== ids.length) { fenced = true; throw new AuthFenced(); }
      const result = {};
      for (let i = 0; i < ids.length; i++) {
        if (rows[i]) {
          let value = decode(rows[i]);
          if (type === 'app-state-sync-key') value = proto.Message.AppStateSyncKeyData.fromObject(value);
          result[ids[i]] = value;
        }
      }
      return result;
    },
    async set(data) {
      const entries = [];
      for (const [category, values] of Object.entries(data)) {
        for (const [id, value] of Object.entries(values ?? {})) {
          entries.push([sqlText(category), sqlText(id), value == null ? null : encode(value)]);
        }
      }
      await callback('set', entries);
    },
  };
  const state = {creds, keys: addTransactionCapability(underlying, logger, DEFAULT_CONNECTION_CONFIG.transactionOpts)};
  const save = () => callback('set', [['creds', 'default', encode(creds)]]);
  // whatsappAuth saves synchronously on construction. With a remote Rust store,
  // complete that same full commit before reporting READY, never a fake sync DB.
  await save();
  const repo = makeLibSignalRepository(state, logger);
  const operations = Object.freeze({
    injectE2ESession: args => repo.injectE2ESession(args),
    encryptMessage: args => repo.encryptMessage(args),
    decryptMessage: args => repo.decryptMessage(args),
    processSenderKeyDistributionMessage: args => repo.processSenderKeyDistributionMessage(args),
    encryptGroupMessage: args => repo.encryptGroupMessage(args),
    decryptGroupMessage: args => repo.decryptGroupMessage(args),
    getKeys: args => state.keys.get(args.type, args.ids),
    setKeys: args => state.keys.set(args),
    credentials: () => creds,
    // This command supplies the complete encoded credential object, as returned
    // by credentials. Retain its object identity for the repository, but do not
    // resurrect an absent field from an earlier in-memory version.
    saveCredentials: async args => {
      for (const key of Object.keys(creds)) delete creds[key];
      Object.defineProperties(creds, Object.getOwnPropertyDescriptors(args)); await save();
    },
  });
  return {
    fence() { fenced = true; },
    close() { fenced = true; repo.close(); },
    async call(method, encoded) {
      check();
      if (!Object.hasOwn(operations, method)) throw new TypeError('Unsupported authentication operation.');
      const result = await operations[method](decode(encoded));
      check();
      return encode(result) ?? null;
    },
  };
}
