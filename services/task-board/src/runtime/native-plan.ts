import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { checkedNativeContract } from '../../../../packages/sdk/src/node.js';
import { readNativePlan } from '../../../../packages/sdk/src/native-plan.js';
import type { TaskBoardStore } from './store.js';

export async function resolvedPlan(store: TaskBoardStore, plan: TaskBoard.NativePlan) {
  return readNativePlan(store.client, checkedNativeContract(plan.nativeVersion, { sourceHash: plan.catalogSourceHash }).contract,
    { plan, kind: 'task-board', parentId: store.rootObjectId });
}
export async function readPlan(store: TaskBoardStore, pin: TaskBoard.ObjectPin) {
  return resolvedPlan(store, (await store.read('task-board/native-plan', pin)).value);
}
