import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

/** A compact outcome is evidence for the original identity, never authority to replay it. */
export function operationReceipt(operation: TaskBoard.Operation): TaskBoard.OperationReceipt {
  return { receipt: true, operationId: operation.operationId, callerPrincipalId: operation.callerPrincipalId,
    requestHash: operation.requestHash, phase: operation.phase, action: operation.request.action,
    taskId: 'taskId' in operation.request ? operation.request.taskId : operation.outcome?.task?.objectId ?? null,
    updatedAt: operation.updatedAt, outcome: operation.outcome, error: operation.error,
    ...(operation.handoffRun ? { handoffRun: operation.handoffRun } : {}) };
}
