import { hashJson } from '../../../../packages/sdk/src/node.js';
import { serviceTools } from '../../../../packages/sdk/src/client.js';
import type { Agent } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardEngine } from './engine.js';

/** Retain native requests and answers in the ticket, bound to their original identity. */
export async function syncNativeComments(engine: TaskBoardEngine, runId: string): Promise<void> {
  const run = await engine.store.read('task-board/run', runId);
  if (!run.value.primaryResourceRef || !run.value.turnId) return;
  const current = await engine.store.read('task-board/task', run.value.taskId);
  if (current.value.claim?.run?.objectId !== runId) return;
  const page = await serviceTools(engine.store.client, run.value.target.serviceNodeId,
    [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).read('agent.inputs', { includeExpired: true, limit: 128 }) as Agent.PendingInputPage;
  for (const input of page.items) {
    if (input.identity.serviceNodeId !== run.value.target.serviceNodeId || input.threadId !== run.value.primaryResourceRef.nativeId || input.turnId && input.turnId !== run.value.turnId || input.identity.epoch !== run.value.nativeEpoch) continue;
    const identity = hashJson(input.identity).slice(7), questionId = 'native-question-' + identity;
    const secret = JSON.stringify(input.params).includes('"isSecret":true');
    // Secret answers remain exclusively at the native owner, never in the ticket or contact route.
    const retained = { ...input, reply: secret ? null : input.reply };
    const reply = input.reply && 'result' in input.reply ? input.reply.result as Record<string, unknown> : null;
    const answers = reply?.answers && typeof reply.answers === 'object' ? Object.values(reply.answers) as Array<{ answers?: string[] }> : [];
    const answerText = answers.flatMap(answer => answer.answers ?? []).join('\n\n') || (typeof reply?.decision === 'string' ? reply.decision : 'Response recorded.');
    const params = input.params as Record<string, unknown>;
    const questions = Array.isArray(params?.questions) ? params.questions as Array<{ question?: string }> : [];
    const body = questions.map(question => question.question).filter(Boolean).join('\n\n') ||
      (typeof params?.reason === 'string' ? params.reason : 'The agent needs your approval to continue.');
    let task = await engine.store.read('task-board/task', run.value.taskId);
    if (!task.value.comments.some(comment => comment.commentId === questionId)) {
      await engine.nativeComment({ action: 'comment', taskId: run.value.taskId, expectedRevision: task.pin.revision,
        operationId: questionId + '-' + task.pin.revision, commentId: questionId, purpose: 'question',
        body, nativeInput: { ...retained, state: 'pending', reply: null, answerOperationId: null, answerCallerPrincipalId: null, code: null }, contactUser: input.state === 'pending',
        requests: [], responses: [], attachments: [], replyTo: null, agentDelivery: 'none' });
    }
    if (!['answered', 'expired'].includes(input.state)) continue;
    task = await engine.store.read('task-board/task', run.value.taskId);
    const commentId = 'native-response-' + identity;
    if (task.value.comments.some(comment => comment.commentId === commentId)) continue;
    await engine.nativeComment({ action: 'comment', taskId: run.value.taskId, expectedRevision: task.pin.revision,
      operationId: commentId + '-' + task.pin.revision, commentId, purpose: 'update', nativeInput: retained,
      body: input.state === 'expired' ? 'This request has expired.' : secret ? 'A confidential response was provided.' :
        answerText.slice(0, 16000),
      requests: [], responses: [], attachments: [], replyTo: questionId, agentDelivery: 'none' });
  }
}
