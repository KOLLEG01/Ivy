/** Keep schemas of already published ticket revisions byte-for-byte stable. */
export function freezePublishedWorkflowSchema(schema: Record<string, any>): void {
  for (const key of ['completionIssue', 'consumedBlockers', 'deliveries', 'questionWait']) delete schema.properties?.[key];
  const defs = schema.$defs ?? {};
  for (const key of ['deliveries', 'questionWait']) delete defs.TaskBoardHandoff?.properties?.[key];
  delete defs.TaskBoardDeferRequest?.properties?.questionWait;
  const blocker = defs.TaskBoardBlocker;
  if (blocker) {
    delete blocker.properties.key;
    delete blocker.allOf;
    blocker.properties.until.enum = blocker.properties.until.enum.filter((value: string) => value !== 'delivery');
  }
  const used = new Set<string>();
  const visit = (value: any): void => {
    if (!value || typeof value !== 'object') return;
    if (Array.isArray(value)) { value.forEach(visit); return; }
    for (const [key, child] of Object.entries(value)) {
      if (key === '$defs') continue;
      if (key === '$ref' && typeof child === 'string' && child.startsWith('#/$defs/')) {
        const name = child.slice('#/$defs/'.length);
        if (!used.has(name)) { used.add(name); visit(defs[name]); }
      } else visit(child);
    }
  };
  visit(schema);
  for (const name of ['BlockerObservation', 'QuestionWait', 'WorkDelivery', 'DeliveryReceipt', 'CompletionIssue', 'Artifact', 'CodeReference']) {
    const key = 'TaskBoard' + name;
    if (!used.has(key)) delete defs[key];
  }
}

/** Preserve published schemas when adding automatic completion recovery. */
export function freezeRecoverySchema(schema: Record<string, any>): void {
  if (!schema || typeof schema !== 'object') return;
  delete schema.properties?.continuation;
  const defs = schema.$defs ?? {};
  if (defs.TaskBoardWaiting?.properties?.reason?.enum)
    defs.TaskBoardWaiting.properties.reason.enum = defs.TaskBoardWaiting.properties.reason.enum.filter((value: string) => value !== 'workflow');
  delete defs.TaskBoardTask?.properties?.continuation;
  delete defs.TaskBoardContinueRequest?.properties?.recoveryFor;
  delete defs.TaskBoardDeferRequest?.properties?.recoveryRetryAt;
  const issue = defs.TaskBoardCompletionIssue;
  if (issue) { delete issue.properties.attempts; delete issue.properties.retryAt; }
  const handoff = defs.TaskBoardHandoff;
  if (handoff) {
    delete handoff.properties.nextStep;
    handoff.properties.workflowState.enum = handoff.properties.workflowState.enum.filter((value: string) => value !== 'todo');
    handoff.allOf = [
      { if: { properties: { workflowState: { const: 'waiting' } } }, then: { required: ['waiting'] } },
      { if: { properties: { workflowState: { const: 'review' } } }, then: { not: { required: ['waiting'] } } },
    ];
  }
}
