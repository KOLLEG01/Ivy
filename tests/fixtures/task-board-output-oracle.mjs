import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';

registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const path = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(path))) return next(path.href, context);
  }
  return next(specifier, context);
}});
const {hashJson} = await import('../../packages/contracts/src/canonical.ts');
const {requireThat} = await import('../../packages/contracts/src/errors.ts');
const source = readFileSync(new URL('../../services/task-board/src/runtime/turn-evidence.ts', import.meta.url), 'utf8');
const helpers = source.slice(source.indexOf('export const pageCursor'), source.indexOf('/** Verifies read artifacts'));
const intent = readFileSync(new URL('../../services/task-board/src/runtime/native-intent.ts', import.meta.url), 'utf8');
const nativeString = intent.match(/^export const nativeString = .*;$/m)[0];
const methods = new Function('requireThat', stripTypeScriptTypes((nativeString + '\n' + helpers).replaceAll('export ', '')) +
  '\nreturn {pageCursor,turnsParams,outputParams,finalOutputText,boundedOutputSummary};')(requireThat);

const cases = [];
function add(method, args, name) {
  let expected;
  try { expected = methods[method](...args); }
  catch (error) { expected = {error: {code: error.code, message: error.message, outcome: error.outcome}}; }
  cases.push({method, args, name, expectedHash: hashJson(expected)});
}
add('pageCursor', [], 'missing cursor');
for (const value of [null, '', false, true, 0, 1, {}, [], ['cursor'], ' ', 'opaque', '\ud800', '\udfff', '💬'])
  add('pageCursor', [value], 'cursor ' + JSON.stringify(value));
for (const length of [8191, 8192, 8193]) add('pageCursor', ['a'.repeat(length)], 'cursor units ' + length);
for (const length of [4095, 4096, 4097]) add('pageCursor', ['💬'.repeat(length)], 'cursor pairs ' + length);
for (const type of [undefined, 'agentMessage', 'userMessage', 'reasoning'])
  for (const phase of [undefined, null, 'commentary', 'final_answer', 'other'])
    for (const text of [undefined, null, 17, '', 'hello\ud800💬'])
      add('finalOutputText', [{type, phase, text}], 'item ' + [type, phase, text].join('/'));
for (const text of ['', 'plain', '\ud800', '\udfff', '💬', '\ud800\ud800\udfff'])
  add('boundedOutputSummary', [text], 'short text ' + JSON.stringify(text));
for (const unit of ['a', 'é', '€', '💬', '\ud800', '\udfff']) {
  const count = Math.floor(262144 / Buffer.byteLength(unit));
  for (const offset of [-1, 0, 1]) {
    const text = unit.repeat(count + offset);
    add('boundedOutputSummary', [text], 'byte threshold ' + JSON.stringify(unit) + '/' + offset);
  }
}
for (const boundary of ['\ud800', '\udfff', '💬', '\ud800\ud800', '\udfff\ud800'])
  add('boundedOutputSummary', ['a'.repeat(65535) + boundary + '€'.repeat(90000)], 'slice boundary ' + JSON.stringify(boundary));
for (const cursor of [null, '', 'opaque\ud800']) {
  add('turnsParams', ['thread\udfff', cursor], 'turn params ' + JSON.stringify(cursor));
  add('outputParams', ['thread\udfff', 'turn\ud800', cursor], 'output params ' + JSON.stringify(cursor));
}
// Keep transport ASCII: these are UTF-16 output tests, not UTF-8 parser tests.
process.stdout.write(JSON.stringify(cases).replace(/[\u007f-\uffff]/g,
  unit => '\\u' + unit.charCodeAt(0).toString(16).padStart(4, '0')));
