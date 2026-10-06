import { randomUUID } from 'node:crypto';
import { hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import type { NativeRpc } from './rpc.js';

/** Short-lived response cache, deliberately independent of Hive and the durable operation journal.
 * A cache miss after an uncertain mutation is not permission to execute that mutation again. */
export class NativeInteractions {
  private entries = new Map<string, { hash: string; work: Promise<Agent.Interaction>; bytes: number; expires: number }>();
  constructor(readonly rpc: Pick<NativeRpc, 'request'>, readonly epoch: string,
    readonly limits = { count: 256, bytes: 32 * 1024 * 1024, ttlMs: 5 * 60_000 }) {}
  private key(caller: string, id: string): string { return JSON.stringify([caller, id]); }
  private prune(): void {
    let bytes = 0;
    for (const [key, entry] of this.entries) {
      if (entry.expires <= Date.now()) this.entries.delete(key); else bytes += entry.bytes;
    }
    for (const [key, entry] of this.entries) {
      if (this.entries.size < this.limits.count && bytes < this.limits.bytes) break;
      if (entry.expires === Infinity) continue;
      this.entries.delete(key); bytes -= entry.bytes;
    }
  }
  lookup(caller: string, id: string): Promise<Agent.Interaction> {
    const entry = this.entries.get(this.key(caller, id));
    if (!entry || entry.expires <= Date.now()) throw new IvyError('interaction_expired', 'No retained interaction reply; reconcile with native thread state before another mutation.', 'unknown');
    return entry.work;
  }
  run(caller: string, id: string, method: string, params: Wire.Json): Promise<Agent.Interaction> {
    const key = this.key(caller, id), hash = hashJson({ method, params }), prior = this.entries.get(key);
    if (prior) { requireThat(prior.hash === hash, 'mutation_conflict', 'Interaction identity has different arguments.'); return prior.work; }
    this.prune();
    requireThat(this.entries.size < this.limits.count, 'native_capacity', 'Native interaction capacity is occupied.');
    const entry = { hash, work: undefined! as Promise<Agent.Interaction>, bytes: 0, expires: Infinity };
    this.entries.set(key, entry);
    entry.work = (async () => {
      try {
        const reply = await this.rpc.request(method, params, { requestId: 'interaction-' + randomUUID(), detachOnTimeout: true, priority:'interaction' });
        const result: Agent.Interaction = { operationId: id, epoch: this.epoch, reply };
        entry.bytes = Buffer.byteLength(JSON.stringify(result));
        return result;
      } catch (error) {
        if(error instanceof IvyError&&error.outcome==='not_executed')this.entries.delete(key);
        throw error;
      } finally { entry.expires = Date.now() + this.limits.ttlMs; this.prune(); }
    })();
    return entry.work;
  }
}
