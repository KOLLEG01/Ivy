import type { Wire } from '../../../../packages/sdk/src/node.js';
import { same } from './checks.js';
import type { TaskBoardEngine } from './engine.js';
import { nativeArray, nativeRecord, nativeString } from './native-intent.js';
import { TaskBoardNativeInspector } from './native-inspector.js';
import type { Document } from './store.js';

const retryDelayMs = 15 * 60 * 1000;
const capacityRetryDelayMs = 30 * 1000;
const connectionRetryDelayMs = 10 * 1000;
const resetGraceMs = 60 * 1000;
export const rateLimitWaitingDetail = (at: string): string => `Agent temporarily unavailable; automatic continuation after ${at}.`;

function localReset(message: string, failedAt: number): number | null {
  const iso = /\bresets?(?: at)?\s+(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2}))/i.exec(message);
  if (iso) {
    const at = Date.parse(iso[1]!);
    return Number.isFinite(at) ? at : null;
  }
  const clock = /\bresets?(?: at)?\s+(\d{1,2}):(\d{2})\s*(am|pm)?\s*\(([^)]+)\)/i.exec(message);
  if (!clock) return null;
  let hour = Number(clock[1]);
  const minute = Number(clock[2]), period = clock[3]?.toLowerCase();
  if (minute > 59 || (period ? hour < 1 || hour > 12 : hour > 23)) return null;
  if (period) hour = hour % 12 + (period === 'pm' ? 12 : 0);
  try {
    const format = new Intl.DateTimeFormat('en-US', { timeZone: clock[4], year: 'numeric', month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });
    const parts = (at: number) => Object.fromEntries(format.formatToParts(at).map(part => [part.type, Number(part.value)]));
    const day = parts(failedAt);
    const resolve = (offset: number) => {
      const target = Date.UTC(day['year']!, day['month']! - 1, day['day']! + offset, hour, minute);
      let at = target;
      for (let pass = 0; pass < 3; pass++) {
        const local = parts(at);
        const actual = Date.UTC(local['year']!, local['month']! - 1, local['day']!, local['hour']!, local['minute']!, local['second']!);
        at += target - actual;
      }
      return at;
    };
    const today = resolve(0);
    const reset = today + resetGraceMs >= failedAt ? today : resolve(1);
    // A five-hour/session reset cannot be almost a day away. Treat a repeated stale clock as unavailable.
    return /\bsession limit\b|\bfive hour\b/i.test(message) && reset - failedAt > 5 * 60 * 60 * 1000 ? null : reset;
  } catch { return null; }
}

/** Only a failed native error can request a retry; assistant prose and usage warnings cannot. */
export function nativeRateLimitRetryAt(turn: Record<string, Wire.Json>, observedAt: string): string | null {
  if (turn['status'] !== 'failed') return null;
  const error = nativeRecord(turn['error']);
  const observed = Date.parse(observedAt), completed = turn['completedAt'];
  const failedAt = typeof completed === 'number' && Number.isFinite(completed) && completed * 1000 <= observed
    ? completed * 1000 : observed;
  return nativeErrorRetryAt(error, new Date(failedAt).toISOString());
}

/** A conclusive native failure may retry; unknown dispatch outcomes still require reconciliation. */
export function nativeErrorRetryAt(error: Record<string, Wire.Json>, observedAt: string): string | null {
  const failedAt = Date.parse(observedAt), message = nativeString(error['message']);
  const info = error['codexErrorInfo'] ?? nativeRecord(error['data'])['codexErrorInfo'];
  const kind = typeof info === 'string' ? info : Object.keys(nativeRecord(info))[0];
  const httpStatus = kind ? nativeRecord(nativeRecord(info)[kind])['httpStatusCode'] : null;
  const connectionFailure = ['httpConnectionFailed', 'responseStreamConnectionFailed', 'responseTooManyFailedAttempts'].includes(kind ?? '') &&
    (httpStatus == null || typeof httpStatus === 'number' && ([408, 425, 429].includes(httpStatus) || httpStatus >= 500));
  if (kind === 'rateLimitExceeded' || connectionFailure && httpStatus === 429)
    return new Date(failedAt + 60_000).toISOString();
  if (kind === 'serverOverloaded' || connectionFailure && httpStatus === 503 ||
    /^Selected model is at capacity(?:[.!]|$)/i.test(message))
    return new Date(failedAt + capacityRetryDelayMs).toISOString();
  if (connectionFailure || kind === 'responseStreamDisconnected')
    return new Date(failedAt + connectionRetryDelayMs).toISOString();
  if (kind !== 'usageLimitExceeded' &&
    !/^(?:you['’]ve hit your (?:session|weekly|usage) limit\b|Claude [\w ]{1,64} limit reached\b|rate limit (?:reached|exceeded)\b)/i.test(message)) return null;
  const reset = localReset(message, failedAt);
  return new Date(Math.max(failedAt + resetGraceMs, reset === null || reset < failedAt
    ? failedAt + retryDelayMs : reset + resetGraceMs)).toISOString();
}

/** Reinspect the original failed turn when a ticket is still waiting for user intervention. */
export async function failedRateLimitRetryAt(engine: TaskBoardEngine, task: Document<'task-board/task'>): Promise<string | null> {
  const value = task.value, primary = value.primaryResourceRef;
  if (value.runtimeWait?.reason !== 'host' ||
    !primary || primary.namespace !== 'codex' || primary.kind !== 'thread' || !value.lastRun) return null;
  const run = await engine.store.read('task-board/run', value.lastRun);
  if (run.value.taskId !== task.pin.objectId || run.value.phase !== 'failed' || run.value.externalOutcome.code !== 'native_failed' ||
    run.value.externalOutcome.state !== 'failed' || run.value.cancellation || !run.value.turnId || !run.value.nativeEpoch ||
    !run.value.result || !same(run.value.result, value.latestResult) || !same(run.value.primaryResourceRef, primary)) return null;
  const result = await engine.store.read('task-board/result', run.value.result);
  if (result.value.content.summary !== 'Native turn failed.' && !nativeRateLimitRetryAt({ status: 'failed', error: { message: result.value.content.summary } }, run.value.finishedAt!)) return null;
  const inspected = await new TaskBoardNativeInspector(engine).find(run.pin);
  if (inspected.kind !== 'found' || !('result' in inspected.turns.reply) || !('result' in inspected.metadata.reply)) return null;
  const thread = nativeRecord(nativeRecord(inspected.metadata.reply.result)['thread']);
  if (nativeRecord(thread['status'])['type'] === 'active') return null;
  const turn = nativeArray(nativeRecord(inspected.turns.reply.result)['data']).map(nativeRecord).find(item => item['id'] === run.value.turnId);
  return turn ? nativeRateLimitRetryAt(turn, run.value.finishedAt ?? inspected.turns.observedAt) : null;
}
