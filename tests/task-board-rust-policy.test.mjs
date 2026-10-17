import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { stripTypeScriptTypes } from 'node:module';

// Execute the original domain functions with a deterministic store. Only import
// wiring is replaced; no copied policy can accidentally bless a matching bug.
async function domainModule(path) {
  const source = readFileSync(new URL(path, import.meta.url), 'utf8').replace(/^import .*;\r?\n/gm, '');
  const prelude = `const requireThat = (ok, code, message) => { if (!ok) throw Object.assign(new Error(message), { code }); };\n`;
  return import('data:text/javascript;base64,' + Buffer.from(prelude + stripTypeScriptTypes(source)).toString('base64'));
}
const { validateBlocker, checkBlocker } = await domainModule('../services/task-board/src/runtime/task-coordination.ts');
const { validateParent } = await domainModule('../services/task-board/src/runtime/task-delegation.ts');

test('Rust TaskBoard policy matches the TypeScript oracle, including read order and failure boundaries', async () => {
  const tempRoot = resolve(tmpdir());
  const directory = mkdtempSync(join(tempRoot, 'ivy-task-board-policy-'));
  try {
    const compiler = process.env.IVY_RUSTC ?? 'rustc';
    const run = (program, args, options = {}) => {
      const result = spawnSync(program, args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, ...options });
      assert.ifError(result.error);
      assert.equal(result.status, 0, result.stderr);
      return result.stdout;
    };
    const library = join(directory, 'libivy_task_board.rlib');
    const isolated = join(directory, 'policy.rs');
    writeFileSync(isolated, `#[path = ${JSON.stringify(resolve('services/task-board/rust/src/coordination.rs'))}] pub mod coordination;\n`);
    run(compiler, ['--edition=2024', '--crate-type=lib', '--crate-name=ivy_task_board', isolated, '-o', library]);
    const probe = join(directory, process.platform === 'win32' ? 'probe.exe' : 'probe');
    run(compiler, ['--edition=2024', 'tests/fixtures/task-board-rust-policy.rs', '--extern', `ivy_task_board=${library}`, '-o', probe]);
    const inputs = [], expected = [];
    const workflows = ['backlog', 'todo', 'in_progress', 'waiting', 'review', 'done', 'cancelled'];
    const wire = value => value === null ? '-' : String(value);
    const add = (input, output) => { inputs.push(input.map(wire).join('\t')); expected.push(output); };
    for (const until of ['idle', 'done']) for (const workflowState of workflows)
      for (const claim of [false, true]) for (const publication of [false, true])
        for (const phase of [null, 'prepared', 'running', 'outcome_unknown', 'completed', 'failed', 'cancelled']) {
          const store = { read: async key => ({ value: key === 'task-board/task'
            ? { taskKey: 'TASK-0001', workflowState, claim, publication, lastRun: phase ? { objectId: 'run' } : null }
            : { phase } }) };
          let satisfied = true;
          try { await checkBlocker(store, { taskId: 'task', until }); }
          catch (error) { assert.equal(error.code, 'task_board_dependency_waiting'); satisfied = false; }
          add(['blocker', until, workflowState, +claim, +publication, phase], String(satisfied));
        }
    const graph = async (kind, own, first, nodes) => {
      const reads = [], nodeMap = new Map(nodes.map(node => [node[0], node]));
      const store = { read: async (key, id) => {
        reads.push(id);
        const node = nodeMap.get(id);
        if (!node) throw Object.assign(new Error('fixture read failed'), { code: 'not_found' });
        return kind === 'graph' ? { value: { fields: { dependencies: node[1] }, ...(node[2] === null ? {} : { blocker: { taskId: node[2] } }) } }
          : { metadata: { effectivelyArchived: !!node[1] }, value: { fields: { control: node[2] ? 'agent' : 'user' }, workflowState: node[3] ? 'done' : 'waiting', ...(node[4] === null ? {} : { parentTaskId: node[4] }) } };
      } };
      let code = 'ok', message = '';
      try {
        if (kind === 'graph') await validateBlocker(store, { taskId: first, until: 'idle' }, own);
        else await validateParent(store, first, own ?? undefined);
      } catch (error) { code = error.code; message = error.message; }
      add([kind, own, first, nodes.map(node => node.map(value => Array.isArray(value) ? value.join(',') || '-' : wire(value)).join('|')).join(';')],
        [code, message, reads.join(',')].join('\t'));
    };
    // Reuse the cycle/idle regression boundary from task-board-coordination.test.ts;
    // add exact bounds, branching order, repeated nodes and unavailable evidence.
    await graph('graph', 1, 1, []);
    await graph('graph', 9, 1, [[1, [2, 3], 4], [4, [3], null], [3, [2], null], [2, [], null]]);
    await graph('graph', 9, 1, [[1, [2], null], [2, [], 1]]);
    await graph('graph', 9, 1, [[1, [2], null], [2, [], 9]]);
    await graph('graph', null, 1, [[1, [2], null]]);
    for (const length of [511, 512, 513]) {
      await graph('graph', null, 1, Array.from({ length }, (_, i) => [i + 1, i + 1 < length ? [i + 2] : [], null]));
    }
    await graph('parent', 1, 1, []);
    await graph('parent', null, 1, [[1, 0, 1, 0, 2], [2, 0, 1, 0, 1]]);
    await graph('parent', null, 1, [[1, 0, 1, 0, 2]]);
    for (const flags of [[1, 1, 0], [0, 0, 0], [0, 1, 1], [0, 1, 0]]) await graph('parent', null, 1, [[1, ...flags, null]]);
    for (const own of [null, 999]) for (const length of [63, 64, 65]) {
      await graph('parent', own, 1, Array.from({ length }, (_, i) => [i + 1, 0, 1, 0, i + 1 < length ? i + 2 : null]));
    }
    const output = run(probe, [], { input: inputs.join('\n') + '\n' }).trimEnd().split(/\r?\n/);
    assert.equal(output.length, expected.length);
    for (let i = 0; i < expected.length; i++) assert.equal(output[i], expected[i], `Mismatch at ${inputs[i]}`);
  } finally {
    assert.equal(dirname(resolve(directory)), tempRoot);
    assert.ok(basename(directory).startsWith('ivy-task-board-policy-'));
    rmSync(directory, { recursive: true, force: true });
  }
});
