import { hashJson } from '../dist/packages/sdk/src/node.js';

/** Synthetic provider catalog for workflow tests; real native schemas are tested in the SDK. */
export function nativeOwnerCatalog(version, nativeExecutableHash) {
  const inputSchema = { type: 'object', properties: { threadId: { type: 'string' }, includeTurns: { type: 'boolean' } }, required: ['threadId'], additionalProperties: false };
  const outputSchema = { type: 'object', properties: { thread: { type: 'object', properties: {
    id: { type: 'string' }, ephemeral: { type: 'boolean' }, canAcceptDirectInput: { type: 'boolean' },
    status: { type: 'object', properties: { type: { enum: ['idle', 'systemError', 'active', 'notLoaded'] } }, required: ['type'], additionalProperties: false },
  }, required: ['id', 'ephemeral', 'status'], additionalProperties: false } }, required: ['thread'], additionalProperties: false };
  const properties = { threadId: { type: 'string' }, turnId: { type: 'string' }, cursor: { type: ['string', 'null'] }, limit: { type: 'integer' }, sortDirection: { enum: ['asc', 'desc'] } };
  const clientRequests = [{ method: 'thread/read', paramsRequired: true, inputSchema, outputSchema, responseType: 'SyntheticThreadReadResponse', inputSource: 'native-json', outputSource: 'native-json' },
    { method: 'thread/items/list', paramsRequired: true, inputSchema: { type: 'object', properties, required: Object.keys(properties), additionalProperties: false },
      outputSchema: { type: 'object', properties: { data: { type: 'array', items: { type: 'object', properties: { turnId: { type: 'string' }, item: { type: 'object', additionalProperties: true } }, required: ['turnId', 'item'] } }, nextCursor: { type: ['string', 'null'] } }, required: ['data', 'nextCursor'] },
      responseType: 'SyntheticItemPageResponse', inputSource: 'native-json', outputSource: 'native-json' }];
  return { schemaVersion: 1, provider: 'codex', version, experimental: true, nativeExecutableHash, sourceHash: hashJson(['synthetic-workflow-schema', clientRequests]),
    clientRequests,
    serverRequests: [], serverNotifications: [], clientNotifications: [], derivedLegacyTypes: [] };
}

/** Strict synthetic workflow input shapes. These are not claims about a shipped native binary. */
export function nativeTriageCatalog(version, nativeExecutableHash) {
  const catalog = nativeOwnerCatalog(version, nativeExecutableHash), string = { type: 'string' }, object = { type: 'object', additionalProperties: true }, array = { type: 'array' };
  const common = { model: string, permissions: string, approvalPolicy: string }, restrictions = { config: object, developerInstructions: string };
  const inputs = {
    'thread/start': { ...common, ...restrictions, projectId: string, cwd: string, ephemeral: { type: 'boolean' }, allowProviderModelFallback: { type: 'boolean' },
      environments: array, selectedCapabilityRoots: array, dynamicTools: array, baseInstructions: string },
    'thread/resume': { ...common, ...restrictions, threadId: string, excludeTurns: { type: 'boolean' } },
    'turn/start': { ...common, threadId: string, effort: string, environments: array, outputSchema: object,
      input: { type: 'array', minItems: 1, items: { oneOf: [
        { type: 'object', properties: { type: { const: 'text' }, text: string, text_elements: array }, required: ['type', 'text', 'text_elements'], additionalProperties: false },
        { type: 'object', properties: { type: { enum: ['image', 'audio'] }, url: string }, required: ['type', 'url'], additionalProperties: false },
      ] } } },
    'thread/unsubscribe': { threadId: string },
  };
  for (const [method, properties] of Object.entries(inputs)) catalog.clientRequests.push({ method, paramsRequired: true,
    inputSchema: { type: 'object', properties, required: Object.keys(properties).filter(key => key !== 'projectId'), additionalProperties: false }, outputSchema: object,
    responseType: 'SyntheticTriageResponse', inputSource: 'native-json', outputSource: 'native-json' });
  catalog.sourceHash = hashJson(['synthetic-triage-workflow', catalog.clientRequests]); return catalog;
}
