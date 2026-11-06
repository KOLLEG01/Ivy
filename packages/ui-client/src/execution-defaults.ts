import type { Agent, RpcClient } from '../../sdk/src/client.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { nativeBackendKey, sameBackend } from './native-backends.js';

export const executionDefaultsPath = '/ivy-agent-execution-defaults';
export const accountExecutionDefaultsPath = '/ivy-agent-account-execution-defaults';
export const emptyExecutionDefaults = (): Agent.ExecutionDefaults => ({ model: null, mode: null, effort: null, permission: null });
export const emptyAccountExecutionDefaults = (): Agent.AccountExecutionDefaults => ({ ...emptyExecutionDefaults(), serviceTier: null });

async function readDocument(client: RpcClient, path: string, contractKey: string, signal?: AbortSignal) {
  const object = await client.request('objects.stat', { path }, signal ? { signal } : undefined).catch(cause => {
    if ((cause as { code?: string }).code === 'not_found') return null;
    throw cause;
  });
  if (!object || object.effectivelyArchived) return null;
  const value = await client.request('objects.read', { objectId: object.id, revision: object.currentRevision }, signal ? { signal } : undefined);
  if (value.object.contractKey !== contractKey || value.revision.contractVersion !== '1.0.0' || value.content.encoding !== 'json')
    throw new Error('Agent execution defaults have a different document contract.');
  return { object, value: value.content.value };
}

/** The original provider-wide document is only for retained unscoped work. */
export async function readExecutionDefaults(client: RpcClient, signal?: AbortSignal) {
  const saved = await readDocument(client, executionDefaultsPath, 'agent/execution-defaults', signal);
  if (!saved) return null;
  validateAgent('ExecutionDefaultsDocument', saved.value);
  return { object: saved.object, document: saved.value as unknown as Agent.ExecutionDefaultsDocument };
}

function validateAccounts(document: Agent.AccountExecutionDefaultsDocument): void {
  validateAgent('AccountExecutionDefaultsDocument', document);
  const identities = new Set<string>();
  for (const { backend } of document.accounts) {
    const key = nativeBackendKey(backend);
    if (identities.has(key)) throw new Error('Agent execution defaults contain a duplicate account identity.');
    identities.add(key);
  }
}
export async function readAccountExecutionDefaults(client: RpcClient, signal?: AbortSignal) {
  const saved = await readDocument(client, accountExecutionDefaultsPath, 'agent/account-execution-defaults', signal);
  if (!saved) return null;
  const document = saved.value as unknown as Agent.AccountExecutionDefaultsDocument;
  validateAccounts(document);
  return { object: saved.object, document };
}
export function accountExecutionProfile(document: Agent.AccountExecutionDefaultsDocument | null | undefined,
  backend: Agent.BackendIdentity | undefined): Agent.AccountExecutionDefaults | null {
  if (!document || !backend) return null;
  validateAccounts(document);
  return document.accounts.find(value => sameBackend(value.backend, backend))?.defaults ?? null;
}
/** Edit one exact account while retaining every other profile. The caller writes with object CAS. */
export function withAccountExecutionProfile(document: Agent.AccountExecutionDefaultsDocument | null | undefined,
  backend: Agent.BackendIdentity, defaults: Agent.AccountExecutionDefaults): Agent.AccountExecutionDefaultsDocument {
  const source = document ?? { schemaVersion: 1, accounts: [] };
  validateAccounts(source);
  const accounts = source.accounts.filter(value => !sameBackend(value.backend, backend))
    .map(value => ({ backend: { ...value.backend }, defaults: { ...value.defaults } }));
  if (Object.values(defaults).some(value => value !== null)) accounts.push({ backend: { ...backend }, defaults: { ...defaults } });
  accounts.sort((a, b) => nativeBackendKey(a.backend).localeCompare(nativeBackendKey(b.backend)));
  const result: Agent.AccountExecutionDefaultsDocument = { schemaVersion: 1, accounts };
  validateAccounts(result);
  return result;
}
