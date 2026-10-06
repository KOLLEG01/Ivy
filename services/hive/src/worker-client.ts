import { Worker } from 'node:worker_threads';
import type { WorkerOptions } from 'node:worker_threads';
import { IvyError } from '../../../packages/contracts/src/errors.js';
import type { WireError } from '../../../packages/contracts/src/errors.js';
import type { KernelOptions } from './kernel.js';
import type { WorkerAction } from './worker-protocol.js';
import type { RoutingUpdate } from './live-routing.js';

interface Pending { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: NodeJS.Timeout; bytes: number; backup: boolean; started(): void }
function fromWire(error: WireError): IvyError { return new IvyError(error.data.code, error.message, error.data.outcome, error.data.details); }

/** Owns one bounded storage worker. Callers decide when to retry startup; accepted mutations are never replayed. */
export class WorkerClient {
  private worker: Worker | null = null;
  private pending = new Map<number, Pending>();
  private sequence = 0;
  private queuedBytes = 0;
  private stopping = false;
  private starting: Promise<void> | null = null;
  ready = false;
  onFailure: ((error: IvyError) => void) | null = null;
  onRoutingUpdate: ((value: RoutingUpdate) => void) | null = null;
  onEventsAvailable: ((throughSequence: number) => void) | null = null;
  onChanges: ((scopes: string[]) => void) | null = null;
  constructor(readonly options: KernelOptions,
    private readonly createWorker: (file: URL, options: WorkerOptions) => Worker = (file, options) => new Worker(file, options)) {}

  start(): Promise<void> {
    if (this.starting) return this.starting;
    if (this.ready) return Promise.resolve();
    this.stopping = false;
    const file = new URL(import.meta.url.endsWith('.ts') ? './worker.ts' : './worker.js', import.meta.url);
    const worker = this.createWorker(file, { workerData: this.options, resourceLimits: { maxOldGenerationSizeMb: 512 } });
    this.worker = worker;
    this.starting = new Promise<void>((resolve, reject) => {
      const deadline = setTimeout(() => { const error = new IvyError('deadline_exceeded', 'Hive storage startup exceeded its deadline.'); reject(error); this.fail(error, worker); }, 30_000);
      const failed = (error: IvyError) => { clearTimeout(deadline); reject(error); this.fail(error, worker); };
      worker.on('error', () => failed(new IvyError('storage_unavailable', 'Hive storage worker failed.', 'unknown')));
      worker.on('exit', code => {
        if (this.worker !== worker || this.stopping) return;
        failed(new IvyError('storage_unavailable', `Hive storage worker exited (${code}).`, 'unknown'));
      });
      worker.on('message', (message: { ready?: boolean; fatal?: WireError; id?: number; started?: boolean; progress?: boolean; error?: WireError; value?: unknown;
        routing?: RoutingUpdate; eventsAvailable?: { throughSequence: number }; changes?: string[] }) => {
        if (this.worker !== worker) return;
        if (message.ready) { clearTimeout(deadline); this.ready = true; resolve(); return; }
        if (message.fatal) { failed(fromWire(message.fatal)); return; }
        if (message.routing) { this.onRoutingUpdate?.(message.routing); return; }
        if (message.eventsAvailable) { this.onEventsAvailable?.(message.eventsAvailable.throughSequence); return; }
        if (message.changes) { this.onChanges?.(message.changes); return; }
        const pending = message.id === undefined ? undefined : this.pending.get(message.id);
        if (!pending) return;
        if (message.progress) { if (pending.backup) pending.started(); return; }
        if (message.started) { pending.started(); return; }
        this.pending.delete(message.id!); this.queuedBytes -= pending.bytes; clearTimeout(pending.timer);
        if (message.error) pending.reject(fromWire(message.error)); else pending.resolve(message.value);
      });
    }).finally(() => { this.starting = null; });
    return this.starting;
  }
  request<T>(payload: WorkerAction, timeoutMs = 30_000, terminateOnTimeout = true, reservedBytes = 4096): Promise<T> {
    const worker = this.worker;
    if (!worker || !this.ready) return Promise.reject(new IvyError('service_unavailable', 'Hive storage is not ready.'));
    const bytes = Number.isSafeInteger(reservedBytes) && reservedBytes >= 0 ? Math.max(1024, reservedBytes) : 4096;
    if (this.pending.size >= 512 || this.queuedBytes + bytes > 64 * 1024 * 1024) return Promise.reject(new IvyError('limit_exceeded', 'Hive storage request queue is full.'));
    const id = ++this.sequence;
    return new Promise<T>((resolve, reject) => {
      const executionExpired = () => {
        const error = new IvyError('deadline_exceeded', 'Hive storage response deadline exceeded; reconcile operation identity.', 'unknown');
        if (terminateOnTimeout) this.fail(error, worker);
        else reject(error); // Keep queue accounting until the issued read returns; health checks cannot kill a valid long sync.
      };
      const timer = setTimeout(() => {
        // Posted work may still execute. A waiting caller cannot kill another valid operation
        // or release its reservation while the worker still retains its payload.
        reject(new IvyError('deadline_exceeded', 'Hive storage queue wait expired; reconcile operation identity.', 'unknown'));
      }, timeoutMs);
      this.queuedBytes += bytes;
      const pending: Pending = { resolve: value => resolve(value as T), reject, timer, bytes, backup: payload.action === 'backup', started: () => {
        clearTimeout(pending.timer); pending.timer = setTimeout(executionExpired, timeoutMs);
      } };
      this.pending.set(id, pending);
      try { worker.postMessage({ id, payload }); }
      catch { this.fail(new IvyError('storage_unavailable', 'Hive storage connection failed.', 'unknown'), worker); }
    });
  }
  private fail(error: IvyError, worker: Worker): void {
    if (this.worker !== worker) return;
    this.ready = false; this.worker = null;
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(error); }
    this.pending.clear(); this.queuedBytes = 0;
    void worker.terminate().catch(() => undefined);
    if (!this.stopping) this.onFailure?.(error);
  }
  async close(): Promise<void> {
    this.stopping = true;
    const worker = this.worker;
    if (!worker) return;
    if (this.ready) { try { await this.request({ action: 'close' }, 5000); } catch { /* definite shutdown continues */ } }
    this.ready = false; this.worker = null;
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(new IvyError('service_unavailable', 'Hive is shutting down.', 'unknown')); }
    this.pending.clear(); this.queuedBytes = 0;
    await worker.terminate();
  }
}
