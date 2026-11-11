import { computed, onScopeDispose, ref, watch } from 'vue';
import type { useAction, DraftRequest } from './action';
import { base, readDocument, tr } from './runtime';
import type { TaskBoard } from './runtime';

type CommentDraft = Omit<Extract<DraftRequest, { action: 'comment' }>, 'taskId' | 'expectedRevision'>;
interface PendingComment { request: CommentDraft; createdAt: string; error: string }

// Keep a small retained conversation buffer, including failed messages and receipt
// confirmations that have not reached the read view yet. It is only a projection.
const maximumMessages = 32, maximumBytes = 4 * 1024 * 1024;

/** Serialize comment writes through the existing action journal while accepting rapid drafts. */
export function useOptimisticComment(workspace: () => TaskBoard.WorkspaceInfo, taskId: string,
  action: ReturnType<typeof useAction>, task: () => TaskBoard.Task | undefined, changed: () => void,
  enabled: () => boolean = () => true, ready: () => boolean = () => true) {
  const identity = () => JSON.stringify([base.href, workspace().serviceNodeId,
    workspace().callerPrincipalId, workspace().principalId, workspace().rootObjectId, taskId]);
  const owner = identity(), key = 'ivy.task-board.pending-comment:' + owner;
  const pending = ref<PendingComment | null>(null), queued = ref<PendingComment[]>([]), failed = ref<PendingComment[]>([]),
    confirmed = ref<PendingComment[]>([]), preparing = ref(false), submissionError = ref('');
  let disposed = false, unreadable = false, controller: AbortController | undefined;
  onScopeDispose(() => { disposed = true; controller?.abort(); });
  try {
    const value = JSON.parse(sessionStorage.getItem(key) ?? 'null');
    if (value?.request?.action === 'comment' && typeof value.request.commentId === 'string') pending.value = value;
    else if (value) {
      pending.value = value.pending ?? null;
      failed.value = value.failed ?? [];
      queued.value = value.queued ?? [];
      confirmed.value = value.confirmed ?? [];
    }
  } catch {
    unreadable = true;
    submissionError.value = tr('Gesicherte Nachrichten sind nicht lesbar. Bitte den Browserspeicher wiederherstellen.', 'Retained messages cannot be read. Restore browser storage before sending.');
  }
  const ownAction = computed(() => action.saved.value?.call.arguments as Extract<TaskBoard.ActionInput, { action: 'comment' }> | undefined);
  const isComment = computed(() => ownAction.value?.action === 'comment');
  if (!pending.value && isComment.value && action.saved.value?.phase !== 'succeeded' &&
      !failed.value.some(message => message.request.commentId === ownAction.value?.commentId)) {
    const { operationId: _operation, expectedRevision: _revision, taskId: _task, ...request } = ownAction.value!;
    pending.value = { request, createdAt: new Date().toISOString(), error: '' };
  }
  const persist = (nextPending = pending.value, nextFailed = failed.value, nextQueued = queued.value, nextConfirmed = confirmed.value) => {
    try {
      if (unreadable) return false;
      if (nextPending || nextFailed.length || nextQueued.length || nextConfirmed.length)
        sessionStorage.setItem(key, JSON.stringify({ pending: nextPending, failed: nextFailed, queued: nextQueued, confirmed: nextConfirmed }));
      else sessionStorage.removeItem(key);
      return true;
    } catch {
      submissionError.value = tr('Nachricht konnte nicht im Browser gesichert werden. Dein Entwurf bleibt erhalten.', 'The message could not be retained in this browser. Your draft is retained.');
      return false;
    }
  };
  const sameAction = () => isComment.value && ownAction.value?.commentId === pending.value?.request.commentId;
  const retained = () => [...failed.value, ...confirmed.value, ...(pending.value ? [pending.value] : []), ...queued.value];
  const identityMatches = () => owner === identity() && action.identityMatches.value;
  const waitingForAction = () => {
    const saved = action.saved.value, request = saved?.call.arguments as { taskId?: string; action?: string } | undefined;
    return !saved ? action.busy.value : request?.taskId === taskId && request.action !== 'archive' &&
      (action.busy.value || !['failed', 'succeeded'].includes(saved.phase));
  };
  const canSubmit = computed(() => !disposed && !unreadable && enabled() && identityMatches() &&
    (!action.locked.value || waitingForAction() || !!pending.value && (sameAction() || preparing.value)) && retained().length < maximumMessages);
  const submissionIssue = computed(() => submissionError.value || (retained().length >= maximumMessages
    ? tr('Einige Nachrichten sind noch nicht abgeglichen. Dein weiterer Entwurf bleibt erhalten.', 'Some messages are still awaiting confirmation. Your next draft is retained.') : ''));
  const sending = (commentId: string) => pending.value?.request.commentId === commentId || queued.value.some(message => message.request.commentId === commentId);
  watch([() => task(), () => action.saved.value?.phase, action.busy, action.locked, preparing,
    () => queued.value.length, enabled, ready, identity], () => {
    if (disposed || !identityMatches()) return;
    const observed = new Set(task()?.comments.map(comment => comment.commentId));
    let nextConfirmed = confirmed.value.filter(message => !observed.has(message.request.commentId));
    let nextPending = pending.value && !observed.has(pending.value.request.commentId) ? pending.value : null;
    let nextFailed = failed.value.filter(message => !observed.has(message.request.commentId));
    let nextQueued = queued.value.filter(message => !observed.has(message.request.commentId));
    if (nextPending && sameAction() && action.saved.value?.phase === 'succeeded') {
      nextConfirmed = [...nextConfirmed, nextPending]; nextPending = null;
    }
    const terminal = nextPending && !preparing.value && !action.busy.value &&
      (sameAction() ? action.saved.value!.phase === 'failed' : nextPending.error && (!action.locked.value || action.canRetryPreparation.value));
    if (terminal && nextPending) {
      nextFailed = [...nextFailed, { ...nextPending, error: nextPending.error || action.error.value || action.saved.value?.detail ||
        tr('Nachricht konnte nicht gespeichert werden.', 'The message could not be saved.') }];
      nextPending = null;
    }
    const promote = !nextPending && nextQueued.length && !preparing.value && !action.locked.value && enabled() && ready();
    if (promote) { nextPending = nextQueued[0]!; nextQueued = nextQueued.slice(1); }
    if (nextPending === pending.value && nextFailed.length === failed.value.length &&
        nextQueued.length === queued.value.length && nextConfirmed.length === confirmed.value.length) return;
    if (!persist(nextPending, nextFailed, nextQueued, nextConfirmed)) return;
    pending.value = nextPending;
    failed.value = nextFailed;
    queued.value = nextQueued;
    confirmed.value = nextConfirmed;
    if (terminal && action.canRetryPreparation.value) action.retryPreparation();
    if (promote) queueMicrotask(() => void send());
  }, { immediate: true });
  const messages = computed<TaskBoard.Comment[]>(() => {
    const current = workspace(), sequence = Math.max(0, ...(task()?.comments.map(comment => comment.sequence) ?? []));
    return retained().sort((a, b) => a.createdAt.localeCompare(b.createdAt)).map((value, index) => ({
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
    if (disposed || !enabled() || !ready() || !identityMatches() || !pending.value || preparing.value || action.busy.value) return;
    if (action.locked.value && !sameAction() && !action.canRetryPreparation.value) return;
    const message = pending.value;
    preparing.value = true;
    pending.value.error = '';
    try {
      if (!persist()) return;
      if (task()?.comments.some(comment => comment.commentId === message.request.commentId)) {
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
      controller = new AbortController();
      const current = await readDocument('task', taskId, workspace().rootObjectId,
        AbortSignal.any([controller.signal, AbortSignal.timeout(15000)]), { limit: 0 });
      if (disposed || !enabled() || !identityMatches() || pending.value !== message) return;
      const outcome = await action.start('Add task comment', { ...message.request, taskId, expectedRevision: current.pin.revision });
      if (outcome) changed();
      else if (pending.value === message && !sameAction()) message.error = action.error.value ?? tr('Nachricht konnte nicht gesendet werden.', 'The message could not be sent.');
    } catch (cause) {
      if (!disposed && pending.value === message) message.error = cause instanceof Error ? cause.message : String(cause);
    } finally { preparing.value = false; controller = undefined; if (!disposed) persist(); }
  };
  const submit = (request: CommentDraft) => {
    if (disposed || !canSubmit.value || retained().some(message => message.request.commentId === request.commentId)) return false;
    const message = { request, createdAt: new Date().toISOString(), error: '' };
    const next = pending.value || preparing.value ? [...queued.value, message] : queued.value;
    const nextPending = pending.value || preparing.value ? pending.value : message;
    if (new TextEncoder().encode(JSON.stringify([nextPending, next, failed.value, confirmed.value])).byteLength > maximumBytes) {
      submissionError.value = tr('Die gesicherten Nachrichten sind zu groß. Dein Entwurf bleibt erhalten, bis Nachrichten abgeglichen sind.', 'Retained messages are too large. Your draft is retained until messages are reconciled.'); return false;
    }
    if (!persist(nextPending, failed.value, next)) return false;
    pending.value = nextPending; queued.value = next; submissionError.value = '';
    if (!preparing.value && !action.locked.value && ready()) void send();
    return true;
  };
  watch(ready, allowed => { if (allowed && pending.value && !preparing.value && !action.locked.value) void send(); });
  const canRetry = (commentId: string) => enabled() && ready() && !disposed && !preparing.value && !action.busy.value && identityMatches() &&
    (pending.value ? pending.value.request.commentId === commentId && (sameAction() || !action.locked.value || action.canRetryPreparation.value)
      : !action.locked.value || action.canRetryPreparation.value);
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
  // Recover retained drafts on mount and after an outage without replaying an
  // unknown operation under a new identity. useAction owns that reconciliation.
  watch([enabled, identity, () => pending.value?.request.commentId, preparing, action.locked], () => {
    if (!disposed && pending.value && !pending.value.error && !preparing.value && !action.locked.value && enabled()) void send();
  }, { immediate: true });
  return { pending, queued, preparing, messages, issue, isComment, sending, canSubmit, submissionIssue, submit, retry, canRetry, discard, canDiscard };
}
