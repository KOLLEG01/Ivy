import { nativeVersions } from '../../sdk/src/client.js';
import type { Agent, RpcClient } from '../../sdk/src/client.js';
import { nativeRead, record } from './native.js';

export async function readNativeThreadControl(client: RpcClient, node: string, threadId: string, signal: AbortSignal) {
  const bounded = AbortSignal.any([signal, AbortSignal.timeout(35000)]);
  const before = await nativeRead(client, node, 'agent.status', {}, bounded) as Agent.Status;
  if (before.serviceNodeId !== node || before.state !== 'ready' || !before.epoch || !nativeVersions.includes(before.nativeVersion)) throw new Error('The selected native owner is not ready or supported.');
  const thread = record(record(await nativeRead(client, node, 'codex.thread/read', { threadId, includeTurns: false }, bounded)).thread);
  if (thread.id !== threadId) throw new Error('The native read returned another task.');
  const attached = thread.canAcceptDirectInput === true, source = attached ? 'native-capability' : 'unconfirmed';
  const after = await nativeRead(client, node, 'agent.status', {}, bounded) as Agent.Status;
  if (after.state !== 'ready' || (['hostId', 'serviceNodeId', 'epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'] as const).some(key => before[key] !== after[key])) throw new Error('The native connection changed during this read. Refresh current ownership.');
  return { status: after, thread, attached, source };
}
