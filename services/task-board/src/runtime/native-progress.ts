import type { Wire } from '../../../../packages/sdk/src/node.js';

/** Observation only: silence never authorizes cancelling or repeating native work. */
export class NativeProgress {
  private readonly runs = new Map<string, { taskId: string; fingerprint: string; lastActivity: number }>();
  constructor(readonly source: string, readonly warningMs: number, readonly now = () => Date.now()) {}
  observe(runId: string, taskId: string, running: boolean, fingerprint: string): void {
    if (!running) { this.runs.delete(runId); return; }
    const previous = this.runs.get(runId);
    if (previous?.fingerprint === fingerprint) return;
    if (!previous && this.runs.size >= 1024) this.runs.delete(this.runs.keys().next().value!);
    this.runs.set(runId, { taskId, fingerprint, lastActivity: this.now() });
  }
  activity(runId: string): void {
    const value = this.runs.get(runId);
    if (value) value.lastActivity = this.now();
  }
  forget(runId: string): void { this.runs.delete(runId); }
  get diagnostics(): Wire.Diagnostic[] {
    const now = this.now();
    return [...this.runs.values()].filter(value => now - value.lastActivity >= this.warningMs).map(value => ({
      code: 'task_board_native_silent', resource: { objectId: value.taskId }, source: this.source,
      severity: 'warning', status: 'current', firstObservedAt: new Date(value.lastActivity + this.warningMs).toISOString(),
      lastObservedAt: new Date(now).toISOString(),
      message: 'The native owner still reports running, but no progress has been observed for ' +
        Math.floor((now - value.lastActivity) / 60_000) + ' minutes. Inspect the original native turn; its claim remains active.',
    }));
  }
}
