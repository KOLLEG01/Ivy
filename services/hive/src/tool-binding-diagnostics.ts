import { hashJson } from '../../../packages/contracts/src/canonical.js';
import type { Wire } from '../../../packages/contracts/src/generated.js';

export interface ToolBindingTarget { callerPrincipalId: string; serviceNodeId: string; qualifiedName: string }
const source = 'hive.tool-bindings';
const identity = (resource: Wire.Json) => hashJson(resource);

/** Retain rejected bindings until this caller completes a call using the current contract. */
export class ToolBindingDiagnostics {
  private readonly active = new Map<string, Wire.Diagnostic>();
  constructor(private readonly record: (value: Wire.Diagnostic) => void, private readonly now = Date.now) {}
  restore(values: Wire.Diagnostic[]): void {
    for (const value of values)
      if (value.source === source && value.code === 'tool_definition_changed' && value.status !== 'resolved')
        this.active.set(identity(value.resource), value);
  }
  rejected(target: ToolBindingTarget): void {
    const resource = { ...target }, key = identity(resource), prior = this.active.get(key), at = new Date(this.now()).toISOString();
    if (prior && this.now() - Date.parse(prior.lastObservedAt) < 30_000) return;
    const value: Wire.Diagnostic = { code: 'tool_definition_changed', source, resource, severity: 'warning', status: 'current',
      firstObservedAt: prior?.firstObservedAt ?? at, lastObservedAt: at,
      message: 'A caller is using an outdated Tool definition. Rediscover the selected provider before resuming this operation.' };
    this.active.set(key, value); this.record(value);
  }
  completed(target: ToolBindingTarget): void {
    const key = identity({ ...target }), prior = this.active.get(key);
    if (!prior) return;
    this.active.delete(key);
    this.record({ ...prior, status: 'resolved', lastObservedAt: new Date(this.now()).toISOString(),
      message: 'The caller completed a call using the current Tool definition.' });
  }
}
