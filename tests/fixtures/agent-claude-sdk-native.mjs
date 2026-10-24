// Inert vendor endpoint for the real packaged transport and native launch tests.
import { appendFileSync } from 'node:fs';
import { join } from 'node:path';
let next = 0;
const attempts = new Map();
export function query({ prompt, options }) {
  const id = ++next;
  const record = value => appendFileSync(join(options.cwd, 'sdk-fixture.jsonl'), JSON.stringify({ id, ...value }) + '\n');
  record({ type: 'open' });
  let release, closed = false;
  const interrupted = new Promise(resolve => { release = resolve; });
  const result = (text, extra = {}) => ({ type: 'result', subtype: 'success', result: text, ...extra });
  const value = (async function* () {
    const messages = await Array.fromAsync(prompt);
    const mode = messages.at(-1)?.message.content ?? '';
    const attempt = (attempts.get(mode) ?? 0) + 1;
    attempts.set(mode, attempt);
    record({ type: 'query', mode, attempt, mcpServers: options.mcpServers, settingSources: options.settingSources });
    if (mode === 'retry' && attempt === 1 || mode === 'empty') {
      yield result('', { num_turns: 0, duration_api_ms: 0 }); return;
    }
    if (mode === 'unknown-empty') { yield result(''); return; }
    if (mode === 'eof') return;
    if (mode === 'tool') {
      yield { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'fixture-tool', content: 'done' }] } };
      yield result(''); return;
    }
    if (mode === 'stream') {
      yield { type: 'stream_event', event: { type: 'message_start', message: { id: 'stream' } } };
      yield result('', { num_turns: 0, duration_api_ms: 0 }); return;
    }
    if (mode === 'input') {
      const permission=await options.canUseTool('AskUserQuestion', { questions: [{ header: 'Pick', question: 'Which?',
        options: [{ label: 'A', description: 'first' }], multiSelect: false }] },
        { toolUseID: 'question', signal: options.abortController.signal });
      record({ type: 'answer', result: permission });
    }
    if (mode === 'approval' || mode === 'interrupt') {
      const permission = await options.canUseTool('Bash', { command: 'inert fixture' },
        { toolUseID: 'fixture-tool', signal: options.abortController.signal });
      record({ type: 'permission', result: permission });
      if (mode === 'interrupt' || permission.behavior !== 'allow') { await interrupted; return; }
    }
    if (mode === 'hold') { await interrupted; return; }
    yield { type: 'assistant', message: { id: 'answer', content: [{ type: 'text', text: 'useful answer' }] } };
    yield result('useful answer', { session_id: 'fixture-session' });
  })();
  value.interrupt = async () => { record({ type: 'interrupt' }); release(); };
  value.close = () => { if (!closed) { closed = true; record({ type: 'close' }); release(); } };
  value.streamInput = async messages => { record({ type: 'steer', messages: await Array.fromAsync(messages) }); };
  value.supportedModels = async () => [{ value: 'sonnet', displayName: 'Sonnet', description: 'inert model' }];
  value.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET = async () => ({ rate_limits_available: false });
  return value;
}
