/** Navigation metadata only: actual schemas, names and permissions remain in their catalogs. */
export const coreServiceDescription = 'Hive stores Wiki pages, structured objects and files, and connects hosts, service resources and events.';

export const coreGroups = new Map(Object.entries({
  wiki: 'Search, browse, read and edit Wiki pages using page-specific tools.',
  objects: 'Generic object storage fallback; prefer dedicated Wiki or service tools when available.',
  contracts: 'Discover and validate object schemas.',
  hosts: 'Connected hosts and their configuration.',
  hostConfigurations: 'Stored host settings, service configuration and revision history.',
  serviceNodes: 'Service instances, status and required schemas.',
  inventory: 'Resources advertised by services and their owning hosts.',
  events: 'Read retained events.',
  topics: 'Discover service events available as subscription triggers.',
  system: 'Hive status, caller identity and diagnostics.',
  retention: 'Storage retention policies and eligible data.',
  uis: 'Browse, inspect and publish applications.',
  packages: 'Available service and application packages.',
  deployments: 'Service deployment status and original operations.',
}));

export const coreGuide = 'Use wiki_search and wiki_read for Wiki pages and the publishing service’s direct tools for its functions. Generic Hive object operations cover data without a dedicated tool. '
  + 'Use hive.objects.list or tree to browse a hierarchy. For structured service data, discover its contract with hive.contracts.list/get and filter fields with hive.objects.query. '
  + 'Find resource owners with hive.inventory.list or hive.serviceNodes.list; discover subscribable events with hive.topics.list. '
  + 'Read before editing, use the current expectedRevision and retain mutationId for the same change. Reconcile an unknown outcome before retrying.';

export const coreDescriptions: Record<string, string> = {
  'wiki.search': 'Search Wiki page titles and Markdown. Optionally search below a page or include archived pages.',
  'wiki.list': 'List Wiki pages directly below a parent; omit parentId to browse root pages.',
  'wiki.read': 'Read a Wiki page and its revision, optionally at a historical revision.',
  'wiki.create': 'Create a Wiki page with a title and Markdown under an optional parent. Retain mutationId for retries.',
  'wiki.update': 'Replace a Wiki page’s Markdown using its current expectedRevision. Read first and retain mutationId for retries.',
  'wiki.history': 'List the saved revisions of a Wiki page.',
  'wiki.move': 'Move or rename a Wiki page using its exact objectId, parentId, name and a stable mutationId.',
  'wiki.archive': 'Archive or restore a Wiki page using archived=true or false and a stable mutationId.',
  'system.status': 'Read Hive readiness, version and current caller identity.',
  'system.instructions': 'Read the complete current MCP instructions or tested workflow examples, with a hash and continuation when needed.',
  'system.toolSchema': 'Read the exact public MCP input and output schemas for one named tool.',
  'system.inspectStorage': 'Inspect stored contract versions and storage compatibility.',
  'system.diagnostics': 'List current or historical service diagnostics.',
  'contracts.list': 'List object schemas (data contracts) and their versions.',
  'contracts.summaries': 'List compact data contract summaries; use hive_schema_get for the complete schema and guidance.',
  'contracts.get': 'Read a data contract, its schema and usage guidance.',
  'contracts.register': 'Register an agent-owned data contract using a stable mutationId.',
  'contracts.validate': 'Validate content against an exact contract version.',
  'objects.stat': 'Read metadata for a Wiki page, structured object or file by ID or path.',
  'objects.read': 'Read a Wiki page, structured object or file, optionally at a saved revision.',
  'objects.write': 'Create or update a Wiki page, structured object or file. Updates require expectedRevision; retain mutationId for retries.',
  'objects.history': 'List saved revisions of a Wiki page, structured object or file.',
  'objects.list': 'List Wiki pages, structured objects and files under a parent.',
  'objects.tree': 'Browse a hierarchy of Wiki pages, structured objects and files.',
  'objects.query': 'Find structured objects by contract and field filters, such as tasks or assignments.',
  'objects.search': 'Find Wiki pages, documents and files by text. Use objects.query for structured field filters.',
  'objects.move': 'Move or rename a stored document or file.',
  'objects.reorder': 'Reorder a stored document or file within its current parent.',
  'objects.archive': 'Archive or restore an object using archived=true or false and a stable mutationId.',
  'objects.delete': 'Permanently delete a Wiki page or TaskBoard task and its stored revisions.',
  'retention.preview': 'Preview current retention eligibility without deleting data.',
  'retention.status': 'Read effective family policies, eligible volume and bounded journal usage.',
  'hosts.list': 'List connected hosts and their service instances.',
  'hosts.observations': 'Read reported host configuration and runtime observations.',
  'hostConfigurations.list': 'List stored host configuration summaries and revisions.',
  'hostConfigurations.get': 'Read a complete stored host configuration. Use hostConfigurations.edit for a view with secrets redacted.',
  'hostConfigurations.put': 'Store a complete host configuration at its current revision. Use hostConfigurations.save to preserve redacted secrets.',
  'hostConfigurations.edit': 'Read a host configuration for editing, with secrets redacted. This call does not change it.',
  'hostConfigurations.save': 'Save an edited host configuration at its current revision, preserving unchanged redacted secrets.',
  'hostConfigurations.history': 'List saved revisions of a host configuration.',
  'serviceNodes.list': 'Find service instances by host or service name.',
  'serviceNodes.get': 'Read status and identity of one exact service instance.',
  'serviceNodes.contracts': 'Read data contracts required by a service instance.',
  'inventory.list': 'Find resources advertised by services and their original owners.',
  'inventory.get': 'Read one advertised resource and its observation freshness.',
  'events.read': 'Read a bounded event batch after a sequence number.',
  'topics.list': 'Find registered event triggers, including Hive object changes, with payload schemas and event kinds.',
  'packages.catalog': 'List available service and application packages, optionally after a catalog revision.',
  'packages.authorizeUpload': 'Create a five-minute, single-use upload token for one validated package identity.',
  'uis.list': 'Read complete published application records; prefer uis.catalog for a compact overview.',
  'uis.get': 'Read one published application and its releases.',
  'uis.catalog': 'List installed application metadata and selected releases.',
  'uis.releases': 'List compact release summaries for an application.',
  'uis.inspect': 'Check an application release and its dependencies.',
  'uis.deploy': 'Publish and select an application release with a stable mutationId.',
  'uis.rollback': 'Select a previous application release with a stable mutationId.',
  'deployments.list': 'List reported service deployment operations.',
  'deployments.get': 'Read the original deployment operation on its host.',
};

export const defaultGroupDescription = (group: string): string => `${group.replaceAll('/', ' / ')} operations and tools.`;
export function compactDescription(text: string, maxBytes = 220): string {
  const clean = text.replace(/\s+/g, ' ').trim();
  let result = '';
  for (const char of clean) { if (Buffer.byteLength(result + char) > maxBytes - 3) return result + '...'; result += char; }
  return result || 'Service capabilities.';
}
const synonyms: Record<string, string> = {
  thread: 'task', threads: 'task', tasks: 'task', aufgabe: 'task', aufgaben: 'task',
  projects: 'project', projekt: 'project', projekte: 'project',
  usage: 'usage', verbrauch: 'usage', kontingent: 'usage', limits: 'limit',
  files: 'file', datei: 'file', dateien: 'file', documents: 'document', dokumente: 'document',
  nachricht: 'message', nachrichten: 'message', messages: 'message',
  sprachchat: 'voice', sprache: 'voice', anruf: 'call', anrufe: 'call',
  events: 'event', ereignis: 'event', ereignisse: 'event', triggers: 'trigger',
  schedules: 'schedule', scheduled: 'schedule', scheduling: 'schedule', zeitplan: 'schedule', zeitpläne: 'schedule',
  reminders: 'reminder', erinnerung: 'reminder', erinnerungen: 'reminder',
  ungelesen: 'unread', ungelesene: 'unread', ungelesenen: 'unread',
  notifications: 'notification', benachrichtigung: 'notification', benachrichtigungen: 'notification',
  suche: 'search', suchen: 'search', finden: 'find', lesen: 'read', erstellen: 'create',
};
export function searchWords(text: string): string[] {
  return text.replace(/([a-z])([A-Z])/g, '$1 $2').toLowerCase().split(/[^\p{L}\p{N}]+/u)
    .filter(word => word && !/^(a|an|the|for|of|to|in|and|or|with|der|die|das|den|dem|ein|eine|einen|und|oder|mit|im|auf|für|ich|möchte|bitte)$/.test(word))
    .map(word => /^archiv/.test(word) ? 'archive' : Object.hasOwn(synonyms, word) ? synonyms[word]! : word);
}
