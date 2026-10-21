import { openTaskQuestions } from './task-board-questions.js';
import type { TaskBoard } from './generated.js';

// Before task 1.5.0 and configuration 1.1.0 a requirement could name one capability instead of a host.
type StoredRequirement = TaskBoard.ExecutionRequirement | { kind: 'capability'; capabilityKey: string } | null;

/** A stored capability requirement now means automatic placement on a host that provides it. */
export function upgradeRequirement(requirement: StoredRequirement): TaskBoard.ExecutionRequirement | null {
  return requirement?.kind === 'capability' ? { kind: 'automatic' } : requirement;
}

/** Reads a Task revision in the current field model; revisions without a capability requirement are unchanged. */
type PriorCondition = { taskId: string; until: 'idle' | 'done' | 'delivery'; key?: string };
type PriorTask = TaskBoard.Task & {
  fields: TaskBoard.TaskFields & { dependencies?: string[]; nextReviewAt?: string | null };
  waiting?: { reason: string; detail: string; since: string } | null;
  blocker?: PriorCondition;
  questionWait?: { taskId: string; commentId: string };
  scheduledCheck?: { scope: string; failure?: { code: string; detail: string; at: string } };
  consumedBlockers?: { blocker: PriorCondition; fingerprint: string }[];
};
type PriorHandoff = { workflowState: 'review' | 'waiting' | 'todo'; nextReviewAt?: string | null;
  waiting?: { reason: string; detail: string; blocker?: PriorCondition }; checkScope?: string;
  nextStep?: string; deliveries?: TaskBoard.WorkDelivery[] };

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
    const text = findingsText(group.findings), fragments: string[] = [];
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

function importedConditions(task: PriorTask, blocker?: PriorCondition): TaskBoard.Condition[] {
  const conditions: TaskBoard.Condition[] = (task.fields.dependencies ?? []).map(taskId => ({ kind: 'task', taskId, until: 'done' }));
  if (blocker) {
    if (blocker.until === 'delivery') {
      const consumed = task.consumedBlockers?.find(value => value.blocker.taskId === blocker.taskId && value.blocker.key === blocker.key)?.fingerprint;
      conditions.push({ kind: 'delivery', taskId: blocker.taskId, key: blocker.key!, ...(consumed ? { after: consumed } : {}) });
    } else conditions.push({ kind: 'task', taskId: blocker.taskId, until: blocker.until });
  }
  return conditions;
}

function importHandoff(value: unknown, task: PriorTask, identity: string): TaskBoard.Handoff {
  const previous = value as PriorHandoff & Partial<TaskBoard.Handoff>;
  if (Array.isArray(previous.nextSteps)) return previous as TaskBoard.Handoff;
  const conditions = importedConditions(task, previous.waiting?.blocker);
  const questions = openTaskQuestions(task).map(question => ({ kind: 'user' as const, questionId: question.commentId }));
  const nextSteps: TaskBoard.NextStep[] = [];
  if (previous.nextReviewAt) nextSteps.push({ id: identity + '-appointment',
    scope: previous.checkScope ?? previous.waiting?.detail ?? 'Continue the authorized task at its appointment.',
    requires: [...conditions, { kind: 'time', at: previous.nextReviewAt }] });
  else if (previous.workflowState !== 'review' && !task.questionWait) nextSteps.push({ id: identity,
    scope: previous.nextStep ?? previous.waiting?.detail ?? 'Continue the authorized task described in this ticket.',
    requires: [...conditions, ...(previous.waiting?.reason === 'user' ? questions : [])] });
  return { workflowState: previous.workflowState, nextSteps, ...(previous.deliveries ? { deliveries: previous.deliveries } : {}) };
}

export function upgradeTaskRecord(value: TaskBoard.Task): TaskBoard.Task {
  const prior = preserveTaskFindings(value) as PriorTask;
  if (Array.isArray(prior.nextSteps)) return prior;
  const requirement = prior.fields.executionRequirement as StoredRequirement;
  const { dependencies: _dependencies, nextReviewAt: _appointment, ...fields } = prior.fields;
  const { waiting, blocker, questionWait, scheduledCheck, consumedBlockers: _consumed, ...base } = prior;
  const ownQuestions = openTaskQuestions(prior);
  const nextSteps: TaskBoard.NextStep[] = [];
  const conditions = importedConditions(prior, blocker);
  if (prior.fields.nextReviewAt) nextSteps.push({ id: 'imported-appointment',
    scope: scheduledCheck?.scope ?? waiting?.detail ?? 'Continue the authorized task at its appointment.',
    requires: [...conditions, { kind: 'time', at: prior.fields.nextReviewAt }] });
  if (!questionWait && !scheduledCheck && !['backlog', 'review', 'done', 'cancelled'].includes(prior.workflowState)) nextSteps.push({
    id: 'imported-work', scope: waiting?.detail ?? 'Continue the authorized task described in this ticket; identify and preserve any answer-dependent prerequisites.',
    requires: [...conditions, ...(waiting?.reason === 'user' ? ownQuestions.map(question => ({ kind: 'user' as const, questionId: question.commentId })) : [])],
  });
  const technical = waiting && ['host', 'capability', 'workspace', 'external_outcome', 'publication', 'workflow'].includes(waiting.reason)
    ? { reason: waiting.reason as TaskBoard.RuntimeWait['reason'], detail: waiting.detail, since: waiting.since,
      retryAt: ['host', 'capability', 'workspace'].includes(waiting.reason) ? new Date(Date.parse(waiting.since) + 30_000).toISOString() : null }
    : questionWait ? { reason: 'workflow' as const, detail: 'The former cross-ticket user wait needs an explicit task delivery or a genuine own question.', since: prior.updatedAt, retryAt: null } : null;
  return { ...base, fields: { ...fields, executionRequirement: upgradeRequirement(requirement),
      ...(requirement?.kind === 'capability' ? { requiredCapabilities: [...new Set([...(fields.requiredCapabilities ?? []), requirement.capabilityKey])] } : {}) },
    nextSteps, runtimeWait: technical, consumedSteps: [],
    comments: prior.comments.map(comment => comment.handoff ? { ...comment, handoff: importHandoff(comment.handoff, prior, 'imported-' + comment.sequence) } : comment),
    openQuestionCount: ownQuestions.length };
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
