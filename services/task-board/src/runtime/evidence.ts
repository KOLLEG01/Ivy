import { canonical, digest, hashJson } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { validateTaskBoard } from '../../../../packages/sdk/src/node.js';
import { checkedNativeContract } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { validateNativeOperation } from '../../../../packages/sdk/src/native-evidence.js';
import { mutation, TaskBoardStore } from './store.js';
import type { Document } from './store.js';
import { evidenceChunkBytes, evidenceMaximumBytes, saveEvidenceChunks, readEvidenceChunks } from './evidence-bytes.js';
import { resolveNativeRequest } from './native-intent.js';
export { evidenceChunkBytes, evidenceMaximumBytes } from './evidence-bytes.js';

const chunkKey = 'task-board/native-evidence-chunk';
export interface NativeEvidenceIdentity {
  operationId: string; callerPrincipalId: string; serviceNodeId: string; nativeVersion: string; method: string; params: Wire.Json;
}
export const catalogFor = checkedNativeContract;

/** Immutable, bounded evidence storage. It does not dispatch native calls or infer run completion. */
export class TaskBoardEvidence {
  constructor(readonly store: TaskBoardStore, readonly retainAuditSnapshot=false) {}
  private verify(value: unknown, expected: NativeEvidenceIdentity, source: ReturnType<typeof catalogFor>): asserts value is Agent.Operation {
    const definition = source.definitions.get(expected.method);
    requireThat(definition, 'task_board_native_method_unsupported', 'The evidence method is not in the exact checked native client catalog.');
    validateNativeOperation(value, { ...expected, nativeExecutableHash: source.catalog.nativeExecutableHash, catalogHash: source.catalogHash }, 'task_board_evidence_mismatch');
    // Error replies are already validated as Agent.NativeError. They never become a successful
    // method result merely because an evidence object can be saved.
  }
  private async scope(ref: string | TaskBoard.ObjectPin, expected: NativeEvidenceIdentity): Promise<ReturnType<typeof catalogFor>> {
    let call: Document<'task-board/native-call'> | null = null;
    try { call = await this.store.read('task-board/native-call', ref); } catch { call = null; }
    if (call) {
      const source = catalogFor(expected.nativeVersion, { sourceHash: call.value.request.catalogSourceHash });
      requireThat(call.value.operationId === expected.operationId && call.value.request.nativeVersion === expected.nativeVersion && call.value.request.catalogSourceHash === source.catalog.sourceHash && call.value.request.method === expected.method &&
        hashJson((await resolveNativeRequest(this.store, call.value.request)).params) === hashJson(expected.params) && call.value.expectedDefinitionHash === hashJson(source.definitions.get(expected.method)),
        'task_board_evidence_mismatch', 'NativeCall evidence must retain its saved exact request and checked definition.');
      return source;
    } else {
      const run = await this.store.read('task-board/run', ref);
      requireThat(run.value.target.serviceNodeId === expected.serviceNodeId, 'task_board_evidence_mismatch', 'Run evidence must come from its original selected native owner.');
      requireThat(run.value.plan.nativeVersion === expected.nativeVersion, 'task_board_evidence_mismatch', 'Run evidence must retain its planned native version.');
      return catalogFor(expected.nativeVersion, { sourceHash: run.value.plan.catalogSourceHash });
    }
  }
  async save(parent: string | TaskBoard.ObjectPin, observation: Agent.Operation, expected: NativeEvidenceIdentity): Promise<TaskBoard.Artifact> {
    const source = await this.scope(parent, expected); this.verify(observation, expected, source);
    const parentId = typeof parent === 'string' ? parent : parent.objectId;
    const body = Buffer.from(canonical(observation, evidenceMaximumBytes));
    const contentHash = digest(body), chunks = await saveEvidenceChunks(this.store, parentId, body, chunkKey);
    const value: TaskBoard.NativeEvidence = { schemaVersion: 1, format: 'agent-operation/canonical-json',
      operationId: observation.operationId, callerPrincipalId: observation.callerPrincipalId, serviceNodeId: observation.serviceNodeId,
      nativeVersion: observation.nativeVersion as TaskBoard.NativeEvidence['nativeVersion'], nativeExecutableHash: observation.nativeExecutableHash,
      method: observation.method, requestHash: observation.requestHash, phase: observation.phase, epoch: observation.epoch, contentHash, byteLength: body.length, chunks };
    validateTaskBoard('NativeEvidence', value);
    const name = 'Native evidence ' + contentHash.slice(7), pin = await this.store.write('task-board/native-evidence', value, mutation(parentId, 'evidence:' + contentHash + ':manifest'), { create: { parentId, name } });
    return { object: pin, label: name, contentHash: hashJson(value), mediaType: 'application/json' };
  }
  async read(parent: string | TaskBoard.ObjectPin, artifact: TaskBoard.Artifact, expected: NativeEvidenceIdentity): Promise<Agent.Operation> {
    validateTaskBoard('Artifact', artifact); const source = await this.scope(parent, expected);
    const parentId = typeof parent === 'string' ? parent : parent.objectId;
    const manifest = await this.store.read('task-board/native-evidence', artifact.object, false), value = manifest.value;
    requireThat(manifest.metadata.parentId === parentId && artifact.mediaType === 'application/json' && manifest.revision.contentHash === artifact.contentHash && value.chunks.length === Math.ceil(value.byteLength / evidenceChunkBytes),
      'task_board_evidence_mismatch', 'Evidence must have the exact manifest scope, hash, media type and bounded ordered chunk count.');
    const observation = await readEvidenceChunks(this.store, parentId, value, chunkKey);
    this.verify(observation, expected, source);
    for (const key of ['operationId', 'callerPrincipalId', 'serviceNodeId', 'nativeVersion', 'nativeExecutableHash', 'method', 'requestHash', 'phase', 'epoch'] as const) {
      requireThat(value[key] === observation[key], 'task_board_evidence_mismatch', 'The evidence header cannot contradict its validated native operation body.');
    }
    return observation;
  }
}
