import { canonical, hashJson } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import type { Agent, Wire } from '../../contracts/src/generated.js';
import type { NativeContract } from '../../contracts/src/native-contract.js';
import type { NativeTarget } from './native.js';

export type NativeOwnerIdentity = Omit<NativeTarget, 'hostId'> & { hostId?: string };
export interface NativeOperationIdentity extends NativeOwnerIdentity {
  callerPrincipalId: string; operationId: string; method: string; params: Wire.Json;
}
export interface NativeReadIdentity extends NativeOwnerIdentity {
  callerPrincipalId: string; epoch: string; method: string; params: Wire.Json;
}
export function nativeInstant(value: string, code = 'native_evidence_mismatch'): number {
  const at = Date.parse(value);
  requireThat(Number.isFinite(at) && new Date(at).toISOString() === value, code, 'Native evidence needs its original valid timestamp.');
  return at;
}
export function validateNativeStatus(value: unknown, target: NativeOwnerIdentity, code = 'native_owner_mismatch'): asserts value is Agent.Status & { epoch: string } {
  validateAgent('Status', value); const status = value as Agent.Status;
  requireThat(status.serviceNodeId === target.serviceNodeId && (target.hostId === undefined || status.hostId === target.hostId) &&
    status.state === 'ready' && status.epoch && status.nativeVersion === target.nativeVersion &&
    status.nativeExecutableHash === target.nativeExecutableHash && status.catalogHash === target.catalogHash,
    code, 'Native readiness requires the exact selected owner, executable and catalog.');
  nativeInstant(status.observedAt, code);
}
export function validateNativeOperation(value: unknown, expected: NativeOperationIdentity, code = 'native_evidence_mismatch'): asserts value is Agent.Operation {
  validateAgent('Operation', value); const observed = value as Agent.Operation;
  requireThat(observed.operationId === expected.operationId && observed.callerPrincipalId === expected.callerPrincipalId &&
    observed.serviceNodeId === expected.serviceNodeId && observed.nativeVersion === expected.nativeVersion &&
    observed.nativeExecutableHash === expected.nativeExecutableHash && observed.method === expected.method &&
    canonical(observed.params) === canonical(expected.params) && observed.requestHash === hashJson({ method: expected.method, params: expected.params }),
    code, 'Native evidence must identify the exact original caller, owner, executable, operation and request.');
  requireThat(nativeInstant(observed.updatedAt, code) >= nativeInstant(observed.createdAt, code), code, 'An original operation cannot finish before it was created.');
}
export function validateNativeRead(value: unknown, expected: NativeReadIdentity, code = 'native_evidence_mismatch'): asserts value is Agent.ReadObservation {
  validateAgent('ReadObservation', value); const observed = value as Agent.ReadObservation;
  requireThat(observed.serviceNodeId === expected.serviceNodeId && observed.callerPrincipalId === expected.callerPrincipalId &&
    observed.nativeVersion === expected.nativeVersion && observed.nativeExecutableHash === expected.nativeExecutableHash &&
    observed.catalogHash === expected.catalogHash && observed.epoch === expected.epoch && observed.method === expected.method &&
    canonical(observed.params) === canonical(expected.params) && observed.requestHash === hashJson({ method: expected.method, params: expected.params }),
    code, 'Native reads must identify the exact original owner, caller, catalog, epoch and request.');
  nativeInstant(observed.observedAt, code);
}
/** These are timestamps from the same operation owner, never the caller host's wall clock. */
export function validateNativeProgress(previous: Agent.Operation, next: Agent.Operation, code = 'native_evidence_regressed'): void {
  validateAgent('Operation', previous); validateAgent('Operation', next);
  const identity = (value: Agent.Operation) => ({ operationId: value.operationId, callerPrincipalId: value.callerPrincipalId,
    serviceNodeId: value.serviceNodeId, nativeVersion: value.nativeVersion, nativeExecutableHash: value.nativeExecutableHash,
    method: value.method, params: value.params, requestHash: value.requestHash, createdAt: value.createdAt });
  const rank = { accepted: 0, dispatched: 1, outcome_unknown: 2, succeeded: 3, failed: 3 };
  requireThat(canonical(identity(previous)) === canonical(identity(next)) &&
    nativeInstant(next.updatedAt, code) >= nativeInstant(previous.updatedAt, code) && rank[next.phase] >= rank[previous.phase] &&
    (previous.epoch === null || previous.epoch === next.epoch) && (previous.requestId === null || previous.requestId === next.requestId) &&
    (!nativeOperationTerminal(previous) || canonical(previous) === canonical(next)),
    code, 'The original operation cannot change identity, regress or replace its terminal receipt.');
}
export function nativeOperationTerminal(value: Agent.Operation): boolean { return value.phase === 'succeeded' || value.phase === 'failed'; }
export function validateNativePayload(contract: NativeContract, observation: Agent.Operation | Agent.ReadObservation,
  limits: { requestBytes: number; responseBytes: number }): void {
  canonical({ id: observation.requestId ?? '00000000-0000-0000-0000-000000000000', method: observation.method, params: observation.params }, limits.requestBytes);
  contract.validateInput(observation.method, observation.params, limits.requestBytes);
  if (observation.reply) {
    canonical({ id: observation.requestId, ...observation.reply }, limits.responseBytes);
    if ('result' in observation.reply) contract.validateResult(observation.method, observation.reply.result, limits.responseBytes);
  }
}
