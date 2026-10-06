import { checkBlocker } from './task-coordination.js';
import { canonical, digest, hashJson, jsonObjectContentBytes } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard } from '../../../../packages/sdk/src/node.js';
import { nativeServiceTools } from '../../../../packages/sdk/src/client.js';
import { hostProject } from '../../../../packages/sdk/src/project-inventory.js';
import { TaskBoardStore } from './store.js';
import type { Document } from './store.js';
import { readPlan } from './native-plan.js';
import { checkConfiguration, configurationView, effectiveExecution, executionWithDefaults } from './configuration.js';
import { freshContextRecovery } from './native-recovery.js';

export const same = (a: unknown, b: unknown) => a === undefined || b === undefined
  ? a === b : canonical(a) === canonical(b);
export const reservesWorkspace = (requirement: Agent.WorkspaceRequirement) => requirement.kind === 'existing_project' && requirement.useWorktree;
export const sharedProjectKey = (requirement: TaskBoard.WorkspaceRequirement, workspace: TaskBoard.WorkspaceResolution): string | null =>
  requirement.kind === 'existing_project' && !requirement.useWorktree ? `project:${requirement.projectId}`
    : requirement.kind === 'directory_path' ? `directory:${workspace.canonicalCwd}` : null;

export async function sharedProjectOwner(store: TaskBoardStore, task: TaskBoard.Task, taskId: string,
  target: TaskBoard.Target, workspace: TaskBoard.WorkspaceResolution): Promise<string | null> {
  const key = sharedProjectKey(task.fields.workspaceRequirement, workspace);
  if (!key) return null;
  const reserved = await store.workspaceOwner(target.hostId, key, taskId);
  if (reserved) return reserved;
  let cursor: string | undefined;
  do {
    const page = await store.page('task-board/task', { limit: 100,
      where: { op: 'in', field: 'data:/workflowState', value: ['in_progress', 'waiting'] },
      ...(cursor ? { cursor } : {}) });
    for (const item of page.items) {
      if (item.objectId === taskId) continue;
      const peer = (await store.read('task-board/task', item.objectId)).value;
      if (task.fields.allowParallel && peer.fields.allowParallel) continue;
      if (peer.workflowState === 'waiting' && (peer.waiting?.reason === 'workspace' || !peer.lastRun)) continue;
      const run = peer.claim ? null : peer.lastRun ? (await store.read('task-board/run', peer.lastRun)).value : null;
      if (!peer.claim && (!run || ['completed', 'failed', 'cancelled'].includes(run.phase))) continue;
      const peerTarget = peer.claim?.target ?? run?.target;
      const peerWorkspace = peer.claim?.workspace ?? run?.workspace;
      if (peerTarget?.hostId !== target.hostId || !peerWorkspace) continue;
      if (sharedProjectKey(peer.fields.workspaceRequirement, peerWorkspace) === key) return item.objectId;
    }
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  return null;
}
export function resultCommentFits(task: TaskBoard.Task, comment: TaskBoard.Comment, publication: TaskBoard.ObjectPin): boolean {
  if (task.comments.length >= 512) return false;
  try {
    canonical({ ...task, publication: null, lastHistory: publication,
      comments: [...task.comments, { ...comment, delivery: task.fields.userContact !== 'ticket' ? publication : null }],
      agentCommentCount: task.agentCommentCount + 1 }, jsonObjectContentBytes - 1024);
    return true;
  } catch (error) {
    if (!(error instanceof IvyError && error.code === 'content_too_large')) throw error;
    return false;
  }
}
export function date(value: string | null): void {
  if (value === null) return;
  const parsed = new Date(value);
  requireThat(Number.isFinite(parsed.valueOf()) && parsed.toISOString() === value, 'task_board_invalid_date', 'Workflow dates must be real, canonical UTC timestamps.');
}
export function planSemantics(plan: Agent.PlanDraft): void {
  requireThat(plan.threadStart.ephemeral !== true, 'task_board_ephemeral_context', 'A durable workflow cannot use an ephemeral native context.');
  for (const params of [plan.threadStart, plan.threadResume, plan.turnStart]) {
    const value = params as Record<string, unknown>;
    requireThat(!(value['permissions'] != null && (value['sandbox'] != null || value['sandboxPolicy'] != null)), 'task_board_permission_conflict', 'A native plan cannot mix permissions with legacy sandbox fields.');
  }
}

export class TaskBoardChecks {
  constructor(readonly store: TaskBoardStore) {}
  async aggregate(task: TaskBoard.Task, taskId: string): Promise<void> {
    if (task.claim) {
      const claim = task.claim;
      requireThat(claim.request.taskId === taskId && claim.operationId === claim.request.operationId && hashJson(claim.request) === claim.requestHash && claim.attempt === task.attemptCount && claim.run && same(claim.run, task.lastRun),
        'task_board_claim_mismatch', 'The published Task claim must retain its exact request, target, attempt and current Run.');
    }
    if (task.lastRun) {
      const run = (await this.store.read('task-board/run', task.lastRun)).value;
      requireThat(run.taskId === taskId && run.originalRequest.taskId === taskId && run.operationId === run.originalRequest.operationId && run.requestHash === hashJson(run.originalRequest) && run.attempt <= task.attemptCount,
        'task_board_run_mismatch', 'The retained Run must belong to this task and its original execution intent.');
      if (task.claim) requireThat(run.attempt === task.claim.attempt && run.operationId === task.claim.operationId && run.callerPrincipalId === task.claim.callerPrincipalId && same(run.owner, task.claim.owner) && same(run.target, task.claim.target),
        'task_board_claim_mismatch', 'The current Run must have the original claimant identity and owner.');
    }
    if (task.latestResult) {
      const result = (await this.store.read('task-board/result', task.latestResult)).value;
      requireThat(result.taskId === taskId && (!['review', 'done'].includes(task.workflowState) || result.kind === 'final'), 'task_board_result_mismatch', 'Review and done tasks must retain their own final Result.');
    }
    if (task.acceptedReview) {
      const review = (await this.store.read('task-board/review', task.acceptedReview)).value;
      requireThat(review.taskId === taskId && (task.workflowState !== 'done' || same(review.result, task.latestResult)), 'task_board_review_mismatch', 'Task acceptance must retain its own exact Result.');
    }
    if (task.lastHistory) requireThat((await this.store.read('task-board/history', task.lastHistory)).value.taskId === taskId, 'task_board_history_mismatch', 'The latest history must belong to this task.');
  }
  async artifact(artifact: TaskBoard.Artifact): Promise<void> {
    const local = this.store.localArtifact(artifact.object);
    if (local) {
      requireThat(local.contentHash === artifact.contentHash && local.mediaType === artifact.mediaType,
        'task_board_evidence_mismatch', 'A local workflow artifact must match its exact revision, hash and media type.');
      return;
    }
    const value = await this.store.client.request('objects.read', artifact.object);
    const bytes = value.content.encoding === 'json' ? Buffer.from(canonical(value.content.value)) : Buffer.from(value.content.value, value.content.encoding === 'base64' ? 'base64' : 'utf8');
    requireThat(!value.object.effectivelyArchived && value.revision.revision === artifact.object.revision && value.revision.contentHash === artifact.contentHash && value.revision.mediaType === artifact.mediaType,
      'task_board_evidence_mismatch', 'An artifact must pin an available exact revision, hash and media type.');
    requireThat(bytes.length === value.revision.byteLength && digest(bytes) === artifact.contentHash, 'task_board_evidence_mismatch', 'The returned artifact bytes must match the retained hash and length.');
  }
  async content(content: TaskBoard.ResultContent): Promise<void> {
    for (const artifact of [...content.artifacts, ...content.checks.flatMap(check => check.evidence)]) await this.artifact(artifact);
    for (const ref of content.codeReferences) {
      let url: URL;
      try { url = new URL(ref.url); } catch { throw new IvyError('task_board_invalid_reference', 'A code reference URL must have valid HTTP(S) syntax.'); }
      requireThat(['http:', 'https:'].includes(url.protocol) && !!url.hostname && !url.username && !url.password, 'task_board_invalid_reference', 'Code references must be HTTP(S) URLs without embedded credentials.');
    }
  }
  async dependencies(fields: TaskBoard.TaskFields, self: string | null, satisfied = false): Promise<void> {
    // The publication gate covers this whole traversal and the eventual graph edit. Iterative DFS
    // avoids stack exhaustion and rejects cycles already present in referenced retained records.
    const done = new Set<string>(), visiting = new Set<string>(), tasks = new Map<string, Document<'task-board/task'>>(), pending = fields.dependencies.map(id => ({ id, exit: false, direct: true }));
    if (satisfied) for (const id of fields.dependencies) {
      const task = await this.store.read('task-board/task', id); tasks.set(id, task);
      requireThat(task.value.workflowState === 'done' && !task.value.publication, 'task_board_dependency_waiting', 'Every direct dependency must have an accepted result before starting.');
    }
    while (pending.length) {
      const entry = pending.pop()!;
      if (entry.exit) { visiting.delete(entry.id); done.add(entry.id); continue; }
      requireThat(entry.id !== self && !visiting.has(entry.id), 'task_board_dependency_cycle', 'The dependency graph would contain a cycle.');
      if (done.has(entry.id)) continue;
      requireThat(done.size + visiting.size < 10000, 'task_board_dependency_limit', 'Dependency verification exceeds the 10,000-task complete traversal limit.');
      const task = tasks.get(entry.id) ?? await this.store.read('task-board/task', entry.id); tasks.set(entry.id, task);
      requireThat(!task.value.publication, 'task_board_publication_pending', 'A dependency still has an unfinished publication.');
      visiting.add(entry.id); pending.push({ ...entry, exit: true });
      for (const id of task.value.fields.dependencies) pending.push({ id, exit: false, direct: false });
      if (task.value.blocker) pending.push({ id: task.value.blocker.taskId, exit: false, direct: false });
    }
  }
  async fields(fields: TaskBoard.TaskFields, self: string | null): Promise<void> {
    date(fields.nextReviewAt); date(fields.dueAt); await this.dependencies(fields, self);
    requireThat(fields.category === null || fields.category.trim() === fields.category && fields.category.length > 0,
      'invalid_arguments', 'Task category must be normalized nonempty text.');
    if (fields.workspaceRequirement.kind === 'repository_path') {
      const url = new URL(fields.workspaceRequirement.repositoryUrl);
      requireThat(!url.username && !url.password, 'invalid_arguments', 'Repository URLs cannot contain embedded credentials.');
    }
    if (fields.workspaceRequirement.kind === 'existing_project') {
      const requirement = fields.executionRequirement ?? (await configurationView(this.store)).configuration.defaults.executionRequirement;
      const hostId = requirement?.kind === 'host' ? requirement.hostId : undefined;
      requireThat(await hostProject(this.store.client, hostId, fields.workspaceRequirement.projectId),
        'task_board_project_unavailable', `The selected project is not registered${hostId ? ' on ' + hostId : ''}. Choose a project from the execution host's catalog.`);
    }
  }
  async executionOptions(fields: TaskBoard.TaskFields, task?: TaskBoard.Task): Promise<void> {
    const options = fields.nativeOptions;
    if (fields.control !== 'agent' && !options?.model && !options?.reasoningEffort &&
      (!options?.serviceTier || options.serviceTier === 'standard')) return;
    const defaults = task
      ? await effectiveExecution(this.store, { ...task, fields })
      : executionWithDefaults(fields, (await configurationView(this.store)).configuration.defaults);
    const serviceNodeId = task?.primaryResourceRef?.serviceNodeId ??
      (task ? (await freshContextRecovery(this.store, task))?.target.serviceNodeId : undefined);
    await checkConfiguration(this.store, { schemaVersion: 1, defaults }, serviceNodeId);
  }
  async execution(task: TaskBoard.Task, intent: TaskBoard.ObjectPin, taskId: string, now: string, target?: TaskBoard.Target, workspace?: TaskBoard.WorkspaceResolution, coordinationOnly = false): Promise<Agent.PlanDraft> {
    requireThat(target && workspace && target.hostId === workspace.hostId && target.serviceNodeId === workspace.serviceNodeId,
      'task_board_execution_plan_required', 'Start must retain one resolved native owner and workspace.');
    const recovery = await freshContextRecovery(this.store, task);
    if (recovery) requireThat(same(target, recovery.target) && workspace.canonicalCwd === recovery.workspace.canonicalCwd && workspace.intendedPath === recovery.workspace.intendedPath,
      'task_board_workspace_mismatch', 'Fresh recovery must retain the original owner and workspace.');
    if (!coordinationOnly) {
      requireThat(!task.fields.nextReviewAt || task.fields.nextReviewAt <= now, 'task_board_time_waiting', 'The task is deferred until its next review time.');
      await this.dependencies(task.fields, taskId, true);
      await checkBlocker(this.store, task.blocker);
    }
    const plan = await readPlan(this.store, intent); planSemantics(plan);
    if (plan.location) {
      requireThat(plan.location.serviceNodeId === target.serviceNodeId && plan.location.hostId === target.hostId && plan.location.cwd === workspace.canonicalCwd,
        'task_board_project_mismatch', 'The saved project location belongs to another target host.');
    }
    const node = await this.store.client.request('serviceNodes.get', { serviceNodeId: target.serviceNodeId });
    requireThat(node.hostId === target.hostId && node.serviceName === 'agent-manager' && node.ready && node.connected && node.nativeVersion === plan.nativeVersion,
      'task_board_target_unavailable', 'The selected native owner is not ready at the exact plan version.');
    for (const [method, key] of [['thread/start', 'threadStart'], ['thread/resume', 'threadResume'], ['turn/start', 'turnStart'], ['turn/interrupt', 'turnInterrupt']] as const) {
      const tool = await nativeServiceTools(this.store.client, node.serviceNodeId).binding('codex.' + method);
      requireThat(tool.definitionHash === plan.definitions[key] && tool.definition.interfaceVersion === plan.nativeVersion && tool.definition.nativeMethod === method,
        'tool_definition_changed', 'The selected native definition differs from the saved plan. Save a new explicit plan before starting.');
    }
    requireThat(plan.location && plan.location.cwd === workspace.canonicalCwd,
      'task_board_project_mismatch', 'The plan must start in the resolved working directory.');
    if (task.primaryResourceRef) requireThat(task.primaryResourceRef.serviceNodeId === target.serviceNodeId && task.primaryResourceRef.namespace === 'codex' && task.primaryResourceRef.kind === 'thread', 'task_board_primary_mismatch', 'The primary context must remain on its original selected native owner.');
    return plan;
  }
}
