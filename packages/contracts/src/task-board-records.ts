import { openTaskQuestions } from './task-board-questions.js';
import type { TaskBoard } from './generated.js';

// Before task 1.5.0 and configuration 1.1.0 a requirement could name one capability instead of a host.
type StoredRequirement = TaskBoard.ExecutionRequirement | { kind: 'capability'; capabilityKey: string } | null;

/** A stored capability requirement now means automatic placement on a host that provides it. */
export function upgradeRequirement(requirement: StoredRequirement): TaskBoard.ExecutionRequirement | null {
  return requirement?.kind === 'capability' ? { kind: 'automatic' } : requirement;
}

type StoredCondition = { kind: string; taskId?: string; at?: string };
type StoredStep = { id: string; scope: string; requires: StoredCondition[] };
type StoredSchedule = { nextSteps?: StoredStep[]; nextReviewAt?: string | null;
  waiting?: { reason: string; detail: string; since: string; blocker?: { taskId: string } } | null;
  deliveries?: unknown[]; nextStep?: string; checkScope?: string };

function schedule(value: StoredSchedule): { wakeAt: string | null; dependencies: string[] } {
  const conditions = value.nextSteps?.flatMap(step => step.requires) ?? [];
  return { wakeAt: conditions.flatMap(c => c.kind === 'time' && c.at ? [c.at] : []).sort()[0] ?? value.nextReviewAt ?? null,
    dependencies: [...new Set([...conditions.flatMap(c => c.taskId ? [c.taskId] : []),
      ...(value.waiting?.blocker ? [value.waiting.blocker.taskId] : [])])] };
}

/** Historical structured planning becomes ordinary context; it never gates execution. */
export function upgradeCommentRecord(comment: TaskBoard.Comment): TaskBoard.Comment {
  if (!comment.handoff) return comment;
  const handoff = comment.handoff as TaskBoard.Handoff & StoredSchedule;
  if (!('nextSteps' in handoff || 'nextReviewAt' in handoff || 'waiting' in handoff)) return comment;
  return { ...comment, handoff: { workflowState: handoff.workflowState,
    ...schedule(handoff), ...(handoff.wakeAt !== undefined ? { wakeAt: handoff.wakeAt } : {}),
    ...(handoff.dependencies ? { dependencies: handoff.dependencies } : {}) } };
}

type StoredFinding = { evidence?: TaskBoard.Artifact[]; [key: string]: unknown };

function findingsText(findings: StoredFinding[]): string {
  const text = JSON.stringify(findings, null, 2);
  const fence = '`'.repeat(Math.max(3, ...[...text.matchAll(/`+/g)].map(match => match[0].length + 1)));
  return '## Preserved verification findings\n\n' + fence + 'json\n' + text + '\n' + fence;
}

function findingsEvidence(findings: StoredFinding[]): TaskBoard.Artifact[] {
  return [...new Map(findings.flatMap(finding => finding.evidence ?? []).map(artifact => [JSON.stringify(artifact), artifact])).values()];
}

function preserveTaskFindings(task: TaskBoard.Task): TaskBoard.Task {
  const { latestChecks, ...current } = task as TaskBoard.Task & { latestChecks?: StoredFinding[] };
  const groups: { source: string; findings: StoredFinding[] }[] = [];
  if (latestChecks?.length) groups.push({ source: 'task', findings: latestChecks });
  const deliveries = (task as TaskBoard.Task & { deliveries?: { delivery: { artifacts: TaskBoard.Artifact[]; codeReferences: TaskBoard.CodeReference[] }; result: TaskBoard.ObjectPin }[] }).deliveries;
  if (deliveries?.length) groups.push({ source: 'saved-material', findings: deliveries.map(receipt => ({
    result: receipt.result, codeReferences: receipt.delivery.codeReferences, evidence: receipt.delivery.artifacts,
  })) });
  const comments = current.comments.map(comment => {
    if (!comment.handoff) return comment;
    const { checks, ...handoff } = comment.handoff as TaskBoard.Handoff & { checks?: StoredFinding[] };
    if (checks?.length) groups.push({ source: 'comment-' + comment.sequence, findings: checks });
    return checks ? { ...comment, handoff } : comment;
  });
  const author: TaskBoard.Actor = { principalId: 'task-board-record-upgrade',
    serviceNodeId: comments.at(-1)?.author.serviceNodeId ?? task.claim?.owner.serviceNodeId ?? 'task-board',
    generation: comments.at(-1)?.author.generation ?? 1, source: 'recovery' };
  let sequence = Math.max(0, ...comments.map(comment => comment.sequence));
  for (const group of groups) {
    const text = group.source === 'saved-material'
      ? 'Imported references for ' + group.findings.length + ' saved results. Details remain with those result records and their attachments.'
      : findingsText(group.findings), fragments: string[] = [];
    for (let offset = 0; offset < text.length;) {
      let end = Math.min(offset + 48000, text.length);
      if (end < text.length && /[\uD800-\uDBFF]/.test(text[end - 1]!)) end--;
      fragments.push(text.slice(offset, end)); offset = end;
    }
    const attachments = findingsEvidence(group.findings).filter(artifact => artifact.byteLength !== undefined)
      .map(artifact => ({ attachmentId: artifact.object.objectId + '-' + artifact.object.revision,
        object: artifact.object, filename: artifact.label, mediaType: artifact.mediaType, byteLength: artifact.byteLength!,
        contentHash: artifact.contentHash, uploader: author, createdAt: task.updatedAt }));
    for (let index = 0; index < Math.max(fragments.length, Math.ceil(attachments.length / 32)); index++) {
      const commentId = 'retained-findings-' + group.source + '-' + task.updatedAt + '-' + index;
      const body = 'Preserved from ' + group.source + '; existing work and questions are unchanged.\n\n' +
        (fragments[index] ?? 'Additional retained verification evidence.');
      if (comments.some(comment => comment.commentId === commentId)) continue;
      comments.push({ commentId, sequence: ++sequence, author, authorKind: 'system', purpose: 'update', body,
        requests: [], responses: [], attachments: attachments.slice(index * 32, (index + 1) * 32),
        run: null, turnId: null, replyTo: null, delivery: null, createdAt: task.updatedAt, operationId: commentId });
    }
  }
  return { ...current, comments };
}

export function upgradeTaskRecord(value: TaskBoard.Task): TaskBoard.Task {
  const prior = preserveTaskFindings(value) as TaskBoard.Task & StoredSchedule & {
    fields: TaskBoard.TaskFields & { dependencies?: string[]; nextReviewAt?: string | null };
    blocker?: { taskId: string }; questionWait?: unknown; scheduledCheck?: unknown;
    consumedBlockers?: unknown; consumedSteps?: unknown; continuation?: unknown; commentStore?: unknown;
  };
  const { nextSteps, consumedSteps, deliveries, waiting, blocker, questionWait, scheduledCheck,
    consumedBlockers, continuation, ...base } = prior;
  const { dependencies: fieldDependencies, nextReviewAt, ...fields } = prior.fields;
  const requirement = fields.executionRequirement as StoredRequirement;
  const imported = schedule({ ...(nextSteps ? { nextSteps } : {}), nextReviewAt: nextReviewAt ?? null,
    ...(waiting ? { waiting } : {}) });
  const comments = prior.comments.map(upgradeCommentRecord);
  if (!prior.commentStore && nextSteps?.length) {
    const context = 'Previous scheduling notes (context only; decide the work from the ticket):\n\n' +
      nextSteps.map(step => step.scope + (step.requires.length ? '\n' + JSON.stringify(step.requires) : '')).join('\n\n');
    for (let offset = 0; offset < context.length; offset += 48000) {
      const commentId = 'schedule-context-' + prior.updatedAt + '-' + offset;
      if (comments.some(comment => comment.commentId === commentId)) continue;
      comments.push({ commentId, sequence: comments.length + 1,
        author: { principalId: 'task-board-record-upgrade', serviceNodeId: 'task-board', generation: 1, source: 'recovery' },
        authorKind: 'system', purpose: 'update', body: context.slice(offset, offset + 48000),
        requests: [], responses: [], attachments: [], run: null, turnId: null, replyTo: null,
        delivery: null, createdAt: prior.updatedAt, operationId: commentId });
    }
  }
  const obsoleteIssue = prior.completionIssue?.code.startsWith('task_board_handoff_') ||
    prior.completionIssue?.code.startsWith('task_board_recovery_');
  const task: TaskBoard.Task = { ...base,
    fields: { ...fields, executionRequirement: upgradeRequirement(requirement),
      ...(requirement?.kind === 'capability' ? { requiredCapabilities: [...new Set([...(fields.requiredCapabilities ?? []), requirement.capabilityKey])] } : {}) },
    wakeAt: prior.wakeAt === undefined ? imported.wakeAt : prior.wakeAt,
    dependencies: prior.dependencies ?? [...new Set([...imported.dependencies, ...fieldDependencies ?? [], ...(blocker ? [blocker.taskId] : [])])],
    runtimeWait: prior.runtimeWait && (prior.runtimeWait.reason as string) !== 'workflow' ? prior.runtimeWait : null,
    comments };
  if (obsoleteIssue) { delete task.completionIssue; if (!task.claim && task.workflowState === 'waiting') task.workflowState = 'todo'; }
  if (task.completionIssue) {
    const { attempts: _attempts, ...issue } = task.completionIssue as TaskBoard.CompletionIssue & { attempts?: number };
    task.completionIssue = issue;
  }
  if (task.workflowState === 'in_progress' && !task.claim && task.fields.control === 'agent') task.workflowState = 'todo';
  return task;
}

export function upgradeRunRecord(run: TaskBoard.Run): TaskBoard.Run {
  if (!run.executionContext) return run;
  const { workflowState, latestResult, acceptedReview } = run.executionContext;
  return { ...run, executionContext: { workflowState, latestResult, acceptedReview } };
}

export function upgradeConfigurationRecord(configuration: TaskBoard.Configuration): TaskBoard.Configuration {
  const requirement = configuration.defaults.executionRequirement as StoredRequirement;
  return requirement?.kind === 'capability'
    ? { ...configuration, defaults: { ...configuration.defaults, executionRequirement: upgradeRequirement(requirement) } }
    : configuration;
}

export function upgradeResultContent(content: TaskBoard.ResultContent): TaskBoard.ResultContent {
  const { checks, ...current } = content as TaskBoard.ResultContent & { checks?: StoredFinding[] };
  if (!checks?.length) return current;
  return { ...current, summary: current.summary + '\n\n' + findingsText(checks),
    artifacts: [...new Map([...current.artifacts, ...findingsEvidence(checks)].map(artifact => [JSON.stringify(artifact), artifact])).values()] };
}

export function upgradeResultRecord(result: TaskBoard.Result): TaskBoard.Result {
  return { ...result, content: upgradeResultContent(result.content) };
}
