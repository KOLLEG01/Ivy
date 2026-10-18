import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { IvyError, requireThat } from '../../packages/contracts/src/errors.ts';

// Execute the retained method itself without launching its production owner.
const source = readFileSync(new URL('../../services/phone-bridge/src/runtime/codex-voice.ts', import.meta.url), 'utf8');
const begin = source.indexOf('  private async call(');
const end = source.indexOf('  private input(', begin);
if (begin < 0 || end < 0) throw Error('Voice call oracle boundary changed');
const code = stripTypeScriptTypes(`class PhoneCodexVoice { ${source.slice(begin, end)} }`);
const Voice = new Function('IvyError', 'requireThat', `${code}; return PhoneCodexVoice`)(IvyError, requireThat);
const trace = [];
for (const method of ['model/list', 'thread/realtime/appendText', 'thread/read']) {
  const voice = new Voice();
  voice.schemas = {validate(schema, params, limit) { trace.push({method, limit}); }};
  voice.connection = {rpc: {connected: true, catalog: {clientRequests: [{method, inputSchema: {}}]},
    async request(actual, params, hooks, timeout) { trace.push({method: actual, timeout}); return {result: {ok: true}}; }}};
  await voice.call(method, {}, {});
}
process.stdout.write(JSON.stringify(trace));
