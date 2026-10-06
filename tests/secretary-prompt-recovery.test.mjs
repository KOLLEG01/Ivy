import test from 'node:test';
import assert from 'node:assert/strict';
import { registerHooks } from 'node:module';
import { IvyError } from '../dist/packages/sdk/src/node.js';
import { triageFixture } from './secretary-triage-fixture.mjs';

test('a lost original turn remains recoverable after only the prompt generator changes, with no second turn', { timeout: 60000 }, async t => {
  const url = new URL('../dist/services/secretary/src/triage-native.js?earlier-prompt-generator', import.meta.url).href;
  const hook = registerHooks({ load(selected, context, next) {
    const loaded = next(selected, context);
    if (selected !== url) return loaded;
    const source = loaded.source.toString();
    assert.ok(source.includes('triagePrompt,'));
    return { ...loaded, source: source.replace('triagePrompt,', 'triagePrompt as installedPrompt,') +
      '\nfunction triagePrompt(...args) { const value = installedPrompt(...args); return { ...value, developerInstructions: value.developerInstructions + "\\nEarlier generator instructions." }; }\n' };
  } });
  let PreviousNative;
  try { ({ TriageNative: PreviousNative } = await import(url)); } finally { hook.deregister(); }
  const f = await triageFixture(t), item = await f.capture(), original = new PreviousNative(f.f.engine, f.settings);
  const work = await original.prepare(item), plan = await original.plan(work); let lost = false;
  f.state.after = async (method, args) => {
    if (!lost && method === 'tools.call' && args.qualifiedName === 'codex.turn/start') {
      lost = true; f.state.cut = true; throw new IvyError('outcome_unknown', 'Original turn started but response lost.', 'unknown');
    }
  };
  await assert.rejects(original.beginTurn(plan), { code: 'outcome_unknown' }); assert.equal(lost, true);
  await f.restart(); f.state.after = null;
  const recovered = await f.worker.native.plan(work);
  assert.deepEqual(recovered.value.nativeParams, plan.value.nativeParams);
  assert.match(recovered.value.developerInstructions, /Earlier generator instructions\.$/);
  assert.equal(await f.worker.step(item), 'assessed');
  assert.equal(f.state.dispatches.filter(call => call.method === 'turn/start').length, 1);
  assert.equal(f.state.dispatches.filter(call => call.method === 'thread/start').length, 1);
  assert.equal(f.state.dispatches.filter(call => call.method === 'thread/unsubscribe').length, 0);
});
