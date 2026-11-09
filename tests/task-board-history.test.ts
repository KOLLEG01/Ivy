import test from 'node:test';
import assert from 'node:assert/strict';
import { readFieldHistory, type TaskHistoryFields } from '../ui/task-board-ui/src/field-history.js';
import type { Operation } from '../packages/contracts/src/generated.js';

const metadata = (revision: number) => ({ revision, createdAt: '2026-01-01T00:00:00.000Z' }) as Operation.RevisionMetadata;
const fields = (title: string, publication: unknown = null) => ({ fields: { title }, publication, lastHistory: null }) as TaskHistoryFields;
const changes = async (before: TaskHistoryFields, after: TaskHistoryFields) => before.fields.title === after.fields.title ? [] :
  [{ field: 'Title', before: before.fields.title, after: after.fields.title }];

test('field history traverses retained metadata instead of missing revision numbers', async () => {
  const reads: number[] = [];
  const result = await readFieldHistory({
    page: async () => ({ items: [metadata(1_000_000), metadata(10)], nextCursor: null, historyComplete: false }),
    fields: async revision => { reads.push(revision); return fields(String(revision)); },
    actor: async () => 'Agent', changes,
  }, new AbortController().signal);
  assert.deepEqual(reads, [1_000_000, 10]);
  assert.equal(result.partial, true);
  assert.deepEqual(result.items[0]?.changes, [{ field: 'Title', before: '10', after: '1000000' }]);
  assert.match(result.items[0]!.summary!, /retained/);
  assert.equal(result.items.some(entry => entry.summary === 'created the task'), false);
});

test('history lookahead is bounded and preserves the visible page cursor', async () => {
  const pages: (string | undefined)[] = [], reads: number[] = [];
  const result = await readFieldHistory({
    page: async cursor => {
      pages.push(cursor);
      return { items: Array.from({ length: 20 }, (_, i) => metadata((cursor ? 80 : 100) - i)),
        nextCursor: cursor ? 'older' : 'next', historyComplete: true };
    },
    fields: async revision => { reads.push(revision); return fields('same', revision < 100 ? {} : null); },
    actor: async () => null, changes,
  }, new AbortController().signal);
  assert.deepEqual(pages, [undefined, 'next']);
  assert.equal(reads.length, 40);
  assert.equal(result.nextCursor, 'next');
  assert.match(result.items[0]!.summary!, /unavailable/);
});

test('history failures remain failures and cancellation stops subsequent reads', async () => {
  const controller = new AbortController(), failure = new Error('transport unavailable');
  const source = {
    page: async () => ({ items: [metadata(8), metadata(7)], nextCursor: null, historyComplete: true }),
    fields: async (_revision: number): Promise<TaskHistoryFields | null> => { throw failure; },
    actor: async () => null, changes,
  };
  await assert.rejects(readFieldHistory(source, controller.signal), error => error === failure);
  const reads: number[] = [];
  await assert.rejects(readFieldHistory({ ...source, fields: async revision => {
    reads.push(revision); controller.abort(); return fields('unchanged');
  } }, controller.signal), { name: 'AbortError' });
  assert.deepEqual(reads, [8]);
});

test('pruned bodies do not invent creation and a retained first revision does', async () => {
  const source = {
    page: async () => ({ items: [metadata(9), metadata(8)], nextCursor: null, historyComplete: false }),
    fields: async (revision: number) => revision === 8 ? null : fields('survivor'),
    actor: async () => null, changes,
  };
  const partial = await readFieldHistory(source, new AbortController().signal);
  assert.equal(partial.items[0]?.summary, 'earlier field values are unavailable');
  const complete = await readFieldHistory({ ...source,
    page: async () => ({ items: [metadata(1)], nextCursor: null, historyComplete: true }),
  }, new AbortController().signal);
  assert.equal(complete.items[0]?.summary, 'created the task');
  assert.equal(complete.partial, false);
});
