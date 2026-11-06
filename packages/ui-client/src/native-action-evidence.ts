import { IvyError } from '../../sdk/src/client.js';
import type { Agent } from '../../sdk/src/client.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { sameNativeBackend } from '../../contracts/src/native-backend.js';
import { hashJson } from './hash.js';

/** Original receipt identity; the request body is already retained in the exact call. */
export interface NativeActionIdentity {
  backend: Agent.BackendIdentity;
  serviceNodeId: string;
  nativeVersion: string;
  nativeExecutableHash: string;
  catalogHash: string;
  method: string;
  requestHash: string;
}
export interface NativeActionCheckpoint {
  phase: Agent.Operation['phase'];
  createdAt: string;
  updatedAt: string;
  epoch: string | null;
  requestId: Agent.RequestId | null;
  terminalHash?: string;
}
const ranks = { accepted: 0, dispatched: 1, outcome_unknown: 2, succeeded: 3, failed: 3 };
const instant = (value: string) => {
  const at = Date.parse(value);
  if (!Number.isFinite(at) || new Date(at).toISOString() !== value)
    throw new IvyError('native_evidence_mismatch', 'The original receipt has an invalid timestamp.');
  return at;
};

/** Check a receipt without copying its native params or reply into browser storage again. */
export async function checkNativeActionReceipt(value: unknown, original: NativeActionIdentity, caller: string,
  operationId: string, previous?: NativeActionCheckpoint): Promise<NativeActionCheckpoint> {
  validateAgent('Operation', value);
  const receipt = value as Agent.Operation;
  if (!sameNativeBackend(receipt.backend, original.backend) || receipt.serviceNodeId !== original.serviceNodeId ||
    receipt.callerPrincipalId !== caller || receipt.operationId !== operationId || receipt.nativeVersion !== original.nativeVersion ||
    receipt.nativeExecutableHash !== original.nativeExecutableHash || receipt.method !== original.method ||
    receipt.requestHash !== original.requestHash || await hashJson({ method: receipt.method, params: receipt.params }) !== original.requestHash ||
    instant(receipt.updatedAt) < instant(receipt.createdAt))
    throw new IvyError('native_evidence_mismatch', 'The receipt does not identify the original account, caller and request.');
  const terminalHash = ranks[receipt.phase] === 3 ? await hashJson(receipt) : undefined;
  if (previous && (receipt.createdAt !== previous.createdAt || instant(receipt.updatedAt) < instant(previous.updatedAt) ||
    ranks[receipt.phase] < ranks[previous.phase] || previous.epoch !== null && receipt.epoch !== previous.epoch ||
    previous.requestId !== null && receipt.requestId !== previous.requestId || previous.terminalHash !== undefined && terminalHash !== previous.terminalHash))
    throw new IvyError('native_evidence_regressed', 'The original receipt cannot regress or replace its terminal result.');
  return { phase: receipt.phase, createdAt: receipt.createdAt, updatedAt: receipt.updatedAt, epoch: receipt.epoch,
    requestId: receipt.requestId, ...(terminalHash ? { terminalHash } : {}) };
}
