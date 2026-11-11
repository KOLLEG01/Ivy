import { computed, effectScope, onScopeDispose, ref, shallowReactive, shallowRef, watch } from 'vue';
import type { ShallowRef } from 'vue';
import { actionStorageKey, assertTaskActionAvailable, useAction } from './action';
import type { SavedAction } from './action';
import { useArchiveProjection, useStatusProjection } from './status-projection';
import type { TaskBoard } from './runtime';

export type BoardProjection = { card: TaskBoard.TaskCardView; state: TaskBoard.Task['workflowState']; archived?: boolean;
  statusPending?: boolean; archivePending?: boolean; fromDetail?: boolean; confirmedRevision?: number; archiveEpoch?: string };
export type TaskActions = { task: ReturnType<typeof useAction>; edit: ReturnType<typeof useAction> };

/** Independent ticket slots use the existing retained action and recovery path. */
export function useBoardActions(workspace: () => TaskBoard.WorkspaceInfo, synchronizeCards?: (ids: string[]) => Promise<boolean>,
  visibleCard?: (id: string) => TaskBoard.TaskCardView | null, selectedTask?: () => string | null) {
  const maximum = 32, prefix = actionStorageKey(workspace(), 'board:task:');
  const identity = () => { const value = workspace(); return JSON.stringify([value.serviceNodeId, value.callerPrincipalId, value.principalId, value.rootObjectId]); };
  const owner = identity();
  type Slot = { action: ReturnType<typeof useAction>; scope: ReturnType<typeof effectScope>;
    card: ShallowRef<TaskBoard.TaskCardView | null>; detail: ShallowRef<TaskActions | null>; peer: ShallowRef<SavedAction | null>; projection: ReturnType<typeof useStatusProjection>;
    archive: ReturnType<typeof useArchiveProjection> };
  const slots = shallowReactive(new Map<string, Slot>());
  const active = shallowReactive(new Set<string>());
  const error = ref<{ taskId: string; message: string } | null>(null);
  const recoveryError = ref('');
  let disposed = false;
  const terminal = (saved: SavedAction) => ['succeeded', 'failed'].includes(saved.phase);
  const selected = (own: SavedAction | null, peer: SavedAction | null, running: boolean) => {
    if (running || !peer || own && !terminal(own)) return own;
    if (!own || !terminal(peer)) return peer;
    const revision = (saved: SavedAction) => (saved.call.arguments as { expectedRevision?: number }).expectedRevision ?? -1;
    if (revision(own) !== revision(peer)) return revision(own) > revision(peer) ? own : peer;
    const a = own.outcome?.archive?.observation, b = peer.outcome?.archive?.observation;
    return a && b && a.runtimeEpoch === b.runtimeEpoch && a.sequence > b.sequence ? own : peer;
  };
  const add = (taskId: string) => {
    const previous = slots.get(taskId);
    if (previous) return previous;
    if (slots.size >= maximum) throw new Error('Resolve a previous task change before starting more changes in this tab.');
    const retained = sessionStorage.getItem(actionStorageKey(workspace(), 'board:task:' + encodeURIComponent(taskId)));
    if (retained && JSON.parse(retained).call?.arguments?.taskId !== taskId)
      throw new Error('The retained change belongs to another task. Its original request has been preserved.');
    const scope = effectScope(true);
    const card = shallowRef<TaskBoard.TaskCardView | null>(null);
    const peer = shallowRef<SavedAction | null>(null);
    const detail = shallowRef<TaskActions | null>(null);
    const slot = scope.run(() => {
      const action = useAction(workspace, 'board:task:' + encodeURIComponent(taskId));
      // The original detail owner lives in this slot; closing its view keeps the
      // same sender/recovery path. The peer is only its last display watermark.
      const saved = computed(() => selected(action.saved.value, peer.value, active.has(taskId) || action.busy.value));
      const display = { saved, identityMatches: action.identityMatches,
        error: computed(() => saved.value === action.saved.value ? action.error.value : null) };
      return { action, scope, card, detail, peer, projection: useStatusProjection(workspace, taskId, display, () => card.value),
        archive: useArchiveProjection(workspace, taskId, display, () => card.value) };
    })!;
    slots.set(taskId, slot);
    return slot;
  };
  const attachDetail = (taskId: string, slot: Slot) => {
    if (slot.detail.value) return slot.detail.value;
    slot.detail.value = slot.scope.run(() => {
      const task = useAction(workspace, 'task:' + taskId), edit = useAction(workspace, 'edit:' + taskId);
      const display = () => {
        const saved = task.saved.value;
        if (task.identityMatches.value && saved && ['task-board.transition', 'task-board.archive'].includes(saved.call.qualifiedName ?? '') &&
            (saved.call.arguments as { taskId?: string }).taskId === taskId) slot.peer.value = saved;
      };
      display();
      watch(task.saved, () => {
        if (!slot.card.value) slot.card.value = visibleCard?.(taskId) ?? null;
        display();
      }, { flush: 'sync' });
      return { task, edit };
    })!;
    return slot.detail.value;
  };
  try {
    for (let index = 0; index < sessionStorage.length; index++) {
      const key = sessionStorage.key(index);
      if (key?.startsWith(prefix)) add(decodeURIComponent(key.slice(prefix.length)));
    }
    for (const name of ['task:', 'edit:']) {
      const prefix = actionStorageKey(workspace(), name);
      for (let index = 0; index < sessionStorage.length; index++) {
        const key = sessionStorage.key(index);
        if (!key?.startsWith(prefix)) continue;
        const taskId = key.slice(prefix.length), saved = JSON.parse(sessionStorage.getItem(key)!) as SavedAction;
        if ((saved.call?.arguments as { taskId?: string })?.taskId === taskId && saved.call.operationId &&
            saved.call.serviceNodeId === workspace().serviceNodeId && saved.callerPrincipalId === workspace().callerPrincipalId &&
            saved.principalId === workspace().principalId && saved.rootObjectId === workspace().rootObjectId)
          attachDetail(taskId, slots.get(taskId) ?? add(taskId));
      }
    }
  } catch {
    recoveryError.value = 'Some previous task changes could not be loaded. Their original requests remain in this tab.';
  }
  onScopeDispose(() => { disposed = true; for (const slot of slots.values()) slot.scope.stop(); });
  const observe = (cards: TaskBoard.TaskCardView[]) => {
    if (disposed || identity() !== owner) return;
    for (const card of cards) {
      const slot = slots.get(card.object.objectId);
      const previous = slot?.card.value;
      if (slot && (!previous || card.object.revision > previous.object.revision || card.object.revision === previous.object.revision &&
          !(card.observation && previous.observation && card.observation.runtimeEpoch === previous.observation.runtimeEpoch &&
            card.observation.sequence < previous.observation.sequence))) slot.card.value = card;
    }
  };
  const observedCommit = (slot: Slot, saved = slot.action.saved.value) => {
    const archive = saved?.outcome?.archive;
    if (saved?.call.qualifiedName === 'task-board.archive') {
      const read = slot.card.value?.observation, receipt = archive?.observation;
      // Archive metadata does not increment the content revision. Only a real
      // card read beyond the receipt can retire this display watermark.
      return !!read && !!receipt && read.runtimeEpoch === receipt.runtimeEpoch && read.sequence >= receipt.sequence;
    }
    const outcome = saved?.outcome?.task;
    return !!slot.card.value && !!outcome && outcome.objectId === slot.card.value.object.objectId && slot.card.value.object.revision >= outcome.revision;
  };
  const peerSettled = (slot: Slot) => !slot.peer.value || slot.peer.value.phase === 'failed' ||
    slot.peer.value.phase === 'succeeded' && observedCommit(slot, slot.peer.value);
  const actions = (slot: Slot) => [slot.action, ...(slot.detail.value ? [slot.detail.value.task, slot.detail.value.edit] : [])];
  const idle = (id: string, slot: Slot) => !disposed && identity() === owner && slots.get(id) === slot &&
    !active.has(id) && id !== selectedTask?.() && actions(slot).every(action => !action.busy.value && !action.error.value &&
      action.identityMatches.value && (!action.saved.value || action.saved.value.phase === 'succeeded'));
  let releasing: Promise<void> | undefined;
  const release = () => {
    if (releasing) return releasing;
    releasing = (async () => {
      if (disposed || identity() !== owner) return;
      const candidates = [...slots].filter(([id, slot]) => idle(id, slot));
      const confirmed = candidates.filter(([, slot]) => actions(slot).some(action => action.saved.value) || !!slot.peer.value).map(([id]) => id);
      // Use the same bounded card/page fence as capacity retirement. A receipt
      // alone cannot release a display watermark while another column is stale.
      const ready = !confirmed.length || !synchronizeCards || await synchronizeCards(confirmed);
      // Keep unknown/failed originals, the selected view and unobserved display
      // watermarks. Both detail journals share this existing bounded ticket slot.
      for (const [id, previous] of candidates) {
        if (!idle(id, previous) || confirmed.includes(id) && !ready) continue;
        for (const action of actions(previous)) {
          if (!idle(id, previous)) break;
          if (action.saved.value?.phase === 'succeeded' && action.canDismiss.value && observedCommit(previous, action.saved.value))
            await action.dismiss();
        }
        if (idle(id, previous) && peerSettled(previous) && actions(previous).every(action => !action.saved.value)) {
          previous.scope.stop(); previous.peer.value = null; previous.card.value = null; previous.detail.value = null; slots.delete(id);
        }
      }
    })().finally(() => { releasing = undefined; });
    return releasing;
  };
  const retain = async (taskId: string) => {
    if (disposed || identity() !== owner) throw new Error('The task workspace changed.');
    let slot = slots.get(taskId);
    if (slot) return slot;
    if (slots.size >= maximum) await release();
    if (disposed || identity() !== owner) throw new Error('The task workspace changed.');
    slot = slots.get(taskId) ?? add(taskId);
    return slot;
  };
  const detail = async (taskId: string) => {
    const slot = await retain(taskId);
    if (disposed || identity() !== owner || slots.get(taskId) !== slot) throw new Error('The task workspace changed.');
    if (!slot.card.value) slot.card.value = visibleCard?.(taskId) ?? null;
    return attachDetail(taskId, slot);
  };
  const run = async (taskId: string, work: (action: ReturnType<typeof useAction>, archive: Slot['archive']) => Promise<void>,
    intent?: { card: TaskBoard.TaskCardView; state: TaskBoard.Task['workflowState'] } | { card: TaskBoard.TaskCardView; archived: boolean }) => {
    if (disposed || identity() !== owner || active.has(taskId) || slots.get(taskId)?.action.locked.value) return;
    active.add(taskId);
    let slot = slots.get(taskId);
    let cancelPreparation: (() => void) | undefined;
    try {
      assertTaskActionAvailable(workspace(), taskId, 'board:task:' + encodeURIComponent(taskId));
      if (intent && intent.card.object.objectId !== taskId) throw new Error('The displayed card belongs to another task.');
      if (!slot) slot = slots.size < maximum ? add(taskId) : await retain(taskId);
      if (disposed || identity() !== owner) return;
      error.value = null;
      if (intent) {
        observe([intent.card]);
        cancelPreparation = 'archived' in intent ? slot.archive.begin(intent.archived).cancel : slot.projection.begin(intent.state);
      }
      await work(slot.action, slot.archive);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : 'The task change could not be prepared.';
      if (slot) slot.action.error.value = message;
      else error.value = { taskId, message };
    } finally { cancelPreparation?.(); active.delete(taskId); }
  };
  return {
    run, detail, observe, release, error, recoveryError,
    clearCards: () => { for (const slot of slots.values()) slot.card.value = null; },
    projections: computed<BoardProjection[]>(() => identity() !== owner ? [] : [...slots].flatMap(([id, slot]) => {
      if (!slot.card.value || !slot.action.identityMatches.value) return [];
      const saved = selected(slot.action.saved.value, slot.peer.value, active.has(id) || slot.action.busy.value);
      const confirmed = saved?.phase === 'succeeded' ? saved.outcome?.task : null;
      const epoch = saved?.archiveBase?.runtimeEpoch ?? slot.card.value.observation?.runtimeEpoch;
      return [{ card: slot.card.value, state: slot.projection.state.value, archived: slot.archive.state.value,
        statusPending: slot.projection.pending.value, archivePending: slot.archive.pending.value, fromDetail: !!saved && saved === slot.peer.value,
        ...(confirmed?.objectId === id ? { confirmedRevision: confirmed.revision } : {}), ...(epoch ? { archiveEpoch: epoch } : {}) }];
    })),
    active: computed(() => [...active]),
    entries: computed(() => [...slots].map(([taskId, slot]) => ({ taskId, action: slot.action }))),
    detailEntries: computed(() => [...slots].flatMap(([taskId, slot]) => slot.detail.value ?
      [{ taskId, action: slot.detail.value.task }, { taskId, action: slot.detail.value.edit }] : [])),
    boardBlocked: computed(() => [...new Set([...active, ...[...slots].filter(([, slot]) => slot.action.locked.value).map(([id]) => id)])]),
    blocked: computed(() => [...new Set([...active, ...[...slots].filter(([, slot]) => actions(slot).some(action => action.locked.value)).map(([id]) => id)])]),
  };
}
