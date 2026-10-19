import { computed, ref, watch } from 'vue';
import type { useAction, DraftRequest } from './action';
import { base, readDocument, tr } from './runtime';
import type { TaskBoard } from './runtime';

type CommentDraft = Omit<Extract<DraftRequest, { action: 'comment' }>, 'taskId' | 'expectedRevision'>;
interface PendingComment { request: CommentDraft; createdAt: string; error: string }

/** Keep one original message through preparation, lost replies, reloads and explicit retries. */
export function useOptimisticComment(workspace: () => TaskBoard.WorkspaceInfo, taskId: string,
  action: ReturnType<typeof useAction>, task: () => TaskBoard.Task | undefined, changed: () => void) {
  const key = 'ivy.task-board.pending-comment:' + JSON.stringify([base.href, workspace().serviceNodeId,
    workspace().callerPrincipalId, workspace().principalId, workspace().rootObjectId, taskId]);
  const pending = ref<PendingComment | null>(null), preparing = ref(false);
  try {
    const value = JSON.parse(sessionStorage.getItem(key) ?? 'null');
    if (value?.request?.action === 'comment' && typeof value.request.commentId === 'string') pending.value = value;
  } catch { /* A retained workflow request below still recovers the original message. */ }
  const ownAction = computed(() => action.saved.value?.call.arguments as Extract<TaskBoard.ActionInput, { action: 'comment' }> | undefined);
  const isComment = computed(() => ownAction.value?.action === 'comment');
  if (!pending.value && isComment.value) {
    const { operationId: _operation, expectedRevision: _revision, taskId: _task, ...request } = ownAction.value!;
    pending.value = { request, createdAt: new Date().toISOString(), error: '' };
  }
  const persist = () => {
    try {
      if (pending.value) sessionStorage.setItem(key, JSON.stringify(pending.value));
      else sessionStorage.removeItem(key);
      return true;
    } catch {
      if (pending.value) pending.value.error = tr('Nachricht konnte nicht im Browser gesichert werden. Bitte erneut versuchen.', 'The message could not be retained in this browser. Please retry.');
      return false;
    }
  };
  const sameAction = () => isComment.value && ownAction.value?.commentId === pending.value?.request.commentId;
  const confirmed = computed(() => !!pending.value && task()?.comments.some(comment => comment.commentId === pending.value!.request.commentId));
  watch(confirmed, value => { if (value) { pending.value = null; persist(); } }, { immediate: true });
  const message = computed<TaskBoard.Comment | null>(() => {
    const value = pending.value;
    if (!value || confirmed.value) return null;
    const current = workspace();
    return { ...value.request, operationId: sameAction() ? action.saved.value!.call.operationId! : value.request.commentId,
      sequence: Math.max(0, ...(task()?.comments.map(comment => comment.sequence) ?? [])) + 1,
      author: { principalId: current.callerPrincipalId, serviceNodeId: current.serviceNodeId, source: 'user', generation: 0 },
      authorKind: 'user', delivery: null, run: null, turnId: null, createdAt: value.createdAt };
  });
  const issue = computed(() => {
    if (!pending.value || preparing.value || action.busy.value) return '';
    if (pending.value.error) return pending.value.error;
    if (!sameAction()) return tr('Nachricht noch nicht gesendet. Dein Text und die Anhänge bleiben erhalten.', 'The message has not been sent. Your text and attachments are retained.');
    return action.error.value || (['failed', 'prepared', 'unknown', 'pending'].includes(action.saved.value!.phase)
      ? tr('Nachricht noch nicht bestätigt. Dein Text und die Anhänge bleiben erhalten.', 'The message is not confirmed. Your text and attachments are retained.') : '');
  });
  const send = async () => {
    if (!pending.value || preparing.value || action.busy.value) return;
    preparing.value = true;
    pending.value.error = '';
    try {
      if (!persist()) return;
      const current = await readDocument('task', taskId, workspace().rootObjectId);
      if (!pending.value) return;
      if (current.value.comments.some(comment => comment.commentId === pending.value!.request.commentId)) {
        pending.value = null; persist(); changed(); return;
      }
      if (sameAction() && action.saved.value!.phase === 'prepared') {
        await action.replay(); changed(); return;
      }
      // Observe unknown outcomes under their original identity; only a failed operation permits a new attempt.
      if (sameAction() && action.saved.value!.phase !== 'failed') {
        await action.reconcile();
        if (!pending.value) return;
        if (!['failed'].includes(action.saved.value?.phase ?? '')) { changed(); return; }
      }
      if (action.canRetryPreparation.value) action.retryPreparation();
      const outcome = await action.start('Add task comment', { ...pending.value.request, taskId, expectedRevision: current.pin.revision });
      if (outcome) changed();
      else if (pending.value && !sameAction()) pending.value.error = action.error.value ?? tr('Nachricht konnte nicht gesendet werden.', 'The message could not be sent.');
    } catch (cause) {
      if (pending.value) pending.value.error = cause instanceof Error ? cause.message : String(cause);
    } finally { preparing.value = false; persist(); }
  };
  const submit = (request: CommentDraft) => {
    if (pending.value || preparing.value || action.locked.value) return false;
    pending.value = { request, createdAt: new Date().toISOString(), error: '' };
    void send();
    return true;
  };
  watch([() => action.saved.value?.phase, action.busy], ([phase, busy]) => {
    if (isComment.value && phase === 'succeeded' && !busy) changed();
  });
  return { pending, preparing, message, issue, isComment, submit, retry: send };
}
