import { canonical } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../packages/sdk/src/node.js';
import { validateShared } from '../../../packages/sdk/src/node.js';
import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import type { RpcClient } from '../../../packages/sdk/src/client.js';
import type { NativeRpc } from './rpc.js';
import type { NativeJournal } from './journal.js';
import { nativeInventorySchemaVersion, nativeProjectSchemaVersion } from './registry.js';

type Fields = Record<string, Wire.Json>;
const fields = (value: Wire.Json): Fields => {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'native_inventory_invalid', 'Native inventory expected an object.'); return value;
};
interface NativeThread extends Fields {
  id: string; preview: string; cwd: string; name: string | null; status: Wire.Json; source: Wire.Json;
  updatedAt: Wire.Json; ephemeral: boolean;
}
export interface NativeThreadSnapshot { observedAt: string; threads: Wire.InventoryEntry[] }
export interface NativeProjectSnapshot { observedAt: string; projects: Agent.ProjectsResult }
export interface NativeSnapshot extends NativeThreadSnapshot { projects: Agent.ProjectsResult }

/** Schema-derived source kinds include noninteractive native owners; native defaults would omit them. */
export function nativeSourceKinds(catalog: Agent.Catalog): string[] {
  const schema = catalog.clientRequests.find(value => value.method === 'thread/list')?.inputSchema;
  requireThat(schema && typeof schema === 'object', 'native_catalog_mismatch', 'Native thread inventory has no generated input schema.');
  const root = schema as Fields;
  const resolve = (value: Wire.Json | undefined): Fields => {
    const node = fields(value ?? null); if (!node['$ref']) return node;
    const ref = node['$ref']; requireThat(typeof ref === 'string' && /^#\/\$defs\/[^/]+$/.test(ref), 'native_catalog_mismatch', 'Thread source schema must use a local definition.');
    return fields(fields(root['$defs']!) [ref.slice('#/$defs/'.length)]!);
  };
  const kinds = resolve(resolve(fields(resolve(root)['properties']!)['sourceKinds'])['items'])['enum'];
  requireThat(Array.isArray(kinds) && kinds.length > 0 && kinds.every(value => typeof value === 'string'), 'native_catalog_mismatch', 'Installed thread source kinds are not an explicit native enum.');
  return kinds as string[];
}

export class NativeInventory {
  private readonly sourceKinds: string[];
  private nativeProjects: Agent.ProjectSummary[] = [];
  constructor(private readonly rpc: Pick<NativeRpc, 'request' | 'catalog'>, private readonly epoch: string) {
    this.sourceKinds = nativeSourceKinds(rpc.catalog);
  }
  private reader(signal: AbortSignal | undefined, budgetMs: number) {
    const deadline = Date.now() + budgetMs;
    const check = () => { signal?.throwIfAborted(); requireThat(Date.now() < deadline, 'native_inventory_deadline', 'Complete native inventory exceeded its observation deadline.'); };
    const read = async (method: string, params: Fields): Promise<Fields> => {
      check(); const reply = await this.rpc.request(method, params, { detachOnTimeout: true }, budgetMs); check();
      if ('error' in reply) throw new IvyError('native_inventory_unavailable', 'A required native inventory read failed.', 'not_executed', { method, nativeCode: reply.error.code });
      return fields(reply.result);
    };
    const pages = async (method: string, params: Fields, maximum: number, summarize: (value: Wire.Json) => Wire.Json = value => value): Promise<Wire.Json[]> => {
      const result: Wire.Json[] = [], cursors = new Set<string>(); let cursor: string | null = null, bytes = 0;
      do {
        const page = await read(method, { ...params, limit: 100, ...(cursor === null ? {} : { cursor }) });
        requireThat(Array.isArray(page['data']) && (page['nextCursor'] === null || typeof page['nextCursor'] === 'string'), 'native_inventory_invalid', 'Native page must explicitly describe its continuation.');
        requireThat(result.length + page['data'].length <= maximum, 'native_inventory_capacity', 'Complete native inventory exceeds its declared capacity.');
        for (const value of page['data']) {
          const item = summarize(value); bytes += Buffer.byteLength(canonical(item, 9 * 1024 * 1024));
          requireThat(bytes <= 9 * 1024 * 1024, 'native_inventory_capacity', 'Accumulated native inventory exceeds its byte capacity.'); result.push(item);
        }
        cursor = page['nextCursor'];
        if (cursor !== null) { requireThat(!cursors.has(cursor) && cursors.size < maximum + 1, 'native_inventory_invalid', 'Native inventory cursor did not advance within its bound.'); cursors.add(cursor); }
      } while (cursor !== null);
      return result;
    };
    return { check, read, pages };
  }
  async collectThreads(signal?: AbortSignal): Promise<NativeThreadSnapshot> {
    // Archive enumeration can take longer than twenty seconds on a busy Windows host.
    // A snapshot budget must not shorten the transport watchdog and kill unrelated turns.
    const { check, read, pages } = this.reader(signal, 120_000);
    const loaded = async () => {
      const ids = await pages('thread/loaded/list', {}, 5000);
      requireThat(ids.every(value => typeof value === 'string') && new Set(ids).size === ids.length, 'native_inventory_invalid', 'Loaded native thread IDs must be unique strings.'); return new Set(ids as string[]);
    };
    const before = await loaded(), threads = new Map<string, Agent.ThreadSummary>(); let threadBytes = 0;
    const add = (value: Wire.Json, archived: boolean) => {
      const thread = fields(value) as NativeThread;
      requireThat(!threads.has(thread.id), 'native_inventory_changed', 'Thread enumeration repeated an ID; retain the last complete snapshot.');
      const threadSource = thread['threadSource'];
      requireThat(threadSource === undefined || threadSource === null || typeof threadSource === 'string',
        'native_inventory_invalid', 'Native thread classification must be explicit text or absent.');
      const projectId = thread['projectId'], recencyAt = thread['recencyAt'];
      requireThat(projectId === undefined || projectId === null || typeof projectId === 'string',
        'native_inventory_invalid', 'Native thread project assignment must be explicit text or absent.');
      requireThat(recencyAt === undefined || recencyAt === null || Number.isSafeInteger(recencyAt) && Number(recencyAt) >= 0,
        'native_inventory_invalid', 'Native thread recency must be a nonnegative integer or absent.');
      const owned = before.has(thread.id);
      const summary: Agent.ThreadSummary = { nativeId: thread.id, owner: owned ? 'this-native-connection' : 'historical-unattached', epoch: owned ? this.epoch : null,
        preview: thread.preview.slice(0, 2048), cwd: thread.cwd, name: thread.name === null ? null : thread.name.slice(0, 512), status: thread.status,
        source: thread.source, updatedAt: thread.updatedAt, canAcceptDirectInput: typeof thread['canAcceptDirectInput'] === 'boolean' ? thread['canAcceptDirectInput'] : null,
        projectId: projectId ?? null, recencyAt: recencyAt == null ? null : Number(recencyAt), threadSource: threadSource ?? null, archived, ephemeral: thread.ephemeral };
      validateAgent('ThreadSummary', summary); threadBytes += Buffer.byteLength(canonical(summary));
      requireThat(threadBytes <= 9 * 1024 * 1024, 'native_inventory_capacity', 'Complete native thread summaries exceed their byte capacity.'); threads.set(thread.id, summary);
      requireThat(threads.size <= 5000, 'native_inventory_capacity', 'Complete native thread inventory exceeds its declared capacity.');
      return summary;
    };
    // Periodic inventory must not rescan and repair multi-gigabyte rollout history.
    // The native state DB is the bounded operational index; explicit maintenance
    // owns any filesystem repair separately from this once-per-minute observation.
    for (const archived of [false, true]) await pages('thread/list', { archived, sourceKinds: this.sourceKinds, modelProviders: [],
      sortKey: 'recency_at', sortDirection: 'desc', useStateDbOnly: true }, 5000, value => add(value, archived));
    // Ephemeral threads need not appear in persisted history, but are still owned by this connection.
    for (const id of before) if (!threads.has(id)) add((await read('thread/read', { threadId: id, includeTurns: false }))['thread']!, false);
    const after = await loaded();
    requireThat(before.size === after.size && [...before].every(id => after.has(id)), 'native_inventory_changed', 'Loaded native ownership changed during enumeration; retain the previous snapshot.');
    const observedAt = new Date().toISOString();
    const result: NativeThreadSnapshot = { observedAt, threads: [...threads.values()].map(summary => ({ nativeId: summary.nativeId, summary, observedAt })) };
    canonical(result, 9 * 1024 * 1024); check(); return result;
  }
  currentProjects(observedAt = new Date().toISOString()): NativeProjectSnapshot {
    const source = 'native' as const;
    const projects = structuredClone(this.nativeProjects);
    requireThat(new Set(projects.map(value => value.nativeId)).size === projects.length, 'native_inventory_invalid', 'Native project inventory contains duplicate IDs.');
    const result: NativeProjectSnapshot = { observedAt, projects: { source, observedAt, projects } };
    validateAgent('ProjectsResult', result.projects); canonical(result, 9 * 1024 * 1024); return result;
  }
  async collectProjects(signal?: AbortSignal): Promise<NativeProjectSnapshot> {
    requireThat(this.rpc.catalog.clientRequests.some(value => value.method === 'project/list'),
      'native_inventory_unavailable', 'The connected Codex instance does not support project/list.');
    // Desktop project enumeration may revisit old rollout metadata. It runs independently
    // from the minute task snapshot and therefore receives its own bounded budget.
    const { check, pages } = this.reader(signal, 600_000), source = 'native' as const;
    const projects = await pages('project/list', { sortKey: 'position', sortDirection: 'asc' }, 256, value => {
      const project = fields(value), roots = project['roots'], position = project['position'], recencyAt = project['recencyAt'];
      requireThat(typeof project['id'] === 'string' && typeof project['name'] === 'string' && Array.isArray(roots),
        'native_inventory_invalid', 'Native project identity, name and roots are required.');
      requireThat(Number.isSafeInteger(position) && Number(position) >= 0 && (recencyAt === undefined || recencyAt === null || Number.isSafeInteger(recencyAt) && Number(recencyAt) >= 0),
        'native_inventory_invalid', 'Native project position and recency must be bounded integers.');
      const paths = roots.map(root => fields(root)['path']);
      requireThat(paths.every(path => typeof path === 'string'), 'native_inventory_invalid', 'Native project roots must contain paths.');
      const summary: Agent.ProjectSummary = { nativeId: project['id'], source, name: project['name'], paths: paths as string[],
        position: Number(position), recencyAt: recencyAt == null ? null : Number(recencyAt) };
      validateAgent('ProjectSummary', summary); return summary;
    }) as Agent.ProjectSummary[];
    this.nativeProjects = projects; check(); return this.currentProjects();
  }
  async collect(signal?: AbortSignal): Promise<NativeSnapshot> {
    const threads = await this.collectThreads(signal), projects = await this.collectProjects(signal);
    return { observedAt: threads.observedAt > projects.observedAt ? threads.observedAt : projects.observedAt,
      threads: threads.threads, projects: projects.projects };
  }
}

/** Only an explicit, unexecuted conflict permits advancing above Hive's authoritative revision. */
async function publishNativeKind(connection: RpcClient, journal: NativeJournal, kind: 'thread' | 'project', entries: Wire.InventoryEntry[], signal?: AbortSignal): Promise<void> {
    let minimum = 0;
    for (let attempt = 0; attempt < 2; attempt++) {
      signal?.throwIfAborted(); const snapshotRevision = journal.inventoryRevision(kind, minimum);
      try {
        await connection.request('inventory.sync', { namespace: 'codex', kind, schemaVersion: kind === 'project' ? nativeProjectSchemaVersion : nativeInventorySchemaVersion, mode: 'snapshot', snapshotRevision, entries }, { ...(signal ? { signal } : {}) }); break;
      } catch (error) {
        if (!(error instanceof IvyError) || error.code !== 'revision_conflict' || error.outcome !== 'not_executed' || attempt !== 0) throw error;
        validateShared('InventoryRevisionConflictDetails', error.details); const details = error.details as Wire.InventoryRevisionConflictDetails;
        requireThat(details.serviceNodeId === journal.owner.serviceNodeId && details.namespace === 'codex' && details.kind === kind,
          'target_conflict', 'Inventory conflict metadata belongs to a different owner or kind.'); minimum = details.currentRevision;
      }
    }
}
export async function publishNativeThreads(connection: RpcClient, journal: NativeJournal, snapshot: NativeThreadSnapshot, signal?: AbortSignal): Promise<void> {
  await publishNativeKind(connection, journal, 'thread', snapshot.threads, signal);
}
export async function publishNativeProjects(connection: RpcClient, journal: NativeJournal, snapshot: NativeProjectSnapshot, signal?: AbortSignal): Promise<void> {
  await publishNativeKind(connection, journal, 'project', snapshot.projects.projects.map(summary => ({ nativeId: summary.nativeId, summary, observedAt: snapshot.observedAt })), signal);
}
export async function publishNativeSnapshot(connection: RpcClient, journal: NativeJournal, snapshot: NativeSnapshot, signal?: AbortSignal): Promise<void> {
  await publishNativeThreads(connection, journal, snapshot, signal);
  await publishNativeProjects(connection, journal, { observedAt: snapshot.observedAt, projects: snapshot.projects }, signal);
}
