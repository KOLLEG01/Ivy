import { computed, onScopeDispose, ref, watch } from 'vue';
import type { Ref } from 'vue';
import { discover, IvyError, newOperationId } from '../../../packages/sdk/src/client.js';
import type { Operation, TaskBoard, Wire } from './runtime';
import { base, client } from './runtime';
import { nativeRead } from '../../../packages/ui-client/src/native';
import { hashJson } from '../../../packages/ui-client/src/hash';
import { hashBytes } from '../../../packages/ui-client/src/content';
import { attachmentBase64 } from '../../../packages/ui-client/src/message-attachments';
import { archiveActionOutcome, readArchiveOutcome } from '../../../packages/ui-client/src/object-archive';
import { maximumUploadBytes, recoverUpload, releaseUpload, retainUpload, saveUpload, uploadFile } from './upload-store';
type WithoutId<T> = T extends unknown ? Omit<T, 'operationId'> : never;
export type UploadDraft = Omit<TaskBoard.UploadAttachmentRequest, 'operationId' | 'bytesBase64'> & { file: File };
export type DraftRequest = WithoutId<Exclude<TaskBoard.ActionInput, { action: 'uploadAttachment' }> | TaskBoard.ArchiveRequest> | UploadDraft;
export type EditSnapshot = { pin: TaskBoard.ObjectPin; fields: TaskBoard.TaskFields; wakeAt: TaskBoard.Task['wakeAt']; dependencies: string[] };
export interface SavedAction {
  label: string; callerPrincipalId: string; principalId: string; rootObjectId: string | null; call: Wire.ToolCall;
  phase: 'prepared' | 'unknown' | 'pending' | 'failed' | 'succeeded'; detail: string; outcome: TaskBoard.ActionOutcome | null;
  upload?: { byteLength: number; contentHash: string };
  requestHash?: string;
  archiveBase?: Operation.ObjectObservation;
}
export const actionStorageKey = (workspace: TaskBoard.WorkspaceInfo, scope: string) =>
  'ivy:task-board:action:' + base.href + workspace.serviceNodeId + ':' + scope;

/** Check the existing per-ticket originals without taking their ownership. */
export function assertTaskActionAvailable(workspace: TaskBoard.WorkspaceInfo, taskId: string, scope: string) {
  for (const other of ['task:' + taskId, 'edit:' + taskId, 'board:task:' + encodeURIComponent(taskId)]) {
    if (other === scope) continue;
    const raw = sessionStorage.getItem(actionStorageKey(workspace, other));
    if (!raw) continue;
    const original = JSON.parse(raw) as SavedAction;
    if (!original.call?.operationId || (original.call.arguments as { taskId?: string })?.taskId !== taskId)
      throw new Error('The retained change belongs to another task. Its original request has been preserved.');
    if (!['succeeded', 'failed'].includes(original.phase))
      throw new Error('A change in this task is still being confirmed. Its original request has been preserved.');
  }
}
export function useAction(workspace: () => TaskBoard.WorkspaceInfo, scope: string) {
  const key = actionStorageKey(workspace(), scope);
  const taskId = scope.startsWith('board:task:') ? decodeURIComponent(scope.slice('board:task:'.length)) :
    scope.startsWith('task:') ? scope.slice('task:'.length) : scope.startsWith('edit:') ? scope.slice('edit:'.length) : null;
  const saved = ref(null) as Ref<SavedAction | null>, busy = ref(false), error = ref<string | null>(null), storageError = ref(false), needsOriginalFile = ref(false);
  let disposed = false;
  try { const raw = sessionStorage.getItem(key); if (raw) { const value = JSON.parse(raw) as SavedAction;
    if (!value.call?.operationId || !value.callerPrincipalId || typeof value.principalId !== 'string') throw new Error('Invalid retained action'); saved.value = value; } }
  catch { storageError.value = true; error.value = 'The original action cannot be read from this tab. Restore browser storage before sending.'; }
  const identityMatches = () => !saved.value || saved.value.callerPrincipalId === workspace().callerPrincipalId && saved.value.principalId === workspace().principalId && saved.value.rootObjectId === workspace().rootObjectId && saved.value.call.serviceNodeId === workspace().serviceNodeId;
  const persist = async () => {
    try {
      if (saved.value?.upload) await saveUpload(saved.value);
      if (saved.value) sessionStorage.setItem(key, JSON.stringify(saved.value)); else sessionStorage.removeItem(key);
      storageError.value = false;
    } catch { storageError.value = true; throw new Error('The original action could not be retained. Further sends are paused.'); }
    if (saved.value?.upload && ['succeeded', 'failed'].includes(saved.value.phase)) {
      // The terminal receipt/metadata is durable before the original bytes are released.
      try { await releaseUpload(saved.value); } catch { /* A later admission may release this known terminal record. */ }
    }
  };
  const originalCall = async () => {
    const action = saved.value;
    if (!action) throw new Error('The original action is unavailable.');
    if (!action.upload) return action.call;
    const file = await uploadFile(action);
    if (!file) { needsOriginalFile.value = true; throw new Error('Choose the original file to resume this upload. Its identity and outcome are retained.'); }
    if (file.size !== action.upload.byteLength || await hashBytes(new Uint8Array(await file.arrayBuffer())) !== action.upload.contentHash)
      throw new Error('The retained file differs from the original upload.');
    const bytesBase64 = await attachmentBase64(file, () => { if (saved.value !== action || !identityMatches()) throw new Error('The original upload owner changed.'); });
    const call = { ...action.call, arguments: { ...action.call.arguments as Record<string, Wire.Json>, bytesBase64 } };
    const hash = await hashJson(call.arguments);
    if (action.requestHash && hash !== action.requestHash) throw new Error('The upload differs from the original request.');
    action.requestHash = hash;
    return call;
  };
  const checkCurrentIdentity = async () => {
    const current = await nativeRead(client, workspace().serviceNodeId, 'task-board.workspace', {}) as TaskBoard.WorkspaceInfo;
    if (!saved.value || current.callerPrincipalId !== saved.value.callerPrincipalId || current.principalId !== saved.value.principalId || current.rootObjectId !== saved.value.rootObjectId || current.serviceNodeId !== saved.value.call.serviceNodeId)
      throw new Error('The current caller or workflow owner changed. The original action is retained without being sent.');
  };
  const readOperation = async () => {
    if (!saved.value) throw new Error('The original action is unavailable.');
    const operation = await nativeRead(client, workspace().serviceNodeId, 'task-board.operation', { operationId: saved.value.call.operationId! }) as TaskBoard.OperationReceipt;
    const matches = (operation.submittedHash ?? operation.requestHash) === (saved.value.requestHash ?? await hashJson((await originalCall()).arguments));
    if (operation.callerPrincipalId !== saved.value.callerPrincipalId || operation.operationId !== saved.value.call.operationId || !matches) throw new Error('The operation differs from the retained caller or original request.');
    return operation;
  };
  const execute = async () => {
    if (disposed || !saved.value || busy.value || !identityMatches()) return;
    const replay = saved.value.phase !== 'prepared';
    busy.value = true; error.value = null;
    try {
      const call = await originalCall();
      if (disposed) return;
      if (taskId && saved.value.phase === 'prepared') {
        try { assertTaskActionAvailable(workspace(), taskId, scope); }
        catch (cause) { error.value = cause instanceof Error ? cause.message : 'The original task change is unavailable.'; return; }
      }
      // expectedWorkspace binds caller and owner in the mutation itself. A separate
      // read here could time out before dispatch and leave a phantom unknown action.
      saved.value.phase = 'unknown'; saved.value.detail = 'Waiting for the original workflow operation.'; await persist();
      if (disposed) return;
      let value = await client.request('tools.call', call) as TaskBoard.ActionOutcome;
      if (disposed) return;
      // The exact successful archive RPC already returns confirmed Object metadata.
      // Lost replies still reconcile the same original receipt below.
      if ((saved.value.call.arguments as { action: string }).action === 'archive') {
        const request = saved.value.call.arguments as TaskBoard.ArchiveRequest;
        value = archiveActionOutcome(value as unknown as Operation.ObjectMetadata, {
          objectId: request.taskId, archived: request.archived, mutationId: call.operationId!, call });
      }
      if (value.operationId !== saved.value.call.operationId) throw new Error('The response does not identify the original action.');
      saved.value.outcome = value; saved.value.phase = 'succeeded'; saved.value.detail = 'The workflow action was saved.'; needsOriginalFile.value = false; await persist();
    } catch (cause) {
      if (disposed) return;
      const failure = IvyError.from(cause);
      // A rejected retry only describes that attempt; an earlier lost result can
      // still be unresolved. Only owner reconciliation can settle that identity.
      saved.value.phase = replay || failure.outcome === 'unknown' || !(cause instanceof IvyError) ? 'unknown' : 'failed';
      saved.value.detail = `${failure.message} (${failure.code}; ${failure.outcome})`;
      try { await persist(); } catch (problem) { error.value = String(problem); }
    } finally { busy.value = false; }
  };
  const start = async (label: string, draft: DraftRequest | (() => Promise<DraftRequest | null>), archiveBase?: () => Operation.ObjectObservation | undefined) => {
    if (disposed || busy.value || error.value || !identityMatches() || saved.value && !['succeeded', 'failed'].includes(saved.value.phase)) return null;
    const selected = workspace(), current = { serviceNodeId: selected.serviceNodeId, callerPrincipalId: selected.callerPrincipalId,
      principalId: selected.principalId, rootObjectId: selected.rootObjectId };
    const sameWorkspace = () => { const value = workspace(); return Object.keys(current).every(key => current[key as keyof typeof current] === value[key as keyof typeof current]); };
    busy.value = true;
    try {
      if (taskId) assertTaskActionAvailable(selected, taskId, scope);
      const request = typeof draft === 'function' ? await draft() : draft;
      // A current read can show a conflict or an unchanged draft before any write.
      if (!request) { busy.value = false; return null; }
      if (disposed) throw new Error('The view was closed before the change was sent.');
      if (!sameWorkspace()) throw new Error('The caller or workspace changed before the change was sent.');
      const file = request.action === 'uploadAttachment' && 'file' in request ? request.file : null;
      if (file && file.size > maximumUploadBytes) throw new Error(file.name + ' is larger than 8 MiB.');
      if (file) {
        const recovered = await recoverUpload(key, workspace());
        if (recovered && !['succeeded', 'failed'].includes(recovered.phase)) {
          saved.value = recovered; await persist(); throw new Error('Check the original pending upload before adding another file.');
        }
      }
      const binding = await discover(client, 'task-board.' + request.action, { serviceNodeId: current.serviceNodeId }), operationId = await newOperationId(client);
      if (disposed) throw new Error('The view was closed before the change was sent.');
      if (!sameWorkspace()) throw new Error('The caller or workspace changed before the change was sent.');
      const { file: _file, ...arguments_ } = request as DraftRequest & { file?: File };
      const action: SavedAction = { label, callerPrincipalId: current.callerPrincipalId, principalId: current.principalId, rootObjectId: current.rootObjectId, phase: 'prepared', detail: '', outcome: null,
        call: { qualifiedName: binding.qualifiedName, serviceNodeId: binding.serviceNodeId, expectedDefinitionHash: binding.definitionHash, operationId,
          arguments: { ...arguments_, operationId, expectedWorkspace: { principalId: current.principalId, rootObjectId: current.rootObjectId, callerPrincipalId: current.callerPrincipalId } } as Wire.Json } };
      const observation = request.action === 'archive' ? archiveBase?.() : undefined;
      if (observation) action.archiveBase = { runtimeEpoch: observation.runtimeEpoch, sequence: observation.sequence };
      if (file) {
        action.upload = { byteLength: file.size, contentHash: await hashBytes(new Uint8Array(await file.arrayBuffer())) };
        await retainUpload(key, action, file);
      }
      // Another view may have retained an original while preparation awaited a
      // read, discovery or file storage. Admission must use the current keys.
      if (taskId) assertTaskActionAvailable(workspace(), taskId, scope);
      saved.value = action;
      await persist(); busy.value = false; await execute(); return saved.value.phase === 'succeeded' ? saved.value.outcome : null;
    } catch (cause) { busy.value = false; error.value = cause instanceof Error ? cause.message : 'The action could not be prepared.'; return null; }
  };
  const reconcile = async () => {
    if (disposed || !saved.value || busy.value || !identityMatches()) return;
    busy.value = true; error.value = null;
    let resumeOriginal = false;
    try {
      await checkCurrentIdentity();
      if (disposed) return;
      if (saved.value.call.qualifiedName === 'task-board.archive') {
        const request = saved.value.call.arguments as TaskBoard.ArchiveRequest;
        const outcome = await readArchiveOutcome(client, { objectId: request.taskId, archived: request.archived,
          mutationId: saved.value.call.operationId!, call: saved.value.call });
        if (disposed) return;
        if (outcome) {
          saved.value.phase = outcome.phase;
          saved.value.outcome = outcome.operation?.outcome ?? (outcome.phase === 'succeeded'
            ? archiveActionOutcome(outcome.archive, { objectId: request.taskId, archived: request.archived,
              mutationId: saved.value.call.operationId!, call: saved.value.call }) : null);
          saved.value.detail = outcome.phase === 'succeeded' ? 'The archive action was confirmed.' : outcome.message;
          await persist();
        }
        return;
      }
      let operation: TaskBoard.OperationReceipt;
      try { operation = await readOperation(); }
      catch (cause) {
        if (!(cause instanceof IvyError) || cause.code !== 'not_found') throw cause;
        // Absence is not proof of non-execution. Resubmit the exact retained call:
        // TaskBoard deduplicates its caller/operation identity, including late arrivals.
        resumeOriginal = true;
        return;
      }
      if (disposed) return;
      saved.value.phase = operation.phase === 'succeeded' ? 'succeeded' : operation.phase === 'failed' ? 'failed' : 'pending';
      saved.value.outcome = operation.outcome; saved.value.detail = operation.error?.message ?? 'Original operation: ' + operation.phase + '.'; await persist();
    } catch (cause) {
      if (disposed) return;
      if (cause instanceof IvyError && cause.code === 'operation_expired') {
        // The owner only retires terminal operations. A released receipt cannot
        // be replayed, but must not keep unrelated new comments locked forever.
        saved.value.phase = 'failed';
        saved.value.detail = 'The original action has finished and its receipt was released. Check the task before repeating it.';
        try { await persist(); } catch (problem) { error.value = String(problem); }
      } else error.value = cause instanceof Error ? cause.message : 'The original outcome is unavailable.';
    }
    finally { busy.value = false; if (resumeOriginal) await execute(); }
  };
  // A lost HTTP reply must not leave this tab locked after the owner finishes.
  // Observe the original identity and resume its exact request if it has no receipt.
  let reconciliation: ReturnType<typeof setTimeout> | undefined;
  watch([() => saved.value?.phase, busy], ([phase, running]) => {
    if (reconciliation) clearTimeout(reconciliation);
    if (!running && (phase === 'unknown' || phase === 'pending') && identityMatches() && !storageError.value)
      reconciliation = setTimeout(() => { void reconcile(); }, 5000);
  }, { immediate: true });
  onScopeDispose(() => { disposed = true; if (reconciliation) clearTimeout(reconciliation); });
  const canRetryPreparation = computed(() => !!error.value && !storageError.value && !busy.value && (!saved.value || ['succeeded', 'failed'].includes(saved.value.phase)));
  const canDismiss = computed(() => !busy.value && !storageError.value && identityMatches() && !!saved.value && ['succeeded', 'failed'].includes(saved.value.phase));
  const dismiss = async () => {
    if (!canDismiss.value) return;
    const previous = saved.value;
    saved.value = null;
    try { await persist(); error.value = null; needsOriginalFile.value = false; }
    catch (cause) { saved.value = previous; error.value = cause instanceof Error ? cause.message : 'The action could not be dismissed.'; }
  };
  const restoreFile = async (file: File) => {
    const action = saved.value;
    if (!action?.upload || busy.value || !identityMatches() || ['succeeded', 'failed'].includes(action.phase)) return;
    busy.value = true; error.value = null;
    try {
      if (file.size !== action.upload.byteLength || await hashBytes(new Uint8Array(await file.arrayBuffer())) !== action.upload.contentHash)
        throw new Error('Choose the same file as the original upload.');
      await retainUpload(key, action, file); needsOriginalFile.value = false;
    } catch (cause) { error.value = cause instanceof Error ? cause.message : 'The original file could not be restored.'; }
    finally { busy.value = false; }
  };
  return { saved, error, busy, start, reconcile, replay: execute, canRetryPreparation, needsOriginalFile, restoreFile,
    canDismiss, dismiss,
    retryPreparation: () => { if (canRetryPreparation.value) error.value = null; },
    locked: computed(() => busy.value || !!error.value || !identityMatches() || !!saved.value && !['succeeded', 'failed'].includes(saved.value.phase)),
    identityMatches: computed(identityMatches) };
}
