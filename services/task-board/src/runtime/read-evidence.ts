import { canonical, digest, hashJson } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { validateNativeRead } from '../../../../packages/sdk/src/native-evidence.js';
import { validateTaskBoard } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { catalogFor } from './evidence.js';
import { evidenceMaximumBytes, saveEvidenceChunks, readEvidenceChunks } from './evidence-bytes.js';
import { mutation, TaskBoardStore } from './store.js';

export interface NativeReadIdentity {
  observationId: string; callerPrincipalId: string; serviceNodeId: string; nativeVersion: string; epoch: string; method: string; params: Wire.Json;
}
const chunkKey = 'task-board/native-read-evidence-chunk';
const headerKeys = ['observationId', 'callerPrincipalId', 'serviceNodeId', 'nativeVersion', 'nativeExecutableHash', 'catalogHash', 'method', 'requestHash', 'epoch'] as const;

/** Durable read observations are separate from accepted/dispatched mutation evidence. */
export class TaskBoardReadEvidence {
  constructor(readonly store: TaskBoardStore) {}
  verify(value: unknown, expected: NativeReadIdentity, catalogSourceHash?: string): asserts value is Agent.ReadObservation {
    const source = catalogFor(expected.nativeVersion, catalogSourceHash ? { sourceHash: catalogSourceHash } : undefined);
    validateNativeRead(value, { ...expected, nativeExecutableHash: source.catalog.nativeExecutableHash, catalogHash: source.catalogHash }, 'task_board_evidence_mismatch');
    requireThat(value.observationId === expected.observationId, 'task_board_evidence_mismatch', 'The read must retain its exact original observation ID.');
  }
  private async scope(parent: string | TaskBoard.ObjectPin, expected: NativeReadIdentity): Promise<string> {
    const run = await this.store.read('task-board/run', parent);
    const plan = run.value.plan;
    requireThat(run.value.target.serviceNodeId === expected.serviceNodeId && plan.nativeVersion === expected.nativeVersion,
      'task_board_evidence_mismatch', 'Read evidence must belong to its primary Run and original selected native owner/version.');
    return plan.catalogSourceHash;
  }
  async save(parent: string | TaskBoard.ObjectPin, observation: Agent.ReadObservation, expected: NativeReadIdentity): Promise<TaskBoard.Artifact> {
    const body = Buffer.from(canonical(observation, evidenceMaximumBytes));
    this.verify(observation, expected, await this.scope(parent, expected));
    const parentId = typeof parent === 'string' ? parent : parent.objectId, contentHash = digest(body);
    const chunks = await saveEvidenceChunks(this.store, parentId, body, chunkKey);
    const value: TaskBoard.NativeReadEvidence = { schemaVersion: 1, format: 'agent-read-observation/canonical-json',
      observationId: observation.observationId, callerPrincipalId: observation.callerPrincipalId, serviceNodeId: observation.serviceNodeId,
      nativeVersion: observation.nativeVersion, nativeExecutableHash: observation.nativeExecutableHash, catalogHash: observation.catalogHash,
      method: observation.method, requestHash: observation.requestHash, epoch: observation.epoch, contentHash, byteLength: body.length, chunks };
    validateTaskBoard('NativeReadEvidence', value);
    const name = 'Native read evidence ' + contentHash.slice(7);
    const pin = await this.store.write('task-board/native-read-evidence', value, mutation(parentId, 'read-evidence:' + contentHash + ':manifest'), { create: { parentId, name } });
    return { object: pin, label: name, contentHash: hashJson(value), mediaType: 'application/json' };
  }
  async read(parent: string | TaskBoard.ObjectPin, artifact: TaskBoard.Artifact, expected: NativeReadIdentity): Promise<Agent.ReadObservation> {
    validateTaskBoard('Artifact', artifact); const catalogSourceHash = await this.scope(parent, expected);
    const parentId = typeof parent === 'string' ? parent : parent.objectId;
    const manifest = await this.store.read('task-board/native-read-evidence', artifact.object, false), value = manifest.value;
    requireThat(manifest.metadata.parentId === parentId && artifact.mediaType === 'application/json' && manifest.revision.contentHash === artifact.contentHash,
      'task_board_evidence_mismatch', 'Read evidence must have its exact manifest scope, hash and media type.');
    const observation = await readEvidenceChunks(this.store, parentId, value, chunkKey); this.verify(observation, expected, catalogSourceHash);
    for (const key of headerKeys) requireThat(value[key] === observation[key], 'task_board_evidence_mismatch', 'Read evidence header contradicts its exact validated native body.');
    return observation;
  }
}
