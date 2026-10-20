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
  const pending = ref<PendingComment | null>(null), failed = ref<PendingComment[]>([]), preparing = ref(false);
  try {
    const value = JSON.parse(sessionStorage.getItem(key) ?? 'null');
    if (value?.request?.action === 'comment' && typeof value.request.commentId === 'string') pending.value = value;
    else if (value) {
      pending.value = value.pending ?? null;
      failed.value = value.failed ?? [];
    }
  } catch { /* A retained workflow request below still recovers the original message. */ }
  const ownAction = computed(() => action.saved.value?.call.arguments as Extract<TaskBoard.ActionInput, { action: 'comment' }> | undefined);
  const isComment = computed(() => ownAction.value?.action === 'comment');
  if (!pending.value && isComment.value && action.saved.value?.phase !== 'succeeded' &&
      !failed.value.some(message => message.request.commentId === ownAction.value?.commentId)) {
    const { operationId: _operation, expectedRevision: _revision, taskId: _task, ...request } = ownAction.value!;
    pending.value = { request, createdAt: new Date().toISOString(), error: '' };
  }
  const persist = (nextPending = pending.value, nextFailed = failed.value) => {
    try {
      if (nextPending || nextFailed.length) sessionStorage.setItem(key, JSON.stringify({ pending: nextPending, failed: nextFailed }));
      else sessionStorage.removeItem(key);
      return true;
    } catch {
      if (pending.value) pending.value.error = tr('Nachricht konnte nicht im Browser gesichert werden. Bitte erneut versuchen.', 'The message could not be retained in this browser. Please retry.');
      return false;
    }
  };
  const sameAction = () => isComment.value && ownAction.value?.commentId === pending.value?.request.commentId;
  watch([() => task(), () => action.saved.value?.phase, action.busy, preparing], () => {
    const confirmed = new Set(task()?.comments.map(comment => comment.commentId));
    // An authoritative receipt stays confirmed even when pagination no longer
    // includes that comment. It must not become a new pending send on reopening.
    if (isComment.value && action.saved.value?.phase === 'succeeded') confirmed.add(ownAction.value!.commentId);
    let nextPending = pending.value && !confirmed.has(pending.value.request.commentId) ? pending.value : null;
    let nextFailed = failed.value.filter(message => !confirmed.has(message.request.commentId));
    const terminal = nextPending && !preparing.value && !action.busy.value &&
      (sameAction() ? action.saved.value!.phase === 'failed' : nextPending.error && (!action.locked.value || action.canRetryPreparation.value));
    if (terminal && nextPending) {
      nextFailed = [...nextFailed, { ...nextPending, error: nextPending.error || action.error.value || action.saved.value?.detail ||
        tr('Nachricht konnte nicht gespeichert werden.', 'The message could not be saved.') }];
      nextPending = null;
    }
    if (nextPending === pending.value && nextFailed.length === failed.value.length) return;
    if (!persist(nextPending, nextFailed)) return;
    pending.value = nextPending;
    failed.value = nextFailed;
    if (terminal && action.canRetryPreparation.value) action.retryPreparation();
  }, { immediate: true });
  const messages = computed<TaskBoard.Comment[]>(() => {
    const current = workspace(), sequence = Math.max(0, ...(task()?.comments.map(comment => comment.sequence) ?? []));
    return [...failed.value, ...(pending.value ? [pending.value] : [])].map((value, index) => ({
      ...value.request, operationId: ownAction.value?.commentId === value.request.commentId ? action.saved.value!.call.operationId! : value.request.commentId,
      sequence: sequence + index + 1,
      author: { principalId: current.callerPrincipalId, serviceNodeId: current.serviceNodeId, source: 'user', generation: 0 },
      authorKind: 'user', delivery: null, run: null, turnId: null, createdAt: value.createdAt,
    }));
  });
  const issue = (commentId: string) => {
    const failure = failed.value.find(message => message.request.commentId === commentId);
    if (failure) return failure.error;
    if (pending.value?.request.commentId !== commentId) return '';
    if (!pending.value || preparing.value || action.busy.value) return '';
    if (pending.value.error) return pending.value.error;
    if (!sameAction()) return tr('Nachricht noch nicht gesendet. Dein Text und die Anhänge bleiben erhalten.', 'The message has not been sent. Your text and attachments are retained.');
    if (action.saved.value!.phase === 'failed') return action.error.value || action.saved.value!.detail ||
      tr('Nachricht konnte nicht gespeichert werden. Dein Text und die Anhänge bleiben erhalten.', 'The message could not be saved. Your text and attachments are retained.');
    return action.error.value || (['prepared', 'unknown', 'pending'].includes(action.saved.value!.phase)
      ? tr('Nachricht noch nicht bestätigt. Dein Text und die Anhänge bleiben erhalten.', 'The message is not confirmed. Your text and attachments are retained.') : '');
  };
  const send = async () => {
    if (!pending.value || preparing.value || action.busy.value) return;
    preparing.value = true;
    pending.value.error = '';
    try {
      if (!persist()) return;
      if (task()?.comments.some(comment => comment.commentId === pending.value!.request.commentId)) {
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
      // Sending only needs the current revision. Reading the complete conversation
      // here blocked the composer before the comment request even reached its owner.
      const current = await readDocument('task', taskId, workspace().rootObjectId,
        AbortSignal.timeout(15000), { limit: 0 });
      if (!pending.value) return;
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
  const canRetry = (commentId: string) => !preparing.value && !action.busy.value && action.identityMatches.value &&
    (pending.value ? pending.value.request.commentId === commentId : !action.locked.value || action.canRetryPreparation.value);
  const retry = (commentId: string) => {
    if (!canRetry(commentId)) return;
    if (!pending.value) {
      const message = failed.value.find(value => value.request.commentId === commentId);
      if (!message) return;
      const remaining = failed.value.filter(value => value !== message);
      if (!persist(message, remaining)) return;
      pending.value = message;
      failed.value = remaining;
    }
    void send();
  };
  const canDiscard = (commentId: string) => failed.value.some(message => message.request.commentId === commentId) &&
    (ownAction.value?.commentId !== commentId || action.canDismiss.value);
  const discard = (commentId: string) => {
    if (!canDiscard(commentId)) return;
    if (ownAction.value?.commentId === commentId) {
      action.dismiss();
      if (ownAction.value?.commentId === commentId) return;
    }
    const remaining = failed.value.filter(message => message.request.commentId !== commentId);
    if (persist(pending.value, remaining)) failed.value = remaining;
  };
  watch([() => action.saved.value?.phase, action.busy], ([phase, busy]) => {
    if (isComment.value && phase === 'succeeded' && !busy) changed();
  });
  return { pending, preparing, messages, issue, isComment, submit, retry, canRetry, discard, canDiscard };
}
