import { hashJson } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import type { Agent, Wire } from '../../contracts/src/generated.js';
import type { NativeContract } from '../../contracts/src/native-contract.js';
import { callBound, nativeServiceTools, serviceTools } from './client.js';
import type { RpcClient, RequestOptions } from './client.js';
import { readNativeContract } from './native.js';
import { nativeThreadState } from './native-observations.js';
import { validateNativeOperation, validateNativeRead, validateNativeStatus } from './native-evidence.js';
import type { NativeOwnerIdentity } from './native-evidence.js';
import { nativeThreadProject } from './native-project.js';

export interface NativeOperationCall { operationId: string; method: string; params: Wire.Json; definitionHash: string }
const contracts = new WeakMap<RpcClient, Map<string, Promise<NativeContract>>>();

/** Exact native owner transport. Domain claims and permission to act remain with the caller. */
export class NativeOwner {
  readonly target: NativeOwnerIdentity;
  constructor(readonly client: RpcClient, readonly caller: string, target: NativeOwnerIdentity, readonly options: RequestOptions = {}) {
    this.target = Object.freeze(structuredClone(target));
  }
  private checkSignal(): void { this.options.signal?.throwIfAborted(); }
  async selectedContract(): Promise<NativeContract> {
    this.checkSignal(); const key = hashJson(this.target);
    let cache = contracts.get(this.client); if (!cache) { cache = new Map(); contracts.set(this.client, cache); }
    let pending = cache.get(key);
    if (!pending) {
      pending = (async () => {
        const hostId = this.target.hostId ?? (await this.status()).hostId;
        return (await readNativeContract(this.client, { ...this.target, hostId })).contract;
      })();
      if (cache.size >= 16) cache.delete(cache.keys().next().value!);
      cache.set(key, pending); const selected = cache;
      void pending.catch(() => { if (selected.get(key) === pending) selected.delete(key); });
    }
    const contract = await pending; this.checkSignal(); return contract;
  }
  async binding(method: string, expectedDefinitionHash?: string) {
    this.checkSignal(); const binding = await nativeServiceTools(this.client, this.target.serviceNodeId).binding('codex.' + method, expectedDefinitionHash);
    this.checkSignal(); return binding;
  }
  management(name: string, args: Wire.Json, operationId?: string): Promise<Wire.Json> {
    return this.callManagement(name, args, operationId);
  }
  private async callManagement(name: string, args: Wire.Json, operationId?: string, beforeSend: () => Promise<void> = async () => {}, reconcileUnknown = false, reassess: () => Promise<void> = async () => {}): Promise<Wire.Json> {
    this.checkSignal(); const original = structuredClone(args);
    const tools = serviceTools(this.client, this.target.serviceNodeId, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]);
    let binding = await tools.binding('agent.' + name);
    for (let attempt = 0; ; attempt++) {
      try { await beforeSend(); this.checkSignal(); }
      catch (error) {
        if (!attempt) throw error;
        const e = IvyError.from(error);
        throw new IvyError(e.code, e.message, 'not_executed', e.details);
      }
      try {
        return await callBound(this.client, binding, original, operationId, { ...this.options, expectedCallerPrincipalId: this.caller });
      } catch (error) {
        if (reconcileUnknown && error instanceof IvyError && error.outcome !== 'not_executed') return null;
        if (attempt || !(error instanceof IvyError) || error.code !== 'tool_definition_changed' || error.outcome !== 'not_executed') throw error;
        // Reassess only the stable Agent envelope on the same provider/interface.
        // Native catalog/hash, caller, params and operation ID stay pinned; guards run again.
        try {
          const fresh = await tools.refresh(binding);
          if (fresh.definitionHash === binding.definitionHash) throw error;
          await reassess();
          binding = fresh;
        } catch (refreshError) {
          // Discovery/read failures cannot erase the original dispatch rejection.
          const e = IvyError.from(refreshError);
          throw new IvyError(e.code, e.message, 'not_executed', e.details);
        }
      }
    }
  }
  async status(): Promise<Agent.Status> {
    const value = await this.management('status', {}); validateNativeStatus(value, this.target); return value;
  }
  async frameLimits(): Promise<Agent.FrameLimits> {
    const value = await this.management('frameLimits', {}); validateAgent('FrameLimits', value); const limits = value as Agent.FrameLimits;
    requireThat(limits.serviceNodeId === this.target.serviceNodeId && limits.nativeVersion === this.target.nativeVersion &&
      limits.nativeExecutableHash === this.target.nativeExecutableHash && limits.catalogHash === this.target.catalogHash && limits.epoch,
      'native_owner_mismatch', 'Frame limits must come from the exact selected native owner.'); return limits;
  }
  /** Resolve membership before the caller retains its immutable start request. */
  threadStartParams(params: Record<string, Wire.Json>): Promise<Record<string, Wire.Json>> {
    this.checkSignal();
    return nativeThreadProject(this.client, this.target.serviceNodeId, params, this.options);
  }
  checkOperation(value: unknown, call: NativeOperationCall): void {
    validateNativeOperation(value, { ...this.target, callerPrincipalId: this.caller, ...call });
  }
  async operation(call: NativeOperationCall): Promise<Agent.Operation | Agent.OperationAbsence> {
    const original = structuredClone(call);
    try {
      const value = await this.management('operation', { operationId: original.operationId }); this.checkOperation(value, original); return value as Agent.Operation;
    } catch (error) {
      if (!(error instanceof IvyError && error.code === 'not_found')) throw error;
      validateAgent('OperationAbsence', error.details); const absence = error.details as Agent.OperationAbsence;
      requireThat(absence.serviceNodeId === this.target.serviceNodeId && absence.operationId === original.operationId,
        'native_absence_mismatch', 'Only exact owner-produced absence can identify an unobserved original operation.'); return absence;
    }
  }
  async dispatch(call: NativeOperationCall, guard: () => Promise<void>): Promise<Agent.Operation | undefined> {
    const original = structuredClone(call);
    const binding = await this.binding(original.method, original.definitionHash);
    // Only uncertain dispatches need a separate journal lookup.
    const result = await this.callManagement('invoke', { operationId: original.operationId, nativeVersion: this.target.nativeVersion,
      method: original.method, params: original.params, expectedDefinitionHash: binding.definitionHash }, original.operationId,
      guard, true, () => this.reassessNative(original)) as Agent.Operation | null;
    return result ?? undefined;
  }
  checkRead(value: unknown, method: string, params: Wire.Json, epoch: string, allowError = false): void {
    validateNativeRead(value, { ...this.target, callerPrincipalId: this.caller, epoch, method, params });
    requireThat(allowError || 'result' in value.reply, 'native_read_failed', 'The native read returned its original error.');
  }
  private async reassessNative(call: NativeOperationCall): Promise<void> {
    await this.status();
    const tools = nativeServiceTools(this.client, this.target.serviceNodeId);
    const fresh = await tools.refresh(await tools.binding('codex.' + call.method));
    requireThat(fresh.definitionHash === call.definitionHash, 'tool_definition_changed',
      'Envelope recovery cannot change the original native method definition.');
  }
  /** Interactive calls never pass through the durable Hive operation store. */
  async interact(call: NativeOperationCall, beforeSend: () => Promise<void> = async () => {}): Promise<Agent.Interaction> {
    const original = structuredClone(call), binding = await this.binding(original.method, original.definitionHash);
    const value = await this.callManagement('interact', { operationId: original.operationId, nativeVersion: this.target.nativeVersion,
      method: original.method, params: original.params, expectedDefinitionHash: binding.definitionHash }, original.operationId, beforeSend,
      false, () => this.reassessNative(original));
    validateAgent('Interaction', value);
    requireThat((value as Agent.Interaction).operationId === original.operationId, 'native_observation_invalid', 'Interaction reply identity differs.');
    return value as Agent.Interaction;
  }
  async interaction(operationId: string): Promise<Agent.Interaction> {
    const value = await this.management('interaction', { operationId }); validateAgent('Interaction', value);
    requireThat((value as Agent.Interaction).operationId === operationId, 'native_observation_invalid', 'Interaction lookup identity differs.');
    return value as Agent.Interaction;
  }
  async read(method: Agent.ReadObservation['method'], params: Wire.Json, epoch?: string, allowError = false): Promise<Agent.ReadObservation> {
    const original = structuredClone(params), value = await this.management('read', { nativeVersion: this.target.nativeVersion, method, params: original });
    this.checkRead(value, method, original, epoch ?? (value as Agent.ReadObservation).epoch, allowError); return value as Agent.ReadObservation;
  }
  async threadState(observation: Agent.ReadObservation, epoch: string): Promise<ReturnType<typeof nativeThreadState>> {
    this.checkRead(observation, 'thread/read', observation.params, epoch);
    const contract = await this.selectedContract();
    requireThat('result' in observation.reply, 'native_read_failed', 'Thread state needs a successful original read.');
    return nativeThreadState(contract, observation.reply.result);
  }
}
