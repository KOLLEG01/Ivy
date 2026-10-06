import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';
import { NativeJournal } from './journal.js';
import { NativeRpc } from './rpc.js';

/** Local protocol actor, never a Hive caller or a model-selected answer. */
export const nativeClockPrincipal = 'ivy-native-clock';
export function answerNativeClock(journal: NativeJournal, rpc: NativeRpc, input: Agent.PendingInput): void {
  requireThat(input.method === 'currentTime/read' && input.identity.epoch === journal.epoch && rpc.connected,
    'native_clock_unavailable', 'A host clock answer requires its exact live native request.');
  const original = journal.input(input.identity);
  requireThat(original?.method === input.method && original.observedAt === input.observedAt && canonical(original.params) === canonical(input.params),
    'native_clock_conflict', 'The host clock must retain its original observed request and time.');
  const seconds = Math.floor(Date.parse(original.observedAt) / 1000);
  requireThat(Number.isSafeInteger(seconds), 'native_clock_invalid', 'The original host clock observation must be a finite whole Unix time.');
  const key = { callerPrincipalId: nativeClockPrincipal, operationId: 'clock:' + hashJson(input.identity).slice(7) };
  const reply: Agent.Reply = { result: { currentTimeAt: seconds } };
  const accepted = journal.accept(key, 'agent.answer', { identity: input.identity, reply });
  if (!accepted.created) return;
  try {
    rpc.answer(input.method, input.identity.requestId, reply, () => { journal.dispatchAnswer(key, input.identity, reply); });
  } catch (error) {
    if (journal.get(key)?.phase === 'accepted') journal.rejectBeforeDispatch(key, IvyError.from(error).code);
    throw error;
  }
}
