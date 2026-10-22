import {registerHooks} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier, context, next) {
  if (specifier.endsWith('.js') && specifier.startsWith('.') && context.parentURL) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const {newOperationId, scopedOperationId} = await import('../../packages/sdk/src/client.ts');
const {IvyError} = await import('../../packages/contracts/src/errors.ts');
crypto.randomUUID = () => '11111111-1111-4111-8111-111111111111';
const capture = async action => {
  try { return {ok: true, value: await action()}; }
  catch (error) { return {ok: false, error: IvyError.from(error).toWire()}; }
};
const result = [];
for (const row of JSON.parse(readFileSync(0, 'utf8'))) {
  Date.now = () => row.time;
  const trace = [];
  const client = {async request(method, params) {
    trace.push({method, params});
    if (trace.length > 1) return {runtimeEpoch: 'second'};
    if (row.failure) throw new IvyError('service_unavailable', 'offline\ud800', row.failure, {unit: '\udfff'});
    return row.status;
  }};
  const first = await capture(() => newOperationId(client, row.nonce));
  const second = await capture(() => newOperationId(client, row.nonce));
  const scoped = await capture(() => scopedOperationId(client, 'scope'));
  const repeated = await capture(() => scopedOperationId(client, 'scope'));
  result.push({name: row.name, first, second, scoped, repeated, trace});
}
process.stdout.write(JSON.stringify(result));
