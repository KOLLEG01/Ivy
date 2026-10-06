import { join } from 'node:path';
import { stat } from 'node:fs/promises';
import { atomicJson, jsonFile } from './config.js';
import { hashJson } from '../../contracts/src/canonical.js';
import type { Host } from '../../contracts/src/generated.js';

interface Progress {
  componentId: string; snapshotId: string; step: string; startedAt: string; stepStartedAt: string;
  observedAt: string; lastOutputAt: string | null; outputBytes: number; workerPid: number;
  command: string | null; filePath: string | null; fileBytes: number | null;
}
const location = (config: Host.HostConfig, componentId: string, snapshotId: string) =>
  join(config.runtimeRoot, 'preparation-progress', hashJson({ componentId, snapshotId }).slice(7) + '.json');

/** Bounded diagnostic state, separate from authoritative deployment transitions. No output contents. */
export class PreparationProgress {
  private value: Progress;
  private pending = Promise.resolve();
  private timer: ReturnType<typeof setInterval>;
  private path: string;
  constructor(config: Host.HostConfig, componentId: string, snapshotId: string) {
    const at = new Date().toISOString();
    this.path = location(config, componentId, snapshotId);
    this.value = { componentId, snapshotId, step: 'starting', startedAt: at, stepStartedAt: at,
      observedAt: at, lastOutputAt: null, outputBytes: 0, workerPid: process.pid,
      command: null, filePath: null, fileBytes: null };
    this.timer = setInterval(() => { void this.publish(); }, 5000); this.timer.unref();
  }
  async step(step: string, options: { command?: string; filePath?: string } = {}): Promise<void> {
    this.value = { ...this.value, step, stepStartedAt: new Date().toISOString(), lastOutputAt: null, outputBytes: 0,
      command: options.command ?? null, filePath: options.filePath ?? null, fileBytes: null };
    await this.publish();
  }
  output = (_stream: 'stdout' | 'stderr', text: string): void => {
    this.value.lastOutputAt = new Date().toISOString(); this.value.outputBytes += Buffer.byteLength(text);
  };
  private publish(): Promise<void> {
    const value = { ...this.value, observedAt: new Date().toISOString() };
    this.pending = this.pending.then(async () => {
      if (value.filePath) value.fileBytes = await stat(value.filePath).then(file => file.size, () => null);
      await atomicJson(this.path, value);
    }).catch(() => undefined); // Observability failure cannot change an activation outcome.
    return this.pending;
  }
  async close(): Promise<void> { clearInterval(this.timer); await this.publish(); }
}

export async function preparationProgress(config: Host.HostConfig, entry: Host.JournalEntry) {
  if (!entry.sourceSnapshot || entry.record.phase !== 'preparing') return null;
  const value = await jsonFile<Progress>(location(config, entry.record.componentId, entry.sourceSnapshot.snapshotId), 16384).catch(() => null);
  if (!value || value.componentId !== entry.record.componentId || value.snapshotId !== entry.sourceSnapshot.snapshotId ||
      !Number.isFinite(Date.parse(value.startedAt)) || !Number.isFinite(Date.parse(value.stepStartedAt)) || !Number.isFinite(Date.parse(value.observedAt)) ||
      Date.parse(value.startedAt) < Date.parse(entry.record.createdAt)) return null;
  return { ...value, elapsedMs: Math.max(0, Date.now() - Date.parse(value.startedAt)),
    stepElapsedMs: Math.max(0, Date.now() - Date.parse(value.stepStartedAt)),
    heartbeatAgeMs: Math.max(0, Date.now() - Date.parse(value.observedAt)) };
}
