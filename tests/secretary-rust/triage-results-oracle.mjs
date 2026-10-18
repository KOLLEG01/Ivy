import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
const root = resolve(process.env.IVY_SECRETARY_ORACLE ?? '.local/build/secretary');
const { triageResult, triageTurn } = await import(pathToFileURL(resolve(root, 'services/secretary/src/triage-results.js')));
// Extend the existing secretary-triage-contract fixture across identity, item and
// JSON boundaries. The oracle is the unchanged TypeScript implementation.
const assessment = (changes = {}) => ({ urgency: 'normal', disposition: 'notify', notification: 'main',
  summary: 'The agenda needs your review.', question: true, usefulIntel: true, task: null, ...changes });
const answer = (changes = {}) => ({ id: 'answer', type: 'agentMessage', phase: 'final_answer',
  text: JSON.stringify(assessment()), ...changes });
const turn = (changes = {}) => ({ id: 'original-turn', itemsView: 'full', status: 'completed', error: null,
  items: [answer()], ...changes });
const read = (changes = {}) => ({ reply: { result: { data: [turn(changes)], nextCursor: null } } });
const cases = [];
function add(label, observation = read(), mode = 'result', turnId = 'original-turn', full = true) {
  let expected;
  try { expected = { value: mode === 'turn' ? triageTurn(observation, turnId, full) : triageResult(observation, turnId) }; }
  catch (error) { expected = { code: error.code, message: error.message, outcome: error.outcome }; }
  cases.push({ label, observation, mode, turnId, full, expected });
}
add('original successful final');
for (const full of [false, true]) {
  for (const view of ['full', 'notLoaded', '', null, false]) {
    add(`view ${view}/${full}`, read({ itemsView: view }), 'turn', 'original-turn', full);
  }
}
for (const id of ['', null, false, 1, [], 'different', 'turn\ud800', 'turn😀']) {
  add(`mismatched id ${JSON.stringify(id)}`, read({ id }));
  if (typeof id === 'string') add(`exact id ${JSON.stringify(id)}`, read({ id }), 'result', id);
}
for (const result of [null, [], {}, { data: [] }, { data: [turn(), turn()] }, { data: [null] },
  { data: [{ ...turn(), id: null }] }]) add('result shape', { reply: { result } });
add('no result', { reply: { error: { code: 'failed' } } });
for (const status of ['inProgress', 'failed', 'interrupted', '', null, false]) add('status ' + status, read({ status }));
for (const error of [false, '', 0, {}, [], { message: 'failure' }]) add('non-null error', read({ error }));
const noError = read(); delete noError.reply.result.data[0].error; add('omitted error', noError);
for (const items of [null, {}, '', [], [null], [false], [[]], [answer(), answer()]]) add('items shape', read({ items }));
for (const id of ['', null, false, 1, [], 'answer\ud800', 'answer😀']) add('item id', read({ items: [answer({ id })] }));
for (const type of ['userMessage', 'reasoning', 'agentMessage', 'commandExecution', 'webSearch', 'mcpToolCall',
  'fileChange', 'imageView', 'plan', null, false, 7, {}, [], ['reasoning'], [['userMessage']], ['agentMessage']]) {
  add('item type ' + JSON.stringify(type), read({ items: [{ id: 'extra', type }, answer()] }));
}
for (const phase of [null, '', false, 'commentary', 'final_answer', ['final_answer']]) {
  add('agent phase', read({ items: [answer({ phase })] }));
}
for (const text of [null, 1, [], {}, false]) add('nonstring text', read({ items: [answer({ text })] }));
add('invalid non-final text precedes valid final', read({ items: [answer({ id: 'partial', phase: 'commentary', text: null }), answer()] }));
add('non-final text is not parsed', read({ items: [answer({ id: 'partial', phase: 'commentary', text: '{' }), answer()] }));
add('two distinct finals', read({ items: [answer(), answer({ id: 'second' })] }));
add('duplicate reasoning id', read({ items: [{ id: 'extra', type: 'reasoning' }, { id: 'extra', type: 'userMessage' }, answer()] }));
add('distinct Unicode IDs', read({ items: [{ id: 'é', type: 'reasoning' }, { id: 'e\u0301', type: 'reasoning' }, answer()] }));
for (const [field, values] of Object.entries({ urgency: ['low', 'normal', 'high', 'critical', 'unknown', null],
  disposition: ['ignore', 'record', 'notify', 'task', null], notification: ['none', 'main', 'voice', 'needs_attention', 'unknown', null],
  summary: ['', 'x', '😀'.repeat(8192), '😀'.repeat(8193), '\ud800', '\udfff', 'x'.repeat(8193)],
  question: [true, false, 0, null], usefulIntel: [true, false, 'true', null],
  task: [null, {}, { objectId: '11111111-1111-4111-8111-111111111111', revision: 1 }] })) {
  for (const value of values) add('assessment ' + field, read({ items: [answer({ text: JSON.stringify(assessment({ [field]: value })) })] }));
}
for (const field of Object.keys(assessment())) {
  const value = assessment(); delete value[field];
  add('missing ' + field, read({ items: [answer({ text: JSON.stringify(value) })] }));
}
add('unknown assessment field', read({ items: [answer({ text: JSON.stringify(assessment({ extra: true })) })] }));
for (const text of ['', '{', 'null', '[]', 'true', '0', '{}', 'NaN', '\uFEFF' + JSON.stringify(assessment()),
  JSON.stringify(assessment()) + '\ntrailing', ' '.repeat(1024 * 1024 + 1) + JSON.stringify(assessment()),
  JSON.stringify(assessment()).replace('"question":true', '"question":true,"question":false'),
  JSON.stringify(assessment()).replace('"question":true', '"question":false,"question":true'),
  JSON.stringify(assessment()).replace('"summary":"The agenda needs your review."', '"summary":"raw\ud800"'),
  JSON.stringify(assessment()).replace('"summary":"The agenda needs your review."', '"summary":"raw\\\ud800"'),
  JSON.stringify(assessment()).replace('"summary":"The agenda needs your review."', '"summary":"raw\\\\\ud800"'),
  '\ud800' + JSON.stringify(assessment()),
  JSON.stringify(assessment()).replace('"question":true', '"question":1e999'),
  JSON.stringify(assessment()).replace('"question":true', '"question":9007199254740992')]) {
  add('final JSON grammar', read({ items: [answer({ text })] }));
}
process.stdout.write(JSON.stringify(cases));
