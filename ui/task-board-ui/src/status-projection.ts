import { computed, shallowRef, watch } from 'vue';
import type { SavedAction, useAction } from './action';
import type { Operation, TaskBoard } from './runtime';

type State = TaskBoard.Task['workflowState'];
type Observed<T> = { revision: number; value: T };
type Projection<T> = { value: T; base: Observed<T>; previousOperationId?: string | undefined;
  operationId?: string | undefined; confirmedRevision?: number };
type Request<T> = { value: T; revision: number };
type ProjectionAction = Pick<ReturnType<typeof useAction>, 'saved' | 'identityMatches' | 'error'>;

/** One display-only value and watermark, using the original action journal. */
function useProjection<T, V extends { object: TaskBoard.ObjectPin }>(workspace: () => TaskBoard.WorkspaceInfo, taskId: string,
  action: ProjectionAction, view: () => V | null, select: (view: V) => T,
  request: (saved: SavedAction) => Request<T> | null, same: (a: T, b: T) => boolean) {
  const identity = () => { const value = workspace(); return JSON.stringify([
    value.serviceNodeId, value.callerPrincipalId, value.principalId, value.rootObjectId,
  ]); };
  const owner = identity(), observed = shallowRef<Observed<T> | null>(null), pending = shallowRef<Projection<T> | null>(null);
  const belongs = () => identity() === owner && action.identityMatches.value && view()?.object.objectId === taskId;
  const observeAction = () => {
    const saved = action.saved.value;
    if (!belongs() || !saved || !saved.call.operationId || !observed.value) return;
    const selection = request(saved);
    if (!selection || !Number.isSafeInteger(selection.revision)) return;
    let current = pending.value;
    // A later original may be observed from another view of the same ticket.
    // Its real revision must include the previously confirmed change.
    if (current?.operationId && current.operationId !== saved.call.operationId &&
        current.confirmedRevision !== undefined && selection.revision >= current.confirmedRevision) current = null;
    if (!current) {
      if (saved.phase === 'failed') return;
      current = { value: selection.value, base: { revision: selection.revision, value: observed.value.value }, operationId: saved.call.operationId };
    } else if (!current.operationId) {
      if (saved.call.operationId === current.previousOperationId || !same(selection.value, current.value)) return;
      current = { ...current, operationId: saved.call.operationId };
    }
    if (current.operationId !== saved.call.operationId) return;
    if (saved.phase === 'failed') { pending.value = null; return; }
    if (saved.phase === 'succeeded' && saved.outcome?.task?.objectId === taskId)
      current = { ...current, confirmedRevision: saved.outcome.task.revision };
    pending.value = current;
  };
  const observe = (value: V) => {
    if (!belongs() || value.object.objectId !== taskId) return;
    if (!observed.value || value.object.revision >= observed.value.revision)
      observed.value = { revision: value.object.revision, value: select(value) };
    observeAction();
  };
  watch([() => { const value = view(); return value ? [value.object.objectId, value.object.revision, select(value)] : null; }, identity],
    () => { const value = view(); if (value) observe(value); }, { immediate: true, flush: 'sync' });
  watch([() => action.saved.value?.call.operationId, () => action.saved.value?.phase, () => action.saved.value?.outcome],
    observeAction, { immediate: true, flush: 'sync' });
  const value = computed(() => {
    const current = pending.value, latest = observed.value, raw = view();
    if (!belongs() || !latest) return raw ? select(raw) : null;
    if (!current) return latest.value;
    if (action.saved.value?.phase === 'prepared' && current.operationId === action.saved.value.call.operationId && action.error?.value)
      return latest.value;
    if (current.confirmedRevision !== undefined) return latest.revision >= current.confirmedRevision ? latest.value : current.value;
    if (latest.revision > current.base.revision && !same(latest.value, current.base.value) && !same(latest.value, current.value)) return latest.value;
    return current.value;
  });
  const begin = (value: T) => {
    const raw = view();
    if (!belongs() || !raw) return () => {};
    const current: Projection<T> = { value, base: observed.value ?? { revision: raw.object.revision, value: select(raw) },
      previousOperationId: action.saved.value?.call.operationId };
    pending.value = current;
    // Only unsent preparation can be cancelled. Unknown writes keep their original projection.
    return () => { if (pending.value === current) pending.value = null; };
  };
  return { value, begin, observe, observed: computed(() => belongs() ? observed.value : null), pending: computed(() => belongs() && !!pending.value &&
    (!pending.value.operationId || pending.value.operationId === action.saved.value?.call.operationId)) };
}

type StatusView = { object: TaskBoard.ObjectPin; task: { workflowState: State } };
export function useStatusProjection(workspace: () => TaskBoard.WorkspaceInfo, taskId: string,
  action: ProjectionAction, view: () => StatusView | null) {
  const projection = useProjection(workspace, taskId, action, view, value => value.task.workflowState, saved => {
    const request = saved.call.arguments as TaskBoard.TransitionRequest;
    return saved.call.qualifiedName === 'task-board.transition' && request.action === 'transition' && request.taskId === taskId && request.workflowState
      ? { value: request.workflowState, revision: request.expectedRevision } : null;
  }, (a, b) => a === b);
  return { state: computed(() => projection.value.value ?? 'todo'), begin: projection.begin, pending: projection.pending,
    observe: projection.observe,
    actual: computed(() => projection.observed.value?.value ?? view()?.task.workflowState ?? 'todo'),
    revision: computed(() => projection.observed.value?.revision ?? view()?.object.revision ?? 0) };
}

export type TaskContent = Pick<TaskBoard.TaskFields, 'title' | 'description' | 'acceptanceCriteria' | 'category' | 'priority'>;
type ContentView = { object: TaskBoard.ObjectPin; task: { fields: TaskContent } };
const content = (fields: TaskContent): TaskContent => ({ title: fields.title, description: fields.description,
  acceptanceCriteria: fields.acceptanceCriteria, category: fields.category, priority: fields.priority });
const sameContent = (a: TaskContent, b: TaskContent) => a.title === b.title && a.description === b.description &&
  a.category === b.category && a.priority === b.priority && a.acceptanceCriteria.length === b.acceptanceCriteria.length &&
  a.acceptanceCriteria.every((value, index) => value === b.acceptanceCriteria[index]);

export function useContentProjection(workspace: () => TaskBoard.WorkspaceInfo, taskId: string,
  action: ProjectionAction, view: () => ContentView | null) {
  const projection = useProjection(workspace, taskId, action, view, value => content(value.task.fields), saved => {
    const request = saved.call.arguments as TaskBoard.EditRequest;
    return saved.call.qualifiedName === 'task-board.edit' && request.action === 'edit' && request.taskId === taskId && request.fields
      ? { value: content(request.fields), revision: request.expectedRevision } : null;
  }, sameContent);
  return { state: projection.value, begin: (fields: TaskContent) => projection.begin(content(fields)), observe: projection.observe, pending: projection.pending };
}

type ArchiveView = { object: TaskBoard.ObjectPin; archived?: boolean; observation?: Operation.ObjectObservation };
type ArchiveObservation = { archived: boolean; observation: Operation.ObjectObservation };
const validObservation = (value: Operation.ObjectObservation | undefined): value is Operation.ObjectObservation =>
  !!value && typeof value.runtimeEpoch === 'string' && !!value.runtimeEpoch && Number.isSafeInteger(value.sequence) && value.sequence >= 0;

/** Archive metadata changes without a new content revision. Keep one actual watermark
 * and one display-only intent alongside the existing original action journal. */
export function useArchiveProjection(workspace: () => TaskBoard.WorkspaceInfo, taskId: string,
  action: ProjectionAction, view: () => ArchiveView | null) {
  const identity = () => { const value = workspace(); return JSON.stringify([
    value.serviceNodeId, value.callerPrincipalId, value.principalId, value.rootObjectId,
  ]); };
  const owner = identity(), actual = shallowRef<ArchiveObservation | null>(null);
  const intent = shallowRef<{ archived: boolean; operationId?: string; previousOperationId?: string | undefined; base?: Operation.ObjectObservation } | null>(null);
  const belongs = () => identity() === owner && action.identityMatches.value && view()?.object.objectId === taskId;
  const accept = (archived: boolean, observation: Operation.ObjectObservation | undefined, receipt = false) => {
    if (!validObservation(observation)) return;
    const previous = actual.value?.observation;
    // Epochs are not comparable. A receipt from a different epoch cannot replace a current read.
    if (previous && (previous.runtimeEpoch === observation.runtimeEpoch ? previous.sequence > observation.sequence : receipt)) return;
    actual.value = { archived, observation: { ...observation } };
  };
  const observeAction = () => {
    if (!belongs()) return;
    const saved = action.saved.value, request = saved?.call.arguments as TaskBoard.ArchiveRequest | undefined;
    if (!saved || saved.call.qualifiedName !== 'task-board.archive' || request?.action !== 'archive' || request.taskId !== taskId ||
        saved.call.operationId !== request.operationId) return;
    const current = intent.value;
    if (current && !current.operationId && (current.previousOperationId === saved.call.operationId || current.archived !== request.archived)) return;
    if (current?.operationId && current.operationId !== saved.call.operationId) return;
    if (saved.phase === 'failed') { intent.value = null; return; }
    if (saved.phase === 'succeeded') {
      const metadata = saved.outcome?.archive;
      if (metadata?.id === taskId && metadata.effectivelyArchived === request.archived && (metadata.archivedAt !== null) === request.archived)
        accept(metadata.effectivelyArchived, metadata.observation, true);
      intent.value = null;
      return;
    }
    // Old retained calls without a read watermark stay recoverable without guessing an ordering.
    const base = saved.archiveBase;
    if (!current && (!validObservation(base) || actual.value?.observation.runtimeEpoch !== base.runtimeEpoch)) return;
    intent.value = { archived: request.archived, operationId: saved.call.operationId!, ...(base ? { base } : {}) };
  };
  const observe = (value: ArchiveView) => {
    if (!belongs() || value.object.objectId !== taskId) return;
    accept(value.archived ?? false, value.observation);
    observeAction();
  };
  watch([() => { const value = view(); return value ? [value.object.objectId, value.archived,
    value.observation?.runtimeEpoch, value.observation?.sequence] : null; }, identity],
    () => { const value = view(); if (value) observe(value); }, { immediate: true, flush: 'sync' });
  watch([() => action.saved.value?.call.operationId, () => action.saved.value?.phase, () => action.saved.value?.outcome, () => action.error.value],
    observeAction, { immediate: true, flush: 'sync' });
  const authoritative = computed(() => belongs() ? actual.value?.archived ?? view()?.archived ?? false : view()?.archived ?? false);
  const projecting = computed(() => {
    const current = intent.value;
    return belongs() && !!current && (!current.operationId || current.operationId === action.saved.value?.call.operationId) &&
      (!current.base || current.base.runtimeEpoch === actual.value?.observation.runtimeEpoch) &&
      !(action.saved.value?.phase === 'prepared' && action.error.value);
  });
  return {
    actual: authoritative,
    observation: computed(() => belongs() ? actual.value?.observation : undefined),
    observeReceipt: (metadata: Operation.ObjectMetadata) => {
      if (belongs() && metadata.id === taskId && metadata.contractKey === 'task-board/task' &&
          (metadata.archivedAt !== null) === metadata.effectivelyArchived)
        accept(metadata.effectivelyArchived, metadata.observation, true);
    },
    state: computed(() => projecting.value ? intent.value!.archived : authoritative.value),
    pending: projecting, observe,
    begin: (archived: boolean) => {
      const current = { archived, previousOperationId: action.saved.value?.call.operationId,
        ...(actual.value ? { base: actual.value.observation } : {}) };
      if (belongs()) intent.value = current;
      return { base: actual.value?.observation, cancel: () => { if (intent.value === current) intent.value = null; } };
    },
  };
}
