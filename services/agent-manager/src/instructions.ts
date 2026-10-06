import { mkdir, lstat, readFile, open, unlink, realpath, rename } from 'node:fs/promises';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { atomicJson } from '../../../packages/host-runtime/src/config.js';
import { ExecutorLock } from '../../../packages/host-runtime/src/journal.js';
import { hashJson, digest } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../packages/sdk/src/node.js';
import type { Agent, Operation } from '../../../packages/sdk/src/node.js';
import type { ServiceConnection } from '../../../packages/sdk/src/service.js';
import { hiveMcpAgentSummary, composeHiveMcpInstructions, compactHiveMcpInstructions } from '../../../instructions/hive-mcp.js';

export const instructionsContract = 'agent/instructions';
export function instructionsName(hostId: string | null): string {
  return hostId === null ? 'ivy-agent-instructions' : 'ivy-agent-instructions-host-' + digest(hostId).slice(7);
}
interface VersionedDocument { document: Agent.InstructionsDocument; objectId: string; revision: number; contentHash: string }
async function discoverMcpInstructions(connection: Pick<ServiceConnection, 'request'>): Promise<string> {
  try { return compactHiveMcpInstructions((await connection.request('discovery.instructions', {})).instructions); }
  catch (error) { if (!(error instanceof IvyError) || error.code !== 'not_found') throw error; }
  const services: Array<{ serviceName: string; discoveryHint: string }> = [];
  let cursor: string | null = null;
  for (let page = 0; page < 20; page++) {
    const result: Operation.DiscoveryListResult | null = await connection.request('discovery.list', { limit: 20, ...(cursor ? { cursor } : {}) }).catch(error => {
      if (error instanceof IvyError && error.code === 'not_found') return null;
      throw error;
    });
    if (!result) return hiveMcpAgentSummary;
    for (const item of result.items) if (item.kind === 'service')
      services.push({ serviceName: item.serviceName, discoveryHint: item.description });
    if (!result.nextCursor) return compactHiveMcpInstructions(composeHiveMcpInstructions(services));
    cursor = result.nextCursor;
  }
  throw new IvyError('limit_exceeded', 'Hive service discovery exceeds the bounded instruction projection.');
}
export async function readInstructions(connection: Pick<ServiceConnection, 'request'>, hostId: string | null): Promise<VersionedDocument | null> {
  const object = await connection.request('objects.stat', { path: '/' + instructionsName(hostId) }).catch(error => {
    if (error instanceof IvyError && error.code === 'not_found') return null; throw error;
  });
  if (!object) return null;
    if (object.effectivelyArchived) return null;
    const value = await connection.request('objects.read', { objectId: object.id, revision: object.currentRevision });
    requireThat(value.object.contractKey === instructionsContract && value.revision.contractVersion === '1.0.0' && value.content.encoding === 'json',
      'contract_mismatch', 'Instructions path contains a different document contract.');
    validateAgent('InstructionsDocument', value.content.value);
    const document = value.content.value as unknown as Agent.InstructionsDocument;
    requireThat(document.hostId === hostId, 'target_conflict', 'Instruction document belongs to another scope.');
    return { document, objectId: object.id, revision: object.currentRevision, contentHash: value.revision.contentHash };
}

function inheritedInstructions(defaults: Agent.InstructionsDocument | null, global: VersionedDocument | null, host: VersionedDocument | null) {
  const fallback = defaults ? { document: defaults, objectId: 'project-default', revision: 0, contentHash: hashJson(defaults) } : null;
  const hive = global?.document.enabled === null ? fallback : global ?? fallback;
  const effectiveHost = host?.document.enabled === null ? null : host;
  const selected = effectiveHost ?? hive;
  return { hive, effectiveHost, active: selected?.document.enabled === true, includeLocal: selected?.document.includeLocal === true };
}

export function composeInstructions(global: VersionedDocument | null, host: VersionedDocument | null, local: Buffer | null,
  defaults: Agent.InstructionsDocument | null = null, discoveredInstructions = hiveMcpAgentSummary) {
  const inherited = inheritedInstructions(defaults, global, host), hasLocal = Boolean(local?.length);
  const sources = inherited.active ? [inherited.hive, inherited.effectiveHost].filter(value => value?.document.enabled).map(value => value!) : [];
  let localText = '';
  if (inherited.active && inherited.includeLocal && hasLocal) {
    try { localText = new TextDecoder('utf-8', { fatal: true }).decode(local!); }
    catch { throw new IvyError('target_conflict', 'Local AGENTS.md is not valid UTF-8; its bytes remain unchanged.'); }
  }
  const body = !inherited.active ? null : Buffer.from([
    ...(inherited.includeLocal && hasLocal ? ['## Local user instructions', localText] : []),
    ...sources.flatMap(value => value.document.hostId === null
      ? [value.document.text.replace(/^# Actions attributed to the user(?=\r?\n|$)/, '## Actions attributed to the user')]
      : ['## Host instructions (take precedence over Hive instructions)', value.document.text]),
    '## Ivy MCP',
    discoveredInstructions,
    ...(inherited.includeLocal ? ['@AGENTS.md'] : []),
  ].join('\n\n') + '\n');
  requireThat(!body || body.length <= 32768, 'limit_exceeded', 'Composed instructions exceed 32 KiB; shorten the documents instead of silently truncating them.');
  return { body, version: hashJson({ defaults, global, host, localHash: inherited.active && inherited.includeLocal && hasLocal ? digest(local!) : null,
    discoveredInstructions: inherited.active ? discoveredInstructions : null }) };
}

async function regularBytes(path: string): Promise<Buffer | null> {
  try {
    const info = await lstat(path);
    requireThat(info.isFile() && !info.isSymbolicLink() && info.size <= 1024 * 1024, 'target_conflict', 'Instruction file must be a bounded regular file.');
    return await readFile(path);
  } catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error; }
}
const bytesHash = (bytes: Buffer | null) => bytes === null ? null : digest(bytes);
interface ManagedState {
  schemaVersion: 1; authority: string; originalHash: string | null; appliedHash: string | null; appliedVersion: string | null;
  pending: { before: string | null; after: string | null; version: string; active: boolean } | null;
  active: boolean;
}

/** Only this small managed output is owned in a shared user home. Never owns the home itself. */
export class InstructionsManager {
  status: Agent.InstructionsStatus;
  private running: Promise<void> | null = null;
  constructor(readonly home: string, readonly hostId: string, readonly hiveUrl: string,
    readonly defaults: Agent.InstructionsDocument) {
    this.status = { state: 'pending', home, desiredVersion: null, appliedVersion: null, observedAt: new Date().toISOString(), code: null, bytes: 0 };
  }
  synchronize(connection: Pick<ServiceConnection, 'request'>): Promise<void> {
    if (!this.running) this.running = this.apply(connection).catch(error => {
      this.status = { ...this.status, state: ['target_conflict', 'limit_exceeded', 'contract_mismatch'].includes(IvyError.from(error).code) ? 'conflict' : 'pending',
        observedAt: new Date().toISOString(), code: IvyError.from(error).code };
    }).finally(() => { this.running = null; });
    return this.running;
  }
  bootstrap(): Promise<void> {
    return this.synchronize({ request: async () => { throw new IvyError('not_found', 'No stored instruction override.'); } } as unknown as Pick<ServiceConnection, 'request'>);
  }
  async close(): Promise<void> { await this.running; }
  private async apply(connection: Pick<ServiceConnection, 'request'>): Promise<void> {
    // Failed/incomplete reads never mean disable: keep the last successfully applied file.
    const actualHome = await realpath(this.home), target = join(actualHome, 'AGENTS.override.md'), root = join(actualHome, '.ivy-agent-instructions');
    const desiredNow = async () => {
      const global = await readInstructions(connection, null), host = await readInstructions(connection, this.hostId);
      const selected = inheritedInstructions(this.defaults, global, host);
      const discovered = selected.active ? await discoverMcpInstructions(connection) : null;
      return composeInstructions(global, host, selected.active && selected.includeLocal ? await regularBytes(join(actualHome, 'AGENTS.md')) : null,
        this.defaults, discovered ?? hiveMcpAgentSummary);
    };
    // A disabled, never-managed home does not need a lock directory or any filesystem changes.
    const existingRoot = await lstat(root).catch(error => { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error; });
    if (!existingRoot) {
      const desired = await desiredNow();
      if (!desired.body) {
        this.status = { ...this.status, desiredVersion: desired.version, state: 'disabled', observedAt: new Date().toISOString(), code: null }; return;
      }
    }
    await mkdir(root, { recursive: true, mode: 0o700 });
    const rootInfo = await lstat(root);
    requireThat(rootInfo.isDirectory() && !rootInfo.isSymbolicLink(), 'target_conflict', 'Managed instruction state is not a real directory.');
    for (const suffix of ['', '-journal', '-wal', '-shm']) await regularBytes(join(root, 'executor-lock.sqlite' + suffix));
    const lock = new ExecutorLock(root);
    try {
      // A peer may have applied a newer revision while this caller waited for the home lock.
      const desired = await desiredNow();
      this.status = { ...this.status, desiredVersion: desired.version, observedAt: new Date().toISOString() };
      const authority = hashJson({ hive: this.hiveUrl, hostId: this.hostId });
      const statePath = join(root, 'state.json');
      const raw = await regularBytes(statePath);
      let state: ManagedState | null = raw ? JSON.parse(raw.toString('utf8')) as ManagedState : null;
      requireThat(!state || state.schemaVersion === 1 && state.authority === authority, 'target_conflict', 'Another Hive or host owns this home instruction output.');
      const saveBlob = async (bytes: Buffer | null): Promise<string | null> => {
        if (bytes === null) return null;
        const hash = digest(bytes), path = join(root, hash.slice(7) + '.md');
        const existing = await regularBytes(path);
        if (existing) requireThat(digest(existing) === hash, 'target_conflict', 'Instruction backup bytes changed.');
        else { const file = await open(path, 'wx', 0o600); try { await file.writeFile(bytes); await file.sync(); } finally { await file.close(); } }
        return hash;
      };
      const loadBlob = async (hash: string | null) => {
        if (hash === null) return null;
        requireThat(/^sha256:[a-f0-9]{64}$/.test(hash), 'target_conflict', 'Invalid instruction backup reference.');
        const bytes = await regularBytes(join(root, hash.slice(7) + '.md'));
        requireThat(bytes && digest(bytes) === hash, 'target_conflict', 'Instruction backup is unavailable or changed.'); return bytes;
      };
      const finishPending = async () => {
        const pending = state!.pending!;
        const current = await regularBytes(target), currentHash = bytesHash(current);
        requireThat(currentHash === pending.before || currentHash === pending.after, 'target_conflict', 'Local instructions changed during application; user edits are retained.');
        if (currentHash !== pending.after) {
          const bytes = await loadBlob(pending.after);
          if (bytes === null) { requireThat(bytesHash(await regularBytes(target)) === pending.before, 'target_conflict', 'Local instructions changed.'); await unlink(target); }
          else {
            const temporary = join(actualHome, '.ivy-instructions-' + randomUUID() + '.tmp');
            const file = await open(temporary, 'wx', 0o600);
            try { await file.writeFile(bytes); await file.sync(); } finally { await file.close(); }
            try { requireThat(bytesHash(await regularBytes(target)) === pending.before, 'target_conflict', 'Local instructions changed.'); await rename(temporary, target); }
            finally { await unlink(temporary).catch(() => undefined); }
          }
          if (process.platform !== 'win32') {
            const directory = await open(actualHome, 'r'); try { await directory.sync(); } finally { await directory.close(); }
          }
        }
        state = { ...state!, appliedHash: pending.after, appliedVersion: pending.version, active: pending.active, pending: null };
        await atomicJson(statePath, state);
      };
      if (state?.pending) await finishPending();
      const current = await regularBytes(target);
      if (!state) {
        if (!desired.body) { this.status = { ...this.status, state: 'disabled', code: null }; return; }
        const originalHash = await saveBlob(current);
        state = { schemaVersion: 1, authority, originalHash, appliedHash: originalHash, appliedVersion: null, pending: null, active: false };
      }
      requireThat(bytesHash(current) === state.appliedHash, 'target_conflict', 'AGENTS.override.md has user changes; Ivy will not overwrite or remove them.');
      const after = desired.body ? await saveBlob(desired.body) : state.originalHash;
      if (after !== state.appliedHash || state.appliedVersion !== desired.version || state.active !== Boolean(desired.body)) {
        state.pending = { before: state.appliedHash, after, version: desired.version, active: Boolean(desired.body) };
        await atomicJson(statePath, state); await finishPending();
      }
      this.status = { state: desired.body ? 'applied' : 'disabled', home: actualHome, desiredVersion: desired.version,
        appliedVersion: state.appliedVersion, observedAt: new Date().toISOString(), code: null, bytes: desired.body?.length ?? 0 };
    } finally { lock.close(); }
  }
}
