import { hashJson } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardEngine } from './engine.js';
import { openQuestions } from './conversation.js';

/** The canonical question stays with its sender. The deterministic relay is only a wake-up message. */
export async function routeTaskQuestions(engine: TaskBoardEngine, taskId: string): Promise<void> {
  const source = await engine.store.read('task-board/task', taskId);
  if (source.value.publication) return;
  for (const question of openQuestions(source.value).filter(question => question.recipient?.kind === 'task').slice(0, 10)) {
    if (question.recipient?.kind !== 'task') continue;
    const target = await engine.store.read('task-board/task', question.recipient.taskId);
    const commentId = 'question:' + hashJson({ taskId, commentId: question.commentId }).slice(7);
    if (target.value.comments.some(comment => comment.commentId === commentId)) continue;
    await engine.nativeComment({ action: 'comment', operationId: commentId + ':' + target.pin.revision,
      taskId: target.pin.objectId, expectedRevision: target.pin.revision, commentId,
      authorKind: 'agent', sourceTaskId: taskId, questionSource: { taskId, commentId: question.commentId },
      purpose: 'update', agentDelivery: 'queue', replyTo: null, requests: [], responses: [],
      attachments: question.attachments,
      body: ('Frage von ' + source.value.taskKey + ' an dieses Ticket:\n\n' + question.body +
        '\n\nAntworte im Ursprungsticket ' + taskId + ' mit sourceTaskId=' + target.pin.objectId +
        ' und replyTo=' + question.commentId + '. Die Frage richtet sich an diesen Task; sie ist keine Nutzerfrage.').slice(0, 65536),
    });
  }
}
