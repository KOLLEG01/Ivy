import { hashJson, canonical, digest } from '../../../packages/contracts/src/canonical.js';
import { bundledSchema, operationInputSchema } from '../../../packages/contracts/src/core-bundle.js';
import { operations, operationSchema } from '../../../packages/contracts/src/core-validation.js';
import { requireThat } from '../../../packages/contracts/src/errors.js';
import type { Operation, Wire } from '../../../packages/contracts/src/generated.js';
import { toolDefinitionHash } from '../../../packages/contracts/src/tool-definition.js';
import { composeHiveMcpInstructions, hiveMcpExamples } from '../../../instructions/hive-mcp.js';
import { coreMcpNames, publicCoreMcpMethods } from './core-mcp.js';
import { directDescriptors } from './mcp-catalog.js';
import type { Registry, RegistrySync, ToolBinding, ServiceNode } from './registry.js';
import { compactDescription, coreDescriptions, coreGroups, coreGuide, coreServiceDescription, defaultGroupDescription, searchWords } from './discovery-metadata.js';

type Entry = Operation.DiscoveryEntry;
type ListInput = Operation.DiscoveryListParams;
type IndexedTool = { qualifiedName: string; namespace: string; group: string; groupDescription: string; description: string; effect: 'read' | 'write' | 'unknown'; words: string[]; mcp?: NonNullable<Wire.ToolDefinition['discovery']>['mcp'] };
type Index = { source: string; identity: string; tools: IndexedTool[]; groups: ReadonlyMap<string, string>; description: string; bytes: number };
const coreMethods = Object.keys(operations).filter(name => operations[name]!.access === 'client' && operations[name]!.discoverable !== false && !/^(discovery|namespaces|tools)\./.test(name));
const coreBindings = new Map<string, ToolBinding>();
function coreBinding(qualifiedName: string): ToolBinding {
  const method = qualifiedName.slice('hive.'.length);
  requireThat(qualifiedName.startsWith('hive.') && coreMethods.includes(method), 'not_found', 'Unknown discoverable Hive operation.');
  let binding = coreBindings.get(method);
  if (!binding) {
    const mcpName = coreMcpNames[method];
    const inputSchema = operationInputSchema(method), readOnly = !operations[method]!.mutation;
    const definition: ToolBinding['definition'] = { namespace: 'hive', name: method, interfaceVersion: '1.0.0', description: coreDescriptions[method] ?? method,
      discovery: { ...(mcpName ? { mcp: { name: mcpName, surface: publicCoreMcpMethods.has(method) ? 'ivy' : 'ivy_dev' } } : {}) },
      inputSchema, outputSchema: bundledSchema(operations[method]!.output, String(operationSchema['$id'])),
      // Hive mutation IDs retain receipts or reject expired requests; token minting has no such identity.
      annotations: { readOnlyHint: readOnly, idempotentHint: readOnly || ((inputSchema['required'] ?? []) as string[]).includes('mutationId') } };
    binding = { qualifiedName, definition, definitionHash: toolDefinitionHash(definition) }; coreBindings.set(method, binding);
  }
  return binding;
}
export function resolveCoreCall(params: Operation.DiscoveryCallParams): { method: string; params: Record<string, unknown> } {
  requireThat(!params.serviceNodeId && !params.operationId, 'invalid_arguments', 'Hive core uses its declared arguments and mutationId, not a service node or outer operationId.');
  const binding = coreBinding(params.qualifiedName);
  requireThat(binding.definitionHash === params.expectedDefinitionHash, 'tool_definition_changed', 'The retained Hive definition changed; describe and reassess.');
  requireThat(params.arguments !== null && typeof params.arguments === 'object' && !Array.isArray(params.arguments), 'invalid_arguments', 'Hive operation arguments must be an object.');
  return { method: binding.definition.name, params: params.arguments as Record<string, Wire.Json> };
}
function indexed(namespace: string, name: string, description: string, readOnly?: boolean,
  discovery?: ToolBinding['definition']['discovery'], groups: ReadonlyMap<string, string> = new Map()): IndexedTool {
  const group = discovery?.group ?? namespace, groupDescription = groups.get(group) ?? defaultGroupDescription(group), summary = compactDescription(discovery?.summary ?? description);
  return { qualifiedName: namespace + '.' + name, namespace, group, groupDescription, description: summary,
    effect: readOnly === true ? 'read' : readOnly === false ? 'write' : 'unknown',
    words: searchWords(namespace + ' ' + name + ' ' + summary + ' ' + description + ' ' + groupDescription + ' ' + (discovery?.keywords ?? []).join(' ')) };
}
const coreIndex = coreMethods.map(method => indexed('hive', method, coreDescriptions[method] ?? method, !operations[method]!.mutation,
  { group: method.split('.')[0]! }, coreGroups));

export interface McpBinding extends ToolBinding {
  serviceName: string;
  provider: Operation.DiscoveryProvider | null;
  guideMarkdown: string;
}

function mcpGuide(guide: string, catalog: RegistrySync): string {
  const names = new Map([
    ...Object.entries(coreMcpNames).map(([method, name]) => ['hive.' + method, name + (publicCoreMcpMethods.has(method) ? '' : ' (ivy_dev only)')] as const),
    ...catalog.namespaces.flatMap(ns => ns.tools.flatMap(tool => tool.discovery?.mcp
      ? [[ns.namespace + '.' + tool.name, tool.discovery.mcp.name + (tool.discovery.mcp.surface === 'ivy_dev' ? ' (ivy_dev only)' : '')] as const] : [])),
  ]);
  return guide.replace(/[A-Za-z][A-Za-z0-9-]*\.[A-Za-z][A-Za-z0-9./]*/g, value => names.get(value.replace(/\.$/, ''))
    ? names.get(value.replace(/\.$/, ''))! + (value.endsWith('.') ? '.' : '') : value);
}

/** Compact projections are reused until the committed catalog changes; live readiness is never cached. */
export class Discovery {
  private readonly indexes = new Map<string, Index>();
  private indexBytes = 0;
  constructor(private readonly registry: Registry) {}
  instructions(): Operation.DiscoveryInstructionsResult {
    const catalog = this.mcpCatalog();
    const guides = catalog.filter(binding => binding.provider && binding.guideMarkdown).map(binding =>
      `${binding.serviceName}/${binding.definition.namespace}: ${binding.guideMarkdown}`);
    return { instructions: composeHiveMcpInstructions(this.registry.nodes().filter(node => this.index(node).tools.some(tool => tool.mcp)).map(node => ({
      serviceName: node.serviceName,
      discoveryHint: this.registry.getRegistry(node.serviceNodeId)?.discoveryHint,
    })), guides) };
  }
  instructionsPage(params: Operation.SystemInstructionsParams): Operation.SystemInstructionsResult {
    const view = params.view ?? 'instructions';
    const bytes = Buffer.from(view === 'examples' ? hiveMcpExamples : this.instructions().instructions, 'utf8');
    const sha256 = digest(bytes);
    let startByte = 0;
    if (params.cursor) {
      const match = /^(sha256:[0-9a-f]{64}):(\d{1,5})$/.exec(params.cursor);
      requireThat(match && match[1] === sha256, 'invalid_cursor', 'Instructions changed; restart from the first page.');
      startByte = Number(match[2]);
      requireThat(Number.isSafeInteger(startByte) && startByte > 0 && startByte < bytes.length &&
        (bytes[startByte]! & 0xc0) !== 0x80, 'invalid_cursor', 'Invalid instructions page boundary.');
    }
    let endByte = Math.min(bytes.length, startByte + 8192);
    while (endByte < bytes.length && (bytes[endByte]! & 0xc0) === 0x80) endByte--;
    return {
      view,
      text: bytes.toString('utf8', startByte, endByte),
      sha256,
      totalBytes: bytes.length,
      startByte,
      endByte,
      complete: startByte === 0 && endByte === bytes.length,
      nextCursor: endByte < bytes.length ? `${sha256}:${endByte}` : null,
    };
  }
  toolSchema(params: Operation.SystemToolSchemaParams): Operation.SystemToolSchemaResult {
    const bindings = this.mcpCatalog(params.name).filter(binding => binding.definition.discovery?.mcp?.surface === 'ivy');
    const tool = directDescriptors(bindings).find(candidate => candidate.name === params.name);
    requireThat(tool, 'not_found', 'Public MCP tool is not in the current catalog.');
    return {
      name: tool.name,
      description: tool.description ?? '',
      inputSchema: tool.inputSchema as Wire.Json,
      outputSchema: (tool.outputSchema ?? null) as Wire.Json,
      schemaHash: hashJson({ inputSchema: tool.inputSchema, outputSchema: tool.outputSchema ?? null }),
      complete: true,
    };
  }
  private index(node: ServiceNode): Index {
    const source = String(this.registry.store.get('SELECT registry_json FROM registries WHERE node_id=?', node.serviceNodeId)?.['registry_json'] ?? '');
    let cached = this.indexes.get(node.serviceNodeId);
    if (cached?.source === source) { this.indexes.delete(node.serviceNodeId); this.indexes.set(node.serviceNodeId, cached); return cached; }
    const catalog = source ? JSON.parse(source) as RegistrySync : null;
    const declaredGroups = new Map(catalog?.namespaces.flatMap(ns => (ns.discoveryGroups ?? []).map(group => [group.group, group.description] as const)) ?? []);
    cached = { source, identity: digest(source), groups: declaredGroups,
      description: compactDescription(catalog?.discoveryHint?.trim() || catalog?.namespaces.map(ns => ns.description).join('; ') || 'No synchronized tool catalog.'),
      tools: catalog?.namespaces.flatMap(ns => {
        const groups = new Map((ns.discoveryGroups ?? []).map(group => [group.group, group.description] as const));
        return ns.tools.map(tool => ({ ...indexed(ns.namespace, tool.name, tool.description, tool.annotations?.readOnlyHint, tool.discovery, groups), ...(tool.discovery?.mcp ? { mcp: tool.discovery.mcp } : {}) }));
      }) ?? [], bytes: 0 };
    cached.bytes = Buffer.byteLength(source) + Buffer.byteLength(JSON.stringify(cached.tools));
    this.evict(node.serviceNodeId);
    while (this.indexes.size && (this.indexBytes + cached.bytes > 16 * 1024 * 1024 || this.indexes.size >= 128)) this.evict(this.indexes.keys().next().value!);
    if (cached.bytes <= 16 * 1024 * 1024) { this.indexes.set(node.serviceNodeId, cached); this.indexBytes += cached.bytes; }
    return cached;
  }
  private evict(id: string): void { this.indexBytes -= this.indexes.get(id)?.bytes ?? 0; this.indexes.delete(id); }
  mcpCatalog(name?: string): McpBinding[] {
    const result: McpBinding[] = [];
    const include = (binding: ToolBinding) => !!binding.definition.discovery?.mcp && (!name || binding.definition.discovery.mcp.name === name);
    const activePolicies = new Map<string, Set<string>>();
    const policy = (definition: ToolBinding['definition']) => {
      const mcp = definition.discovery?.mcp;
      return mcp ? `${mcp.name}\0${mcp.surface ?? 'ivy_dev'}` : '';
    };
    for (const node of this.registry.nodes().filter(value => this.registry.eligible(value))) {
      const catalog = this.registry.getRegistry(node.serviceNodeId);
      if (!catalog) continue;
      for (const ns of catalog.namespaces) for (const definition of ns.tools) {
        const key = `${node.serviceName}\0${ns.namespace}.${definition.name}`;
        const policies = activePolicies.get(key) ?? new Set<string>();
        policies.add(policy(definition)); activePolicies.set(key, policies);
      }
    }
    for (const method of coreMethods) {
      const binding = coreBinding('hive.' + method);
      if (include(binding)) result.push({ ...binding, serviceName: 'hive', provider: null, guideMarkdown: '' });
    }
    for (const node of this.registry.nodes()) {
      const catalog = this.registry.getRegistry(node.serviceNodeId);
      if (!catalog) continue;
      for (const ns of catalog.namespaces) {
        const guideMarkdown = mcpGuide(ns.guideMarkdown, catalog);
        for (const binding of this.registry.bindings(node.serviceNodeId, ns)) {
          const available = this.registry.eligible(node), current = activePolicies.get(`${node.serviceName}\0${binding.qualifiedName}`);
          if (!available && current?.size === 1 && !current.has(policy(binding.definition))) continue;
          if (include(binding)) result.push({ ...binding, serviceName: node.serviceName,
            provider: { serviceNodeId: node.serviceNodeId, hostId: node.hostId, available }, guideMarkdown });
        }
      }
    }
    return result.sort((a, b) => a.definition.discovery!.mcp!.name!.localeCompare(b.definition.discovery!.mcp!.name!, 'en') || (a.provider?.serviceNodeId ?? '').localeCompare(b.provider?.serviceNodeId ?? '', 'en'));
  }
  list(params: ListInput, mcpOnly = false): Operation.DiscoveryListResult {
    requireThat((!params.group && !params.providers) || params.serviceName, 'invalid_arguments', 'Groups and provider lists require a serviceName.');
    requireThat(!params.providers || (!params.query && !params.group && !params.serviceNodeId), 'invalid_arguments', 'Provider selection cannot be combined with tool filters.');
    const nodes = this.registry.nodes().filter(node => !mcpOnly || this.index(node).tools.some(tool => tool.mcp));
    const tools = (node: ServiceNode) => this.index(node).tools.filter(tool => !mcpOnly || tool.mcp);
    for (const id of this.indexes.keys()) if (!nodes.some(node => node.serviceNodeId === id)) this.evict(id);
    if (params.serviceNodeId) {
      const node = this.registry.node(params.serviceNodeId);
      requireThat(!mcpOnly || this.index(node).tools.some(tool => tool.mcp), 'not_found', 'Service does not publish an MCP surface.');
      requireThat(!params.serviceName || node.serviceName === params.serviceName, 'target_conflict', 'The selected node belongs to a different service.');
    }
    const selected = nodes.filter(node => (!params.serviceName || node.serviceName === params.serviceName) && (!params.serviceNodeId || node.serviceNodeId === params.serviceNodeId));
    const serviceName = params.serviceName ?? (params.serviceNodeId ? selected[0]?.serviceName : undefined);
    requireThat(!serviceName || serviceName === 'hive' || selected.length, 'not_found', 'Service is not registered.');
    const identity = selected.map(node => ({ id: node.serviceNodeId, serviceName: node.serviceName,
      available: this.registry.eligible(node), catalog: this.index(node).identity }));
    let entries: Entry[] = [];
    const scores = new Map<string, number>();
    if (!serviceName && !params.query) {
      entries.push({ kind: 'service', name: 'hive', serviceName: 'hive', description: coreServiceDescription, toolCount: coreIndex.length, providerCount: 1, availableCount: 1 });
      for (const name of [...new Set(selected.map(node => node.serviceName))].sort()) {
        const owners = selected.filter(node => node.serviceName === name);
        entries.push({ kind: 'service', name, serviceName: name, description: this.index(owners[0]!).description,
          toolCount: new Set(owners.flatMap(node => tools(node).map(tool => tool.qualifiedName))).size,
          providerCount: owners.length, availableCount: owners.filter(node => this.registry.eligible(node)).length });
      }
    } else if (params.providers) {
      entries = selected.map(node => ({ kind: 'provider', name: node.serviceNodeId, serviceName: node.serviceName, serviceNodeId: node.serviceNodeId,
        hostId: node.hostId, available: this.registry.eligible(node), description: compactDescription(`${node.hostId}: ${node.serviceName} ${node.version}; native ${node.nativeVersion ?? 'none'}.`) }));
    } else {
      const records: Array<{ serviceName: string; tool: IndexedTool; available: boolean }> = [];
      if (!serviceName || serviceName === 'hive') for (const tool of coreIndex) records.push({ serviceName: 'hive', tool, available: true });
      for (const node of selected) for (const tool of tools(node)) records.push({ serviceName: node.serviceName, tool, available: this.registry.eligible(node) });
      const query = params.query ? [...new Set(searchWords(params.query))] : [];
      requireThat(!params.query || query.length, 'invalid_arguments', 'Search needs at least one meaningful term.');
      const filtered = records.filter(({ serviceName, tool }) => (!params.group || tool.group === params.group || tool.group.startsWith(params.group + '/')) &&
        query.every(term => [...tool.words, ...searchWords(serviceName)].some(word => word.includes(term))));
      const byName = new Map<string, Entry>();
      for (const { serviceName, tool, available } of filtered) {
        const key = serviceName + '\0' + tool.qualifiedName;
        const names = searchWords(tool.qualifiedName), summary = searchWords(tool.description);
        const score = query.reduce((sum, term) => sum + (names.includes(term) ? 20 : names.some(word => word.includes(term)) ? 10 : summary.includes(term) ? 5 : 1), 0);
        scores.set(key, Math.max(scores.get(key) ?? 0, score));
        const prior = byName.get(key);
        if (prior) { prior.providerCount!++; if (available) prior.availableCount!++; if (prior.effect !== tool.effect) prior.effect = 'unknown'; }
        else byName.set(key, { kind: 'tool', name: tool.qualifiedName, description: tool.description, serviceName, namespace: tool.namespace, group: tool.group,
          ...((tool.mcp?.name ?? (serviceName === 'hive' ? coreMcpNames[tool.qualifiedName.slice(5)] : undefined)) ? { mcpName: tool.mcp?.name ?? coreMcpNames[tool.qualifiedName.slice(5)]! } : {}),
          effect: tool.effect, providerCount: 1, availableCount: available ? 1 : 0 });
      }
      entries = [...byName.values()];
      if (!params.query && entries.length > 20) {
        const prefix = params.group ? params.group + '/' : '', grouped = new Map<string, Entry>();
        for (const entry of entries) {
          if (entry.group === params.group) { grouped.set(entry.name, entry); continue; }
          const group = prefix + entry.group!.slice(prefix.length).split('/')[0];
          const prior = grouped.get(group);
          if (prior) prior.toolCount!++;
          else grouped.set(group, { kind: 'group', name: group, group, serviceName: entry.serviceName,
            description: compactDescription((entry.serviceName === 'hive' ? coreGroups.get(group) : selected.map(node => this.index(node).groups.get(group)).find(Boolean)) ?? defaultGroupDescription(group)), toolCount: 1 });
        }
        entries = [...grouped.values()];
      }
    }
    entries.sort((a, b) => (scores.get(b.serviceName + '\0' + b.name) ?? 0) - (scores.get(a.serviceName + '\0' + a.name) ?? 0) ||
      Buffer.compare(Buffer.from(a.serviceName + '\0' + a.name), Buffer.from(b.serviceName + '\0' + b.name)));
    const { cursor, limit: _limit, ...selectors } = params;
    const cursorIdentity = { method: 'discovery.list', mcpOnly, selectors, catalogs: identity, entries: hashJson(entries) };
    const offset = cursor ? this.registry.store.readCursor(cursorIdentity, cursor) : 0;
    requireThat(typeof offset === 'number' && Number.isSafeInteger(offset) && offset >= 0 && offset <= entries.length, 'invalid_cursor', 'Invalid discovery boundary.');
    const items: Entry[] = [];
    let nextCursor: string | null = null;
    for (let index = offset; index < entries.length && items.length < (params.limit ?? 20); index++) {
      const next = index + 1 < entries.length ? this.registry.store.cursor(cursorIdentity, index + 1) : null;
      if (Buffer.byteLength(JSON.stringify({ items: [...items, entries[index]!], nextCursor: next })) > 8192) break;
      items.push(entries[index]!); nextCursor = next;
    }
    requireThat(items.length || offset === entries.length, 'result_too_large', 'A discovery entry exceeds the page limit.');
    return { items, nextCursor };
  }
  describe(params: Operation.DiscoveryDescribeParams, mcpOnly = false): Operation.DiscoveryDescribeResult {
    if (params.serviceName === 'hive') {
      requireThat(!params.serviceNodeId, 'target_conflict', 'Hive core operations do not have a service node.');
      return this.details({ serviceName: 'hive', provider: null, guides: [{ namespace: 'hive', guideMarkdown: coreGuide }], items: params.tools.map(coreBinding) });
    }
    let owner = params.serviceNodeId;
    const guides = new Map<string, string>();
    const namespaces = new Map<string, ReturnType<Registry['namespace']>>();
    const items = params.tools.map(qualifiedName => {
      const namespace = qualifiedName.split('.')[0]!;
      let ns = namespaces.get(namespace);
      if (!ns) {
        const node = this.registry.select(namespace, { serviceName: params.serviceName, ...(owner ? { serviceNodeId: owner } : {}) });
        owner = node.serviceNodeId;
        ns = this.registry.namespace(owner, namespace);
        namespaces.set(namespace, ns);
      }
      const definition = ns.tools.find(tool => namespace + '.' + tool.name === qualifiedName);
      requireThat(definition, 'not_found', 'Exact tool is absent from the selected provider catalog.');
      requireThat(!mcpOnly || definition.discovery?.mcp, 'not_found', 'Tool is not published through MCP.');
      guides.set(namespace, ns.guideMarkdown);
      return { qualifiedName, definition, definitionHash: toolDefinitionHash(definition) };
    });
    const node = this.registry.node(owner!);
    return this.details({ serviceName: params.serviceName, provider: { serviceNodeId: node.serviceNodeId, hostId: node.hostId, available: this.registry.eligible(node) },
      guides: [...guides].map(([namespace, guideMarkdown]) => ({ namespace, guideMarkdown })), items });
  }
  private details(result: Operation.DiscoveryDescribeResult): Operation.DiscoveryDescribeResult {
    requireThat(Buffer.byteLength(canonical(result)) <= 512 * 1024, 'result_too_large', 'Selected definitions exceed the detail budget. Request fewer tools; schemas are never truncated.');
    return result;
  }
  resolveCall(params: Operation.DiscoveryCallParams): { method: string; params: Record<string, unknown> } {
    if (params.serviceName === 'hive') return resolveCoreCall(params);
    requireThat(params.serviceNodeId, 'invalid_arguments', 'Service calls require the exact described serviceNodeId.');
    return { method: 'tools.call', params: { ...params } };
  }
}
