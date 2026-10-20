import { requireThat, type TaskBoard } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardStore } from './store.js';

/** Uncertain and input-waiting Runs still own execution capacity. */
export async function requireExecutionCapacity(store: TaskBoardStore, settings: TaskBoard.Settings): Promise<void> {
  const limit = settings.scheduler.maxConcurrentRuns ?? 10;
  let count = 0, cursor: string | undefined;
  do {
    const page = await store.page('task-board/run', { limit: 50, where: {
      op: 'in', field: 'data:/phase',
      value: ['starting', 'running', 'waiting_input', 'publishing_result', 'cancel_requested', 'outcome_unknown'],
    }, ...(cursor ? { cursor } : {}) });
    count += page.items.length;
    requireThat(count < limit, 'task_board_execution_capacity',
      `All ${limit} TaskBoard execution slots are occupied. Waiting for an existing Run to finish.`);
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
}
