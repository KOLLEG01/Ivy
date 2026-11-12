import { ref, watch } from 'vue';
import { base, tr } from './runtime';
import type { TaskBoard } from './runtime';

/** Unsent composer content, separate from the original operation journal. */
export function useCommentDraft(workspace: () => TaskBoard.WorkspaceInfo, taskId: string) {
  const identity = () => JSON.stringify([base.href, workspace().serviceNodeId, workspace().callerPrincipalId,
    workspace().principalId, workspace().rootObjectId, taskId]);
  const owner = identity(), key = 'ivy:task-board:comment-draft:' + owner;
  const body = ref(''), attachments = ref<TaskBoard.TaskAttachment[]>([]), replyTo = ref<string | null>(null),
    moveToTodo = ref(false), issue = ref('');
  let unreadable = false;
  try {
    const saved = JSON.parse(sessionStorage.getItem(key) ?? 'null');
    if (saved) {
      if (typeof saved.body !== 'string' || !Array.isArray(saved.attachments) ||
          !saved.attachments.every((item: TaskBoard.TaskAttachment) => item && typeof item.attachmentId === 'string') ||
          !(saved.replyTo === null || typeof saved.replyTo === 'string') || typeof saved.moveToTodo !== 'boolean')
        throw new Error('Invalid comment draft');
      body.value = saved.body; attachments.value = saved.attachments;
      replyTo.value = saved.replyTo; moveToTodo.value = saved.moveToTodo;
    }
  } catch {
    unreadable = true;
    issue.value = tr('Der gesicherte Entwurf ist nicht lesbar. Neuer Text bleibt in dieser Ansicht erhalten.',
      'The saved draft cannot be read. New text stays in this view.');
  }
  watch([body, attachments, replyTo, moveToTodo], () => {
    if (unreadable || owner !== identity()) return;
    try {
      if (body.value || attachments.value.length || replyTo.value || moveToTodo.value)
        sessionStorage.setItem(key, JSON.stringify({ body: body.value, attachments: attachments.value,
          replyTo: replyTo.value, moveToTodo: moveToTodo.value }));
      else sessionStorage.removeItem(key);
      issue.value = '';
    } catch {
      issue.value = tr('Der Entwurf konnte nicht im Browser gesichert werden. Er bleibt in dieser Ansicht erhalten.',
        'The draft could not be saved in this browser. It stays in this view.');
    }
  }, { deep: true, flush: 'sync' });
  return { body, attachments, replyTo, moveToTodo, issue };
}
