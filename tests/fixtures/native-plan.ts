import { hashJson } from '../../packages/contracts/src/canonical.js';
import { checkedNativeContract } from '../../packages/contracts/src/checked-native-contract.js';
import type { Agent } from '../../packages/contracts/src/generated.js';

export function nativePlanDraft(version = '0.154.0', turnStart: Agent.PlanDraft['turnStart'] = {}): Agent.PlanDraft {
  const { contract, catalog } = checkedNativeContract(version);
  return { nativeVersion: version, catalogSourceHash: catalog.sourceHash, definitions: {
    threadStart: hashJson(contract.definition('thread/start')), threadResume: hashJson(contract.definition('thread/resume')),
    turnStart: hashJson(contract.definition('turn/start')), turnInterrupt: hashJson(contract.definition('turn/interrupt')) },
    threadStart: {}, threadResume: {}, turnStart };
}
/** Shape-only fixture for transport/outbox tests that never execute a native workflow. */
export function unpersistedNativePlan(version = '0.154.0'): Agent.Plan {
  const draft = nativePlanDraft(version), { catalog } = checkedNativeContract(version);
  const parameters = (method: string, reservedFields: string[], params: Agent.PlanDraft['threadStart']): Agent.ParametersRef => ({ schemaVersion: 1, nativeVersion: version,
    nativeExecutableHash: catalog.nativeExecutableHash, catalogHash: hashJson(catalog), method, reservedFields, params });
  return { ...draft, threadStart: parameters('thread/start', [], draft.threadStart), threadResume: parameters('thread/resume', ['history','path','threadId'], draft.threadResume),
    turnStart: parameters('turn/start', ['threadId'], draft.turnStart) };
}
