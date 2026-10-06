import { canonical, hashJson } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import type { Agent } from '../../contracts/src/generated.js';
import type { NativeContract } from '../../contracts/src/native-contract.js';
import type { RpcClient, RequestOptions } from './client.js';
import { saveNativeParameters, readNativeParameters } from './native-parameters.js';

export type NativePlanKind = 'task-board' | 'chat';
export type NativeInvocation = Agent.InvocationDraft;
const methods = { threadStart: 'thread/start', threadResume: 'thread/resume', turnStart: 'turn/start', turnInterrupt: 'turn/interrupt' } as const;
const keys = ['threadStart', 'threadResume', 'turnStart'] as const;
const reserved = (key: typeof keys[number], kind: NativePlanKind) => key === 'threadStart' ? [] : key === 'threadResume'
  ? ['threadId', 'history', 'path'] : kind === 'chat' ? ['threadId', 'input', 'clientUserMessageId'] : ['threadId'];

/** Raw invocations are transport/evidence bytes, never unvalidated domain state. */
export function validateNativeInvocation(contract: NativeContract, value: unknown, maximumBytes = 1048576): asserts value is NativeInvocation {
  validateAgent('InvocationDraft', value); const request = value as NativeInvocation;
  requireThat(request.nativeVersion === contract.catalog.version && request.catalogSourceHash === contract.catalog.sourceHash,
    'native_request_catalog_mismatch', 'Native invocation requires its exact retained catalog.');
  contract.validateInput(request.method, request.params, maximumBytes);
}

export function validateNativePlanIdentity(contract: NativeContract, plan: Pick<Agent.Plan, 'nativeVersion' | 'catalogSourceHash' | 'definitions'>): void {
  requireThat(plan.nativeVersion === contract.catalog.version && plan.catalogSourceHash === contract.catalog.sourceHash,
    'native_plan_catalog_mismatch', 'The plan must retain its selected native catalog source and version.');
  for (const [key, method] of Object.entries(methods)) requireThat(plan.definitions[key as keyof typeof methods] === contract.definitionHash(method),
    'native_plan_definition_mismatch', 'The plan must retain every original native method definition.');
}

/** Complete preflight happens before the first parameter write, including all reserved fields. */
export function validateNativePlanDraft(contract: NativeContract, draft: unknown, kind: NativePlanKind): asserts draft is Agent.PlanDraft {
  validateAgent('PlanDraft', draft); const value = draft as Agent.PlanDraft;
  canonical(value, 1048576); validateNativePlanIdentity(contract, value);
  if (value.location) for (const [index, params] of [value.threadStart, value.threadResume, value.turnStart].entries())
    requireThat((index !== 0 || params['cwd'] === value.location.cwd) && (params['cwd'] == null || params['cwd'] === value.location.cwd),
      'native_plan_location_mismatch', 'Native parameters must retain the concrete selected project cwd.');
  for (const key of keys) contract.validateTemplate(methods[key], value[key], reserved(key, kind));
}

export async function saveNativePlan(client: RpcClient, contract: NativeContract, input: {
  draft: Agent.PlanDraft; kind: NativePlanKind; parentId: string | null; mutationId: string;
}, options?: RequestOptions): Promise<Agent.Plan> {
  const draft = structuredClone(input.draft); validateNativePlanDraft(contract, draft, input.kind);
  const refs = {} as Pick<Agent.Plan, typeof keys[number]>;
  for (const key of keys) refs[key] = await saveNativeParameters(client, contract, { method: methods[key], params: draft[key],
    reserved: reserved(key, input.kind), parentId: input.parentId,
    mutationId: 'native-plan-' + hashJson({ identity: input.mutationId, key }).slice(7) }, options);
  const plan = { ...draft, ...refs }; validateAgent('Plan', plan); return plan;
}

/** Reconstruct only exact immutable revisions from the expected workflow root. */
export async function readNativePlan(client: RpcClient, contract: NativeContract, input: {
  plan: Agent.Plan; kind: NativePlanKind; parentId: string | null;
}, options?: RequestOptions): Promise<Agent.PlanDraft> {
  const plan = structuredClone(input.plan); validateAgent('Plan', plan); validateNativePlanIdentity(contract, plan);
  const params = {} as Pick<Agent.PlanDraft, typeof keys[number]>;
  const reads = await Promise.allSettled(keys.map(async key => readNativeParameters(client, contract, {
    method: methods[key], reserved: reserved(key, input.kind), parentId: input.parentId, ref: plan[key] }, options)));
  for (const [index, key] of keys.entries()) {
    const read = reads[index]!;
    if (read.status === 'rejected') throw read.reason;
    params[key] = read.value as Agent.PlanDraft[typeof key];
  }
  const draft = { ...plan, ...params }; validateNativePlanDraft(contract, draft, input.kind); return draft;
}
