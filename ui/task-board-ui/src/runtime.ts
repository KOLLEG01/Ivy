import definition from '../ui.json';
import type { Operation, TaskBoard, Wire } from '../../../packages/sdk/src/client.js';
import { uiRuntime, route } from '../../../packages/ui-client/src/runtime';
import { upgradeTaskRecord } from '../../../packages/sdk/src/client.js';
export const { base, client, uiUrl, notifications, live } = uiRuntime(definition.metadata);
export type { TaskBoard, Operation, Wire };
export const tr = (de: string, en: string) => navigator.language.toLowerCase().startsWith('de') ? de : en;
export const taskRoute = (node: string, id: string, values: Record<string, string> = {}) => route('tasks', { id, node, ...values });
export const taskUrl = (node: string, id: string) => uiUrl + taskRoute(node, id);
export const wikiPageUrl = (id: string) => new URL('wiki/#/page?id=' + encodeURIComponent(id), base).href;
export const rootWhere = (root: string | null): Wire.QueryPredicate => root === null ? { op: 'isNull', field: 'object.parentId' } : { op: 'eq', field: 'object.parentId', value: root };
export const and = (...args: Wire.QueryPredicate[]): Wire.QueryPredicate => args.length === 1 ? args[0]! : { op: 'and', args };
export const taskStatuses: TaskBoard.Task['workflowState'][] = ['backlog', 'todo', 'in_progress', 'waiting', 'review', 'done', 'cancelled'];
export const taskContractVersions = ['1.0.0', '1.1.0', '1.2.0', '1.3.0', '1.4.0', '1.5.0', '1.6.0', '1.7.0'];
export const statusTone = (value: string) => ['completed', 'succeeded', 'passed', 'confirmed'].includes(value) ? 'good' as const : ['waiting', 'waiting_input', 'review', 'outcome_unknown', 'unknown', 'local_only', 'cancel_requested', 'blocked', 'pending'].includes(value) ? 'warning' as const : ['failed', 'cancelled'].includes(value) ? 'bad' as const : 'neutral' as const;
export interface Documents { delivery: TaskBoard.Delivery; task: TaskBoard.Task; run: TaskBoard.Run; result: TaskBoard.Result; review: TaskBoard.Review; history: TaskBoard.History; 'native-plan': TaskBoard.NativePlan; 'native-signals': TaskBoard.NativeSignals; 'native-transcript': TaskBoard.NativeTranscript; 'native-result-page': TaskBoard.NativeResultPage; 'native-read-evidence': TaskBoard.NativeReadEvidence; 'native-full-turn-transcript': TaskBoard.NativeFullTurnTranscript; 'native-turn-snapshot': TaskBoard.NativeTurnSnapshot }
export interface Document<K extends keyof Documents> { value: Documents[K]; pin: TaskBoard.ObjectPin; read: Operation.ObjectRead }
export const documentContractVersions = (key: keyof Documents): string[] => key === 'task' ? taskContractVersions : key === 'run' || key === 'delivery' ? ['1.0.0', '1.1.0'] : ['1.0.0'];
export async function readDocument<K extends keyof Documents>(
  key: K,
  pin: string | TaskBoard.ObjectPin,
  root?: string | null,
  signal?: AbortSignal,
): Promise<Document<K>> {
  const read = await client.request('objects.read', typeof pin === 'string' ? { objectId: pin } : pin, signal ? { signal } : {});
  return decodeDocument(key, read, root);
}
export function decodeDocument<K extends keyof Documents>(
  key: K,
  read: Operation.ObjectRead,
  root?: string | null,
): Document<K> {
  if (read.object.contractKey !== 'task-board/' + key || !documentContractVersions(key).includes(read.revision.contractVersion) || read.content.encoding !== 'json' || read.revision.mediaType !== 'application/json') throw new Error('This saved record needs a compatible UI release. Drafts remain in this tab.');
  if (root !== undefined && read.object.parentId !== root) throw new Error('This record belongs to a different workspace.');
  const value = key === 'task' ? upgradeTaskRecord(read.content.value as TaskBoard.Task) : read.content.value;
  return { value: value as Documents[K], pin: { objectId: read.object.id, revision: read.revision.revision }, read };
}
export const consoleObject = (pin: TaskBoard.ObjectPin) => new URL('#/objects?id=' + encodeURIComponent(pin.objectId) + '&revision=' + pin.revision, base).href;
