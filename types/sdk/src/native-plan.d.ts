import type { Agent } from '../../contracts/src/generated.js';
import type { NativeContract } from '../../contracts/src/native-contract.js';
import type { RpcClient, RequestOptions } from './client.js';
export type NativePlanKind = 'symphony' | 'chat';
export type NativeInvocation = Agent.InvocationDraft;
/** Raw invocations are transport/evidence bytes, never unvalidated domain state. */
export declare function validateNativeInvocation(contract: NativeContract, value: unknown, maximumBytes?: number): asserts value is NativeInvocation;
export declare function validateNativePlanIdentity(contract: NativeContract, plan: Pick<Agent.Plan, 'nativeVersion' | 'catalogSourceHash' | 'definitions'>): void;
/** Complete preflight happens before the first parameter write, including all reserved fields. */
export declare function validateNativePlanDraft(contract: NativeContract, draft: unknown, kind: NativePlanKind): asserts draft is Agent.PlanDraft;
export declare function saveNativePlan(client: RpcClient, contract: NativeContract, input: {
    draft: Agent.PlanDraft;
    kind: NativePlanKind;
    parentId: string | null;
    mutationId: string;
}, options?: RequestOptions): Promise<Agent.Plan>;
/** Reconstruct only exact immutable revisions from the expected workflow root. */
export declare function readNativePlan(client: RpcClient, contract: NativeContract, input: {
    plan: Agent.Plan;
    kind: NativePlanKind;
    parentId: string | null;
}, options?: RequestOptions): Promise<Agent.PlanDraft>;
