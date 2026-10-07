import type { Readable, Writable } from 'node:stream';
import { randomUUID } from 'node:crypto';
import { encodeJson } from '../../sdk/src/node.js';
import { IvyError, requireThat } from '../../sdk/src/node.js';
import { validateAgent, validateAgentFrame } from '../../sdk/src/node.js';
import type { Agent, Wire } from '../../sdk/src/node.js';
import { nativeFrameBytes, nativeRequestFrameBytes, nativeAnswerFrameBytes } from './codex-limits.js';

export interface NativeRequest { id: Agent.RequestId; method: string; params: Wire.Json }
export interface NativeNotification { method: string; params: Wire.Json }
export interface NativeRequestHooks {
  requestId?: string;
  /** Background reads may expire without terminating unrelated accepted turns. */
  detachOnTimeout?: boolean;
  /** Reserve native capacity for foreground requests while background work is outstanding. */
  priority?: 'interaction';
  beforeSend?: (id: Agent.RequestId) => void;
  beforeResolve?: (id: Agent.RequestId, reply: Agent.Reply) => void;
}
interface Pending extends NativeRequestHooks {
  resolve: (reply: Agent.Reply) => void;
  reject: (error: IvyError) => void; timer: NodeJS.Timeout;
}

/** Public native JSONL only. The owner supplies durable hooks; this layer never retries. */
export class NativeRpc {
  readonly closed: Promise<string>;
  private finishClosed!: (code: string) => void;
  private ended = false;
  private fragments: Buffer[] = [];
  private bufferedBytes = 0;
  private readonly pending = new Map<string, Pending>();
  private readonly expired = new Set<string>();
  private readonly requests: Map<string, Agent.Catalog['clientRequests'][number]>;
  private readonly serverRequests: Map<string, Agent.Catalog['serverRequests'][number]>;
  private readonly notifications: Map<string, Agent.Catalog['serverNotifications'][number]>;
  private readonly clientNotifications: Map<string, Agent.Catalog['clientNotifications'][number]>;
  constructor(readonly catalog: Agent.Catalog, private readonly input: Writable, output: Readable, private readonly hooks: {
    onRequest: (request: NativeRequest) => void; onNotification: (notification: NativeNotification) => void;
    onClose: (code: string) => void;
  }) {
    validateAgent('Catalog', catalog);
    const methods = <T extends { method: string }>(values: T[]): Map<string, T> => {
      const map = new Map(values.map(value => [value.method, value]));
      requireThat(map.size === values.length, 'native_catalog_invalid', 'Native catalog has duplicate methods.'); return map;
    };
    this.requests = methods(catalog.clientRequests); this.serverRequests = methods(catalog.serverRequests);
    this.notifications = methods(catalog.serverNotifications); this.clientNotifications = methods(catalog.clientNotifications);
    this.closed = new Promise(resolve => { this.finishClosed = resolve; });
    output.on('data', (chunk: Buffer) => this.receive(chunk));
    output.once('end', () => this.close(this.bufferedBytes ? 'native_truncated_frame' : 'native_connection_closed'));
    output.once('error', () => this.close('native_read_failed'));
    input.once('error', () => this.close('native_write_failed'));
  }
  get connected(): boolean { return !this.ended; }
  private writable(): void {
    requireThat(!this.ended && !this.input.destroyed && this.input.writable && !this.input.writableEnded, 'native_unavailable', 'Native connection is unavailable.');
    requireThat(this.input.writableLength < nativeAnswerFrameBytes, 'native_capacity', 'Native write buffer is full; no new native request was sent.');
  }
  private write(encoded: string): void {
    this.input.write(encoded + '\n', error => { if (error) this.close('native_write_failed'); });
  }
  request(method: string, params: Wire.Json, hooks: NativeRequestHooks = {}, timeoutMs = 120_000): Promise<Agent.Reply> {
    let encoded: string, id: string;
    try {
      this.writable(); requireThat(this.pending.size < (hooks.priority==='interaction'?64:56), 'native_capacity', 'Native request capacity is full; foreground slots remain reserved.');
      requireThat(!hooks.detachOnTimeout || this.expired.size + [...this.pending.values()].filter(value=>value.detachOnTimeout).length < (hooks.priority==='interaction'?64:16), 'native_capacity', 'Native observation capacity is full.');
      requireThat(this.requests.has(method), 'native_method_unavailable', 'Method is absent from this installed native catalog.');
      requireThat(Number.isSafeInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= 600_000, 'invalid_arguments', 'Native observation deadline must be finite and at most ten minutes.');
      id = hooks.requestId ?? randomUUID(); validateAgentFrame(id, 'request-id');
      requireThat(!this.pending.has(id) && !this.expired.has(id), 'native_state_conflict', 'Native request ID is already in use.');
      encoded = encodeJson({ id, method, params }, nativeRequestFrameBytes);
    } catch (error) { return Promise.reject(error instanceof IvyError ? error : new IvyError('native_request_invalid', 'Native request validation failed.')); }
    return new Promise<Agent.Reply>((resolve, reject) => {
      const timer = setTimeout(() => {
        if(!hooks.detachOnTimeout){this.close('native_deadline_exceeded');return;}
        this.pending.delete(id);this.expired.add(id);
        reject(new IvyError('native_observation_deadline','Background observation expired; active turns remain connected.','unknown'));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer, ...hooks });
      try { hooks.beforeSend?.(id); }
      catch (error) { clearTimeout(timer); this.pending.delete(id); reject(error); return; }
      try { this.write(encoded); } catch { this.close('native_write_failed'); }
    });
  }
  notify(method: string, params: Wire.Json = null): void {
    this.writable(); const definition = this.clientNotifications.get(method);
    requireThat(definition, 'native_method_unavailable', 'Client notification is absent from the installed native catalog.');
    const encoded = encodeJson({ method, ...(definition.paramsAbsent ? {} : { params }) }, nativeAnswerFrameBytes);
    try { this.write(encoded); } catch { this.close('native_write_failed'); throw new IvyError('outcome_unknown', 'Native notification write failed.', 'unknown'); }
  }
  /** The owner must validate the current pending identity and persist answer intent in beforeSend. */
  answer(method: string, id: Agent.RequestId, reply: Agent.Reply, beforeSend: () => void): void {
    this.writable(); validateAgentFrame(id, 'request-id'); validateAgentFrame(reply);
    const definition = this.serverRequests.get(method); requireThat(definition, 'native_method_unavailable', 'Server request is absent from the installed native catalog.');
    const encoded = encodeJson({ id, ...reply }, nativeAnswerFrameBytes); beforeSend();
    try { this.write(encoded); } catch { this.close('native_write_failed'); throw new IvyError('outcome_unknown', 'Native answer write failed.', 'unknown'); }
  }
  private receive(chunk: Buffer): void {
    if (this.ended) return;
    try {
      requireThat(Buffer.isBuffer(chunk), 'native_invalid_frame', 'Native stdio must contain UTF-8 bytes.');
      let offset = 0;
      while (!this.ended && offset < chunk.length) {
        const newline = chunk.indexOf(10, offset), end = newline < 0 ? chunk.length : newline;
        const length = end - offset;
        requireThat(this.bufferedBytes + length <= nativeFrameBytes, 'native_frame_too_large', 'Native frame exceeds its byte bound.');
        let copied = 0;
        while (copied < length) {
          const used = this.bufferedBytes % 65536;
          if (used === 0) this.fragments.push(Buffer.allocUnsafe(Math.min(65536, nativeFrameBytes - this.bufferedBytes)));
          const last = this.fragments.at(-1)!, count = Math.min(last.length - used, length - copied);
          chunk.copy(last, used, offset + copied, offset + copied + count); this.bufferedBytes += count; copied += count;
        }
        if (newline < 0) break;
        // Copy a fragmented frame once, rather than repeatedly copying its growing prefix.
        const line = Buffer.concat(this.fragments, this.bufferedBytes); this.fragments = []; this.bufferedBytes = 0;
        offset = newline + 1;
        const value: unknown = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(line)); this.frame(value);
      }
    } catch (error) { this.close(error instanceof IvyError ? error.code : 'native_invalid_frame'); }
  }
  private frame(value: unknown): void {
    requireThat(value && typeof value === 'object' && !Array.isArray(value), 'native_invalid_frame', 'Native frame must be one protocol object.');
    const frame = value as Record<string, Wire.Json>;
    requireThat(!Object.hasOwn(frame, 'jsonrpc') || frame['jsonrpc'] === '2.0', 'native_invalid_frame', 'Native JSON-RPC version is invalid.');
    if (Object.hasOwn(frame, 'method')) {
      requireThat(typeof frame['method'] === 'string' && !Object.hasOwn(frame, 'result') && !Object.hasOwn(frame, 'error'), 'native_invalid_frame', 'Native request/notification variants cannot mix.');
      const hasId = Object.hasOwn(frame, 'id'), method = frame['method'];
      const definition = (hasId ? this.serverRequests : this.notifications).get(method);
      requireThat(definition, 'native_catalog_mismatch', 'Native method is absent from the pinned generated catalog.');
      requireThat(!definition.paramsRequired || Object.hasOwn(frame, 'params'), 'native_invalid_frame', 'Native method is missing required parameters.');
      const params = frame['params'] ?? null;
      if (hasId) validateAgentFrame(frame['id'], 'request-id');
      try {
        if (hasId) this.hooks.onRequest({ id: frame['id'] as Agent.RequestId, method, params });
        else this.hooks.onNotification({ method, params });
      } catch (error) { this.close(error instanceof IvyError ? error.code : 'native_observation_commit_failed'); }
      return;
    }
    validateAgentFrame(frame['id'], 'request-id');
    requireThat(typeof frame['id'] === 'string', 'native_unknown_response', 'Native response does not identify an outstanding client request.');
    const id = frame['id'], pending = this.pending.get(id), expired = this.expired.has(id);
    requireThat(pending || expired, 'native_unknown_response', 'Native response has no outstanding client request.');
    requireThat(Object.hasOwn(frame, 'result') !== Object.hasOwn(frame, 'error'), 'native_invalid_frame', 'Native response requires exactly one result or error.');
    const reply = Object.hasOwn(frame, 'result') ? { result: frame['result']! } : { error: frame['error']! };
    validateAgentFrame(reply); const typed = reply as Agent.Reply;
    if(!pending){this.expired.delete(id);return;}
    try { pending.beforeResolve?.(id, typed); }
    catch { this.close('native_outcome_commit_failed'); return; }
    clearTimeout(pending.timer); this.pending.delete(id); pending.resolve(typed);
  }
  close(code = 'native_connection_closed'): void {
    if (this.ended) return;
    this.ended = true; this.fragments = []; this.bufferedBytes = 0;
    try { this.hooks.onClose(code); } catch { code = 'native_recovery_storage_failed'; }
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(new IvyError(code, 'Native connection ended; reconcile the original operation.', 'unknown')); }
    this.pending.clear(); this.expired.clear(); this.finishClosed(code);
  }
}
