import definition from '../ui.json';
import type { Operation, TaskBoard, Wire } from '../../../packages/sdk/src/client.js';
import { uiRuntime, route } from '../../../packages/ui-client/src/runtime';
import { upgradeTaskRecord, upgradeResultRecord } from '../../../packages/sdk/src/client.js';
import { CommentReader } from '../../../packages/sdk/src/task-comments.js';
import type { CommentTree } from '../../../packages/sdk/src/task-comments.js';
export const { base, client, uiUrl, notifications, live } = uiRuntime(definition.metadata);
const conversationReader = new CommentReader(client);
export type { TaskBoard, Operation, Wire };
export const tr = (de: string, en: string) => navigator.language.toLowerCase().startsWith('de') ? de : en;
export const taskRoute = (node: string, id: string, values: Record<string, string> = {}) => route('tasks', { id, node, ...values });
export const taskUrl = (node: string, id: string) => uiUrl + taskRoute(node, id);
export const wikiPageUrl = (id: string) => new URL('wiki/#/page?id=' + encodeURIComponent(id), base).href;
export const rootWhere = (root: string | null): Wire.QueryPredicate => root === null ? { op: 'isNull', field: 'object.parentId' } : { op: 'eq', field: 'object.parentId', value: root };
export const and = (...args: Wire.QueryPredicate[]): Wire.QueryPredicate => args.length === 1 ? args[0]! : { op: 'and', args };
export const taskStatuses: TaskBoard.Task['workflowState'][] = ['backlog', 'todo', 'in_progress', 'waiting', 'review', 'done', 'cancelled'];
export const taskContractVersions = ['1.0.0', '1.1.0', '1.2.0', '1.3.0', '1.4.0', '1.5.0', '1.6.0', '1.7.0', '1.8.0', '1.9.0', '1.10.0', '1.11.0', '1.12.0', '2.0.0', '2.1.0', '2.2.0', '2.3.0', '3.0.0'];
export const workflowContractVersions = ['1.10.0', '1.11.0', '1.12.0', '2.0.0', '2.1.0', '2.2.0'];
export const statusTone = (value: string) => ['completed', 'succeeded', 'passed', 'confirmed'].includes(value) ? 'good' as const : ['waiting', 'waiting_input', 'review', 'outcome_unknown', 'unknown', 'local_only', 'cancel_requested', 'blocked', 'pending'].includes(value) ? 'warning' as const : ['failed', 'cancelled'].includes(value) ? 'bad' as const : 'neutral' as const;
export interface Documents { delivery: TaskBoard.Delivery; task: TaskBoard.Task; run: TaskBoard.Run; result: TaskBoard.Result; review: TaskBoard.Review; history: TaskBoard.History; 'native-plan': TaskBoard.NativePlan; 'native-signals': TaskBoard.NativeSignals; 'native-transcript': TaskBoard.NativeTranscript; 'native-result-page': TaskBoard.NativeResultPage; 'native-read-evidence': TaskBoard.NativeReadEvidence; 'native-full-turn-transcript': TaskBoard.NativeFullTurnTranscript; 'native-turn-snapshot': TaskBoard.NativeTurnSnapshot }
export interface Document<K extends keyof Documents> { value: Documents[K]; pin: TaskBoard.ObjectPin; read: Operation.ObjectRead; commentRange?: { start: number; end: number; total: number } }
export const documentContractVersions = (key: keyof Documents): string[] => key === 'task' ? taskContractVersions : key === 'run' ? ['1.0.0', '1.1.0', '1.2.0', '1.3.0', '2.0.0', '2.1.0', '2.2.0', '3.0.0'] : key === 'result' ? ['1.0.0', '2.0.0', '2.1.0', '2.2.0'] : key === 'delivery' ? ['1.0.0', '1.1.0'] : ['1.0.0'];
export async function readDocument<K extends keyof Documents>(
  key: K,
  pin: string | TaskBoard.ObjectPin,
  root?: string | null,
  signal?: AbortSignal,
  commentRange?: { before?: number; limit: number },
): Promise<Document<K>> {
  const read = await client.request('objects.read', typeof pin === 'string' ? { objectId: pin } : pin, signal ? { signal } : {});
  const decoded = decodeDocument(key, read, root);
  if (key === 'task') {
    const header = decoded.value as TaskBoard.Task & { commentStore?: { objectId: string; length: number;
      nativeInputs: Pick<TaskBoard.Comment, 'commentId' | 'nativeInput' | 'handoff' | 'delivery'>[] } };
    if (header.commentStore) {
      if ('kind' in header.commentStore) {
        const { commentStore, ...task } = header as unknown as TaskBoard.Task & { commentStore: CommentTree };
        const end = Math.min(commentRange?.before ?? commentStore.length, commentStore.length);
        const start = Math.max(0, end - (commentRange?.limit ?? commentStore.length));
        const entries = await conversationReader.range(commentStore, read.object.id, start, end, signal);
        decoded.value = { ...task, comments: entries.map(entry => entry.comment),
          commentDeliveries: entries.flatMap(entry => entry.receipt ? [entry.receipt] : []) } as Documents[K];
        decoded.commentRange = { start, end, total: commentStore.length };
        return decoded;
      }
      const { commentStore: reference, ...task } = header;
      const saved = await client.request('objects.read', { objectId: reference.objectId }, signal ? { signal } : {});
      if (saved.object.contractKey !== 'task-board/comments' || !['1.0.0', '1.1.0', '2.0.0', '2.1.0', '2.2.0'].includes(saved.revision.contractVersion) ||
          saved.content.encoding !== 'json' || saved.object.parentId !== read.object.id || saved.object.ownerObjectId !== null)
        throw new Error('The ticket conversation belongs to another record.');
      const log = saved.content.value as unknown as { taskId: string; comments: TaskBoard.Comment[] };
      if (log.taskId !== read.object.id || !Array.isArray(log.comments) || !Number.isSafeInteger(reference.length) ||
          reference.length < 0 || reference.length > log.comments.length) throw new Error('The ticket conversation prefix is unavailable.');
      const states = new Map(reference.nativeInputs.map(state => [state.commentId, state]));
      decoded.value = { ...task, comments: log.comments.slice(0, reference.length).map(comment => {
        const { nativeInput: _input, handoff: _handoff, delivery: _delivery, ...body } = comment;
        return { ...body, ...states.get(comment.commentId) };
      }) } as Documents[K];
    }
  }
  if (key === 'task') decoded.value = upgradeTaskRecord(decoded.value as TaskBoard.Task) as Documents[K];
  return decoded;
}
export function decodeDocument<K extends keyof Documents>(
  key: K,
  read: Operation.ObjectRead,
  root?: string | null,
): Document<K> {
  if (read.object.contractKey !== 'task-board/' + key || !documentContractVersions(key).includes(read.revision.contractVersion) || read.content.encoding !== 'json' || read.revision.mediaType !== 'application/json') throw new Error('This saved record needs a compatible UI release. Drafts remain in this tab.');
  if (root !== undefined && read.object.parentId !== root) throw new Error('This record belongs to a different workspace.');
  const value = key === 'task' && !(read.content.value as unknown as { commentStore?: unknown }).commentStore ? upgradeTaskRecord(read.content.value as TaskBoard.Task) : key === 'result' ? upgradeResultRecord(read.content.value as TaskBoard.Result) : read.content.value;
  return { value: value as Documents[K], pin: { objectId: read.object.id, revision: read.revision.revision }, read };
}
export const consoleObject = (pin: TaskBoard.ObjectPin) => new URL('#/objects?id=' + encodeURIComponent(pin.objectId) + '&revision=' + pin.revision, base).href;
