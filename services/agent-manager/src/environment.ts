import { lstat, mkdir, open, readFile, readdir, realpath, rename, rm, unlink } from 'node:fs/promises';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { atomicJson } from '../../../packages/host-runtime/src/config.js';
import { ExecutorLock } from '../../../packages/host-runtime/src/journal.js';
import { digest, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';
import type { ServiceConnection } from '../../../packages/sdk/src/service.js';

export const mcpContract = 'agent/mcp-configuration', skillsContract = 'agent/skills';
export type EnvironmentKind = 'mcp' | 'skills';
export function environmentName(kind: EnvironmentKind, hostId: string | null): string {
  const base = kind === 'mcp' ? 'ivy-agent-mcp' : 'ivy-agent-skills';
  return hostId === null ? base : base + '-host-' + digest(hostId).slice(7);
}

export interface Versioned<T> { document: T; objectId: string; revision: number; contentHash: string }
export async function readEnvironmentDocument<T extends { hostId: string | null }>(connection: Pick<ServiceConnection, 'request'>, kind: EnvironmentKind,
  hostId: string | null, schema: 'McpDocument' | 'SkillsDocument'): Promise<Versioned<T> | null> {
  const object = await connection.request('objects.stat', { path: '/' + environmentName(kind, hostId) }).catch(error => {
    if (error instanceof IvyError && error.code === 'not_found') return null; throw error;
  });
  if (!object || object.effectivelyArchived) return null;
  const value = await connection.request('objects.read', { objectId: object.id, revision: object.currentRevision });
  requireThat(value.object.contractKey === (kind === 'mcp' ? mcpContract : skillsContract) && value.revision.contractVersion === '1.0.0' && value.content.encoding === 'json',
    'contract_mismatch', 'Managed environment path contains a different document contract.');
  validateAgent(schema, value.content.value);
  const document = value.content.value as unknown as T;
  requireThat(document.hostId === hostId, 'target_conflict', 'Managed environment document belongs to another scope.');
  return { document, objectId: object.id, revision: object.currentRevision, contentHash: value.revision.contentHash };
}

export function selectEnvironmentDocument<T extends { enabled: boolean | null }>(defaults: T, global: Versioned<T> | null, host: Versioned<T> | null) {
  const inherited = global?.document.enabled === null ? defaults : global?.document ?? defaults;
  const document = host?.document.enabled === null ? inherited : host?.document ?? inherited;
  return { document, version: hashJson({ defaults, global, host }) };
}

async function regularBytes(path: string, maximum = 4 * 1024 * 1024): Promise<Buffer | null> {
  try {
    const info = await lstat(path);
    requireThat(info.isFile() && !info.isSymbolicLink() && info.size <= maximum, 'target_conflict', 'Managed target must be a bounded regular file.');
    return await readFile(path);
  } catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error; }
}
const bytesHash = (value: Buffer | null) => value === null ? null : digest(value);
function text(bytes: Buffer | null): string {
  if (!bytes) return '';
  try { return new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
  catch { throw new IvyError('target_conflict', 'Managed Codex configuration is not valid UTF-8.'); }
}
async function replaceFile(path: string, before: Buffer | null, after: Buffer): Promise<void> {
  const temporary = join(dirname(path), '.ivy-environment-' + randomUUID() + '.tmp');
  const file = await open(temporary, 'wx', 0o600);
  try { await file.writeFile(after); await file.sync(); } finally { await file.close(); }
  try {
    requireThat(bytesHash(await regularBytes(path)) === bytesHash(before), 'target_conflict', 'Codex configuration changed during application.');
    await rename(temporary, path);
  } finally { await unlink(temporary).catch(() => undefined); }
}

const beginCodex = '# >>> Ivy AgentManager Codex configuration (managed; edit in AgentUI)';
const endCodex = '# <<< Ivy AgentManager Codex configuration';
const beginMcp = '# >>> Ivy AgentManager MCP configuration (managed; edit in AgentUI)';
const endMcp = '# <<< Ivy AgentManager MCP configuration';
const beginPermissions = '# >>> Ivy AgentManager Codex permissions (managed; edit in AgentUI)';
const endPermissions = '# <<< Ivy AgentManager Codex permissions';
const hostedIvyAppId = 'asdk_app_6aaab03ab3f48191a8e0e4dbdc434e5d';
const approvalPolicy = 'never', defaultPermissions = ':danger-full-access';
function managedBlock(source: string, beginMarker: string, endMarker: string): { block: string | null; outside: string } {
  const begin = source.indexOf(beginMarker), end = source.indexOf(endMarker);
  requireThat((begin < 0) === (end < 0) && (begin < 0 || end > begin) && source.indexOf(beginMarker, begin + 1) < 0 && source.indexOf(endMarker, end + 1) < 0,
    'target_conflict', 'Codex configuration has malformed Ivy management markers.');
  if (begin < 0) return { block: null, outside: source };
  const through = end + endMarker.length;
  return { block: source.slice(begin, through), outside: source.slice(0, begin) + source.slice(through) };
}
function codexBlock(source: string, acceptedHashes: ReadonlySet<string>): { block: string | null; outside: string } {
  const begin = source.indexOf(beginCodex), end = source.indexOf(endCodex);
  if (begin < 0 && end < 0) return { block: null, outside: source };
  requireThat(begin >= 0 && (end < 0 || end > begin) && source.indexOf(beginCodex, begin + 1) < 0 &&
    source.indexOf(endCodex, end + 1) < 0, 'target_conflict', 'Codex configuration has malformed Ivy management markers.');
  const through = end < 0 ? source.length : end + endCodex.length;
  const current = end < 0 ? null : source.slice(begin, through);
  if (current && acceptedHashes.has(digest(current)))
    return { block: current, outside: source.slice(0, begin) + source.slice(through) };

  // Codex may write its own root keys or MCP tables inside our marker span. Recover
  // only when the remaining Ivy entries reconstruct the exact previously applied block.
  const region = source.slice(begin + beginCodex.length, end < 0 ? source.length : end);
  const positions = [...region.matchAll(/^[ \t]*\[\[?/gm)].map(match => match.index!);
  requireThat(positions.length > 0, 'target_conflict', 'The Ivy-managed Codex block has local changes.');
  const root = region.slice(0, positions[0]);
  const permissionLine = (key: string) => new RegExp(`^[ \\t]*${key}[ \\t]*=[^\\r\\n]*(?:\\r?\\n|$)`, 'gm');
  const approvalLines = [...root.matchAll(permissionLine('approval_policy'))];
  const defaultLines = [...root.matchAll(permissionLine('default_permissions'))];
  requireThat(approvalLines.length === 1 && approvalLines[0]![0].trim() === `approval_policy = ${tomlString(approvalPolicy)}` &&
    defaultLines.length === 1 && defaultLines[0]![0].trim() === `default_permissions = ${tomlString(defaultPermissions)}`,
    'target_conflict', 'The Ivy-managed Codex permissions have local changes.');
  const extraRoot = root.replace(permissionLine('approval_policy'), '').replace(permissionLine('default_permissions'), '');
  const rawTables = positions.map((position, index) => region.slice(position, positions[index + 1] ?? region.length));
  requireThat(rawTables[0]!.trimStart().startsWith(`[apps.${tomlString(hostedIvyAppId)}]`),
    'target_conflict', 'The Ivy-managed Codex app has local changes.');
  const tables = rawTables.map(table => table.trim());
  for (let serverCount = 0; serverCount < rawTables.length; serverCount++) {
    const candidate = [beginCodex, `approval_policy = ${tomlString(approvalPolicy)}`,
      `default_permissions = ${tomlString(defaultPermissions)}`, '', tables[0]!, '',
      ...tables.slice(1, serverCount + 1), endCodex].join('\n');
    if (!acceptedHashes.has(digest(candidate))) continue;
    return { block: candidate, outside: source.slice(0, begin) + extraRoot +
      rawTables.slice(serverCount + 1).join('') + source.slice(through) };
  }
  throw new IvyError('target_conflict', 'The Ivy-managed Codex block has local changes.');
}
const tomlString = (value: string) => JSON.stringify(value);
function renderMcpSections(document: Agent.McpDocument, hiveMcpUrls: ReadonlySet<string>, credential: string): string[] | null {
  if (!document.enabled) return null;
  const names = new Set<string>();
  const blocks = document.servers.map(server => {
    requireThat(!names.has(server.name), 'invalid_arguments', 'Managed MCP server names must be unique.'); names.add(server.name);
    const url = new URL(server.url), loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
    requireThat((url.protocol === 'https:' || url.protocol === 'http:' && loopback) && !url.username && !url.password && !url.hash,
      'invalid_arguments', 'Managed remote MCP URLs require HTTPS.');
    if (server.authentication === 'agent-manager') requireThat(hiveMcpUrls.has(url.href), 'access_denied', 'AgentManager credentials can only be sent to this Hive MCP resource.');
    return [`[mcp_servers.${tomlString(server.name)}]`, `url = ${tomlString(url.href)}`,
      ...(server.authentication === 'agent-manager' ? [`http_headers = { Authorization = ${tomlString('Bearer ' + credential)} }`] : []),
      `enabled = ${server.enabled}`, `startup_timeout_sec = ${server.startupTimeoutSeconds}`, `tool_timeout_sec = ${server.toolTimeoutSeconds}`].join('\n');
  });
  return [`[apps.${tomlString(hostedIvyAppId)}]`, 'enabled = false', '', ...blocks];
}
function renderCodex(sections: string[] | null): string | null {
  if (!sections) return null;
  return [beginCodex, `approval_policy = ${tomlString(approvalPolicy)}`,
    `default_permissions = ${tomlString(defaultPermissions)}`, '', ...sections, endCodex].join('\n');
}
function withBlock(source: string, block: string | null): string {
  const bom = source.startsWith('\uFEFF') ? '\uFEFF' : '';
  const outside = source.slice(bom.length).replace(/(?:\r?\n){3,}/g, '\n\n').trim();
  const firstTable = outside.search(/^[ \t]*\[\[?/m);
  const root = (firstTable < 0 ? outside : outside.slice(0, firstTable)).trimEnd();
  const tables = firstTable < 0 ? '' : outside.slice(firstTable).trimStart();
  const parts = [root, block, tables].filter((part): part is string => Boolean(part));
  return bom + parts.join('\n\n') + (parts.length ? '\n' : '');
}
function serverSections(source: string): Set<string> {
  const names = new Set<string>();
  for (const match of source.matchAll(/^\s*\[mcp_servers\.(?:"([a-z][a-z0-9_-]{0,63})"|([a-z][a-z0-9_-]{0,63}))(?:\.|\])/gmi)) names.add(match[1] ?? match[2]!);
  return names;
}
function hasAppSection(source: string, appId: string): boolean {
  const escaped = appId.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`^\\s*\\[apps\\.(?:"${escaped}"|'${escaped}'|${escaped})(?:\\.|\\])`, 'm').test(source);
}
function rootConfigurationKeys(source: string): Set<string> {
  const firstTable = source.search(/^[ \t]*\[\[?/m), root = firstTable < 0 ? source : source.slice(0, firstTable), keys = new Set<string>();
  for (const match of root.matchAll(/^\s*(?:"(approval_policy|default_permissions|sandbox_mode|sandbox_workspace_write)"|'(approval_policy|default_permissions|sandbox_mode|sandbox_workspace_write)'|(approval_policy|default_permissions|sandbox_mode|sandbox_workspace_write))\s*=/gm))
    keys.add(match[1] ?? match[2] ?? match[3]!);
  return keys;
}
function adoptMatchingPermissions(source: string): string {
  const firstTable = source.search(/^[ \t]*\[\[?/m), boundary = firstTable < 0 ? source.length : firstTable;
  const root = source.slice(0, boundary), expected: Record<string, string> = {
    approval_policy: approvalPolicy, default_permissions: defaultPermissions,
  };
  const seen = new Set<string>();
  const withoutMatching = root.replace(/^([ \t]*)(?:"(approval_policy|default_permissions)"|'(approval_policy|default_permissions)'|(approval_policy|default_permissions))[ \t]*=[ \t]*([^\r\n]*)(\r?\n|$)/gm,
    (_line, _indent: string, quoted: string | undefined, literal: string | undefined, plain: string | undefined, value: string) => {
      const key = quoted ?? literal ?? plain!;
      requireThat(!seen.has(key) && (value.trim() === JSON.stringify(expected[key]) || value.trim() === `'${expected[key]}'`),
        'target_conflict', `Codex ${key} already has a different or ambiguous unmanaged configuration.`);
      seen.add(key);
      return '';
    });
  return withoutMatching + source.slice(boundary);
}
function hasLegacySandboxTable(source: string): boolean {
  return /^\s*\[sandbox_workspace_write(?:\.|\])/m.test(source);
}
interface LegacyMcpState { schemaVersion: 1; authority: string; appliedBlockHash: string | null;
  appliedPermissionsBlockHash?: string | null; appliedVersion: string | null }
interface McpState { schemaVersion: 2; authority: string; appliedBlockHash: string | null; appliedVersion: string | null }

export class McpConfigurationManager {
  status: Agent.ManagedOutputStatus;
  private running: Promise<void> | null = null;
  constructor(readonly home: string, readonly hostId: string, readonly hiveUrl: string, readonly credential: string,
    readonly defaults: Agent.McpDocument) {
    this.status = { state: 'pending', target: join(home, 'config.toml'), desiredVersion: null, appliedVersion: null,
      observedAt: new Date().toISOString(), code: null, bytes: 0 };
  }
  synchronize(connection: Pick<ServiceConnection, 'request'>): Promise<void> {
    if (!this.running) this.running = this.apply(connection).catch(error => {
      const code = IvyError.from(error).code;
      this.status = { ...this.status, state: ['target_conflict', 'limit_exceeded', 'contract_mismatch', 'access_denied', 'invalid_arguments'].includes(code) ? 'conflict' : 'pending', observedAt: new Date().toISOString(), code };
    }).finally(() => { this.running = null; });
    return this.running;
  }
  bootstrap(): Promise<void> {
    return this.synchronize({ request: async () => { throw new IvyError('not_found', 'No stored environment override.'); } } as unknown as Pick<ServiceConnection, 'request'>);
  }
  async close(): Promise<void> { await this.running; }
  private async apply(connection: Pick<ServiceConnection, 'request'>): Promise<void> {
    const actualHome = await realpath(this.home), target = join(actualHome, 'config.toml'), stateRoot = join(actualHome, '.ivy-agent-mcp');
    const global = await readEnvironmentDocument<Agent.McpDocument>(connection, 'mcp', null, 'McpDocument');
    const host = await readEnvironmentDocument<Agent.McpDocument>(connection, 'mcp', this.hostId, 'McpDocument');
    const desired = selectEnvironmentDocument(this.defaults, global, host), hiveMcpUrls = new Set(['/mcp', '/mcp-dev'].map(path => new URL(this.hiveUrl.replace(/\/$/, '') + path).href));
    const sections = renderMcpSections(desired.document, hiveMcpUrls, this.credential);
    const block = renderCodex(sections), blockHash = block === null ? null : digest(block);
    const legacyMcpBlock = sections === null ? null : [beginMcp, ...sections, endMcp].join('\n');
    const legacyPermissionsBlock = sections === null ? null : [beginPermissions,
      `approval_policy = ${tomlString(approvalPolicy)}`, `default_permissions = ${tomlString(defaultPermissions)}`, endPermissions].join('\n');
    const desiredVersion = hashJson({ document: desired.version, approvalPolicy, defaultPermissions, hostedIvyAppId });
    await mkdir(stateRoot, { recursive: true, mode: 0o700 });
    const rootInfo = await lstat(stateRoot);
    requireThat(rootInfo.isDirectory() && !rootInfo.isSymbolicLink(), 'target_conflict', 'Managed MCP state must be a real directory.');
    const lock = new ExecutorLock(stateRoot);
    try {
    const statePath = join(stateRoot, 'state.json'), rawState = await regularBytes(statePath);
    const authority = hashJson({ hive: this.hiveUrl, hostId: this.hostId });
    const state = rawState ? JSON.parse(rawState.toString('utf8')) as LegacyMcpState | McpState : null;
    requireThat(!state || (state.schemaVersion === 1 || state.schemaVersion === 2) && state.authority === authority,
      'target_conflict', 'Another Hive or host owns this Codex MCP output.');
    const before = await regularBytes(target), source = text(before);
    const current = codexBlock(source, new Set([state?.appliedBlockHash, blockHash].filter((hash): hash is string => Boolean(hash))));
    const oldMcp = managedBlock(current.outside, beginMcp, endMcp);
    const oldPermissions = managedBlock(oldMcp.outside, beginPermissions, endPermissions);
    requireThat(current.block === null || oldMcp.block === null && oldPermissions.block === null,
      'target_conflict', 'Codex configuration contains both current and legacy Ivy blocks.');
    requireThat(!state || state.schemaVersion !== 2 || oldMcp.block === null && oldPermissions.block === null,
      'target_conflict', 'Codex configuration has unexpected legacy Ivy blocks.');
    if (state?.schemaVersion === 1 && current.block !== null) {
      requireThat(digest(current.block) === blockHash, 'target_conflict', 'The Ivy-managed Codex block has local changes.');
    } else if (state?.schemaVersion === 1) {
      const oldMcpHash = oldMcp.block === null ? null : digest(oldMcp.block);
      const oldPermissionsHash = oldPermissions.block === null ? null : digest(oldPermissions.block);
      requireThat(oldMcpHash === state.appliedBlockHash || oldMcpHash === (legacyMcpBlock === null ? null : digest(legacyMcpBlock)),
        'target_conflict', 'The Ivy-managed MCP block has local changes.');
      requireThat(oldPermissionsHash === (state.appliedPermissionsBlockHash ?? null) ||
        oldPermissionsHash === (legacyPermissionsBlock === null ? null : digest(legacyPermissionsBlock)),
        'target_conflict', 'The Ivy-managed permission block has local changes.');
    } else if (state?.schemaVersion === 2) {
      const currentHash = current.block === null ? null : digest(current.block);
      requireThat(currentHash === state.appliedBlockHash || currentHash === blockHash,
        'target_conflict', 'The Ivy-managed Codex block has local changes.');
    }
    let unmanaged = oldPermissions.outside;
    if (block) {
      const conflicts = serverSections(unmanaged);
      for (const server of desired.document.servers) requireThat(!conflicts.has(server.name), 'target_conflict', `MCP server ${server.name} already has an unmanaged configuration.`);
      requireThat(!hasAppSection(unmanaged, hostedIvyAppId), 'target_conflict', 'The hosted Ivy app already has an unmanaged configuration.');
    }
    if (block) {
      const keys = rootConfigurationKeys(unmanaged);
      for (const key of ['sandbox_mode', 'sandbox_workspace_write'])
        requireThat(!keys.has(key), 'target_conflict', `Codex ${key} already has an unmanaged configuration.`);
      requireThat(!hasLegacySandboxTable(unmanaged), 'target_conflict', 'Codex sandbox_workspace_write conflicts with the managed permission profile.');
      unmanaged = adoptMatchingPermissions(unmanaged);
    }
    const after = Buffer.from(withBlock(unmanaged, block));
    if (!before?.equals(after)) await replaceFile(target, before, after);
    await atomicJson(statePath, { schemaVersion: 2, authority, appliedBlockHash: blockHash,
      appliedVersion: desiredVersion } satisfies McpState);
    this.status = { state: block ? 'applied' : 'disabled', target, desiredVersion, appliedVersion: desiredVersion,
      observedAt: new Date().toISOString(), code: null, bytes: block ? Buffer.byteLength(block) : 0 };
    } finally { lock.close(); }
  }
}

interface SkillsState { schemaVersion: 1 | 2; authority: string; applied: Record<string, string>; appliedVersion: string | null }
const skillNamePattern = /^[a-z][a-z0-9-]{0,63}$/;
function normalizeSkills(document: Agent.SkillsDocument): Map<string, Map<string, Buffer>> {
  const result = new Map<string, Map<string, Buffer>>(), seen = new Set<string>(); let bytes = 0;
  if (!document.enabled) return result;
  for (const file of document.files) {
    requireThat(!file.path.includes('\\') && !file.path.startsWith('/') && !file.path.endsWith('/') && !file.path.split('/').some(part => !part || part === '.' || part === '..') && !isAbsolute(file.path),
      'invalid_arguments', 'Managed skill paths must be relative slash-separated file paths.');
    const [skill, ...parts] = file.path.split('/');
    requireThat(parts.length > 0 && skillNamePattern.test(skill!), 'invalid_arguments', 'Managed skill files require a safe top-level skill directory.');
    requireThat(!seen.has(file.path), 'invalid_arguments', 'Managed skill file paths must be unique.'); seen.add(file.path);
    const content = Buffer.from(file.content); bytes += content.length;
    requireThat(content.length <= 256 * 1024 && bytes <= 4 * 1024 * 1024, 'limit_exceeded', 'Managed skill files exceed their bounded size.');
    const group = result.get(skill!) ?? new Map<string, Buffer>(); group.set(parts.join('/'), content); result.set(skill!, group);
  }
  for (const [skill, files] of result) requireThat(files.has('SKILL.md'), 'invalid_arguments', `Managed skill ${skill} requires SKILL.md.`);
  return result;
}
async function treeHash(path: string): Promise<string | null> {
  const info = await lstat(path).catch(error => { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error; });
  if (!info) return null;
  requireThat(info.isDirectory() && !info.isSymbolicLink(), 'target_conflict', 'Managed skill target must be a real directory.');
  const entries: Array<[string, string]> = [];
  const visit = async (directory: string, prefix = ''): Promise<void> => {
    for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      requireThat(!entry.isSymbolicLink(), 'target_conflict', 'Managed skills must not contain filesystem links.');
      const local = prefix ? prefix + '/' + entry.name : entry.name, child = join(directory, entry.name);
      if (entry.isDirectory()) await visit(child, local);
      else { requireThat(entry.isFile(), 'target_conflict', 'Managed skills must contain regular files only.'); const value = await regularBytes(child, 256 * 1024); requireThat(value, 'target_conflict', 'Managed skill file disappeared during inspection.'); entries.push([local, digest(value)]); }
    }
  };
  await visit(path); return hashJson(entries);
}
const legacySkillName = (name: string) => 'ivy-managed-' + name;

export class SkillsManager {
  status: Agent.ManagedOutputStatus;
  private running: Promise<void> | null = null;
  constructor(readonly stateHome: string, readonly skillsRoot: string, readonly hostId: string, readonly hiveUrl: string,
    readonly defaults: Agent.SkillsDocument) {
    this.status = { state: 'pending', target: skillsRoot, desiredVersion: null, appliedVersion: null, observedAt: new Date().toISOString(), code: null, bytes: 0 };
  }
  synchronize(connection: Pick<ServiceConnection, 'request'>): Promise<void> {
    if (!this.running) this.running = this.apply(connection).catch(error => {
      const code = IvyError.from(error).code;
      this.status = { ...this.status, state: ['target_conflict', 'limit_exceeded', 'contract_mismatch', 'invalid_arguments'].includes(code) ? 'conflict' : 'pending', observedAt: new Date().toISOString(), code };
    }).finally(() => { this.running = null; });
    return this.running;
  }
  async bootstrap(): Promise<void> {
    requireThat(isAbsolute(this.skillsRoot), 'invalid_arguments', 'Managed skills root must be absolute.');
    const root = resolve(this.skillsRoot);
    const statePath = join(dirname(root), '.ivy-agent-skills-' + digest(root).slice(7, 23), 'state.json');
    const legacyPath = join(await realpath(this.stateHome), '.ivy-agent-skills', 'state.json');
    // An existing installation keeps its last skills until Hive resolves global and host overrides.
    if (await regularBytes(statePath) || await regularBytes(legacyPath)) return;
    await this.synchronize({ request: async () => { throw new IvyError('not_found', 'No stored environment override.'); } } as unknown as Pick<ServiceConnection, 'request'>);
  }
  async close(): Promise<void> { await this.running; }
  private async apply(connection: Pick<ServiceConnection, 'request'>): Promise<void> {
    const global = await readEnvironmentDocument<Agent.SkillsDocument>(connection, 'skills', null, 'SkillsDocument');
    const host = await readEnvironmentDocument<Agent.SkillsDocument>(connection, 'skills', this.hostId, 'SkillsDocument');
    const desired = selectEnvironmentDocument(this.defaults, global, host), groups = normalizeSkills(desired.document);
    requireThat(isAbsolute(this.skillsRoot), 'invalid_arguments', 'Managed skills root must be absolute.');
    const root = resolve(this.skillsRoot), legacyStateRoot = join(await realpath(this.stateHome), '.ivy-agent-skills');
    await mkdir(root, { recursive: true, mode: 0o700 });
    requireThat((await lstat(root)).isDirectory() && !(await lstat(root)).isSymbolicLink(), 'target_conflict', 'Managed skills root must be a real directory.');
    const stateRoot = join(dirname(root), '.ivy-agent-skills-' + digest(root).slice(7, 23));
    await mkdir(stateRoot, { recursive: true, mode: 0o700 });
    const stateInfo = await lstat(stateRoot);
    requireThat(stateInfo.isDirectory() && !stateInfo.isSymbolicLink(), 'target_conflict', 'Managed skill state must be a real directory.');
    const lock = new ExecutorLock(stateRoot);
    try {
    const statePath = join(stateRoot, 'state.json'), authority = hashJson({ hive: this.hiveUrl, hostId: this.hostId });
    let raw = await regularBytes(statePath);
    if (!raw) {
      const legacyInfo = await lstat(legacyStateRoot).catch(error => {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error;
      });
      if (legacyInfo) {
        requireThat(legacyInfo.isDirectory() && !legacyInfo.isSymbolicLink(), 'target_conflict', 'Legacy managed skill state must be a real directory.');
        raw = await regularBytes(join(legacyStateRoot, 'state.json'));
      }
    }
    const state = raw ? JSON.parse(raw.toString('utf8')) as SkillsState : null;
    requireThat(!state || (state.schemaVersion === 1 || state.schemaVersion === 2) && state.authority === authority, 'target_conflict', 'Another Hive or host owns these managed skills.');
    const before = state?.applied ?? {}, legacy = state?.schemaVersion === 1;
    const desiredNames = new Set(groups.keys());
    for (const [name, hash] of Object.entries(before)) {
      const skill = legacy && name.startsWith('ivy-managed-') ? name.slice('ivy-managed-'.length) : legacy ? '' : name;
      requireThat(skillNamePattern.test(skill), 'target_conflict', 'Managed skill state contains an invalid directory name.');
      if (desiredNames.has(skill)) requireThat(await treeHash(join(root, name)) === hash, 'target_conflict', `Managed skill ${name} has local changes.`);
    }
    const after: Record<string, string> = {}, stage = join(stateRoot, '.stage-' + randomUUID()); await mkdir(stage);
    try {
      for (const [skill, files] of groups) {
        const name = skill, directory = join(stage, name); await mkdir(directory, { recursive: true });
        for (const [local, content] of files) { const path = join(directory, ...local.split('/')); await mkdir(dirname(path), { recursive: true }); const file = await open(path, 'wx', 0o600); try { await file.writeFile(content); } finally { await file.close(); } }
        after[name] = (await treeHash(directory))!;
      }
      for (const name of Object.keys(after)) {
        const sourceName = legacy ? legacySkillName(name) : name;
        if (sourceName !== name || !Object.hasOwn(before, sourceName)) requireThat(await treeHash(join(root, name)) === null, 'target_conflict', `Skill directory ${name} already exists outside Ivy management.`);
      }
      for (const name of Object.keys(after)) {
        const sourceName = legacy ? legacySkillName(name) : name, owned = Object.hasOwn(before, sourceName);
        if (sourceName === name && before[name] === after[name]) { await rm(join(stage, name), { recursive: true, force: true }); continue; }
        const target = join(root, name), source = join(root, sourceName), retired = join(stateRoot, '.retired-' + randomUUID());
        if (owned) await rename(source, retired);
        try { await rename(join(stage, name), target); }
        catch (error) { if (owned) await rename(retired, source); throw error; }
        if (owned) await rm(retired, { recursive: true, force: true });
      }
    } finally { await rm(stage, { recursive: true, force: true }); }
    await atomicJson(statePath, { schemaVersion: 2, authority, applied: after, appliedVersion: desired.version } satisfies SkillsState);
    const bytes = [...groups.values()].flatMap(files => [...files.values()]).reduce((sum, value) => sum + value.length, 0);
    this.status = { state: groups.size ? 'applied' : 'disabled', target: root, desiredVersion: desired.version, appliedVersion: desired.version,
      observedAt: new Date().toISOString(), code: null, bytes };
    } finally { lock.close(); }
  }
}
